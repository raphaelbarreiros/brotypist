//! One connection owner; no AT-SPI calls occur on the event path.
use super::{InputState, ObservationPolicy};
use crate::{
    config::Config,
    platform::{Action, ShortcutHandler},
    shortcuts::parse_shortcut,
};
use anyhow::{Result, anyhow, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xproto::{Allow, ConnectionExt, GrabMode, ModMask},
    },
    rust_connection::RustConnection,
};
const SHIFT: u16 = 1;
const LOCK: u16 = 2;
const CTRL: u16 = 4;
const ALT: u16 = 8;
const SUPER: u16 = 64;
const KNOWN: u16 = SHIFT | CTRL | ALT | SUPER;
#[derive(Clone, Debug, Default, serde::Serialize)]
struct Status {
    enabled: bool,
    conflict: bool,
    stopped: bool,
    message: String,
}
enum Command {
    Configure(Config),
    Enable(bool),
    Identity(String),
    Finish(bool, Sender<bool>),
    Stop,
}
pub(super) struct Hook {
    commands: Sender<Command>,
    done: Receiver<()>,
    thread: Option<JoinHandle<()>>,
    status: Arc<Mutex<Status>>,
}
impl Hook {
    pub fn start(
        config: Config,
        callback: ShortcutHandler,
        input: Arc<InputState>,
    ) -> Result<Self> {
        let (connection, screen) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen].root;
        let (tx, rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let state = status.clone();
        let thread = thread::Builder::new()
            .name("brotypist-x11".into())
            .spawn(move || {
                let result = run(&connection, root, config, callback, input, rx, &state);
                // Replay any frozen event before disconnect; disconnect itself drops all passive grabs.
                let _ = connection.allow_events(Allow::REPLAY_KEYBOARD, x11rb::CURRENT_TIME);
                let _ = connection.ungrab_keyboard(x11rb::CURRENT_TIME);
                let _ = connection.flush();
                drop(connection);
                {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.enabled = false;
                    s.stopped = true;
                    if let Err(error) = result {
                        s.message = error.to_string();
                    }
                }
                let _ = done_tx.send(());
            })?;
        Ok(Self {
            commands: tx,
            done,
            thread: Some(thread),
            status,
        })
    }
    pub fn configure(&self, config: Config) -> Result<()> {
        self.commands
            .send(Command::Configure(config))
            .map_err(|_| anyhow!("X11 hook owner stopped"))
    }
    pub fn enable(&self, enabled: bool) {
        let _ = self.commands.send(Command::Enable(enabled));
    }
    pub fn identity(&self, app: String) {
        let _ = self.commands.send(Command::Identity(app));
    }
    pub fn finish(&self, accepted: bool) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.commands
            .send(Command::Finish(accepted, tx))
            .map_err(|_| anyhow!("X11 hook owner stopped"))?;
        if !rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            anyhow!("X11 input resolution was not acknowledged; do not retry the edit")
        })? {
            bail!("X11 input transaction expired; no edit retry is safe");
        }
        Ok(())
    }
    pub fn status(&self) -> serde_json::Value {
        serde_json::to_value(&*self.status.lock().unwrap_or_else(|e| e.into_inner()))
            .unwrap_or(serde_json::Value::Null)
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.thread.is_none() {
            return Ok(());
        }
        let _ = self.commands.send(Command::Stop);
        match self.done.recv_timeout(Duration::from_secs(1)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                bail!("X11 keyboard cleanup incomplete; owner retained, try Quit again")
            }
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("X11 hook owner panicked during cleanup"))?;
        }
        Ok(())
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}
pub(super) fn check_connection() -> Result<()> {
    let (connection, _) = x11rb::connect(None)?;
    connection.flush()?;
    Ok(())
}
fn modifier_mask(modifiers: &[String]) -> u16 {
    modifiers.iter().fold(0, |mask, name| {
        mask | match name.as_str() {
            "Ctrl" => CTRL,
            "Alt" => ALT,
            "Shift" => SHIFT,
            "Super" => SUPER,
            _ => 0,
        }
    })
}
fn keysym(key: &str) -> Option<u32> {
    Some(match key {
        "Tab" => 0xff09,
        "Return" => 0xff0d,
        "Escape" => 0xff1b,
        "Space" => 0x20,
        "Backspace" => 0xff08,
        "Delete" => 0xffff,
        "Left" => 0xff51,
        "Up" => 0xff52,
        "Right" => 0xff53,
        "Down" => 0xff54,
        "Home" => 0xff50,
        "End" => 0xff57,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "Insert" => 0xff63,
        value if value.starts_with('F') => {
            0xffbd
                + value[1..]
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=24).contains(n))?
        }
        value if value.chars().count() == 1 => {
            let c = value.chars().next()? as u32;
            if c <= 0xff { c } else { 0x01000000 | c }
        }
        _ => return None,
    })
}
fn key_code(key: &str, min: u8, per: u8, syms: &[u32]) -> Option<u8> {
    if key == "AboveTab" {
        return Some(49);
    }
    let symbol = keysym(key)?;
    syms.chunks(per as usize)
        .position(|row| row.contains(&symbol))
        .and_then(|index| u8::try_from(index + min as usize).ok())
}
fn lock_variants(num: u16) -> BTreeSet<u16> {
    BTreeSet::from([0, LOCK, num, LOCK | num])
}
#[derive(Default)]
struct Plan {
    grabs: BTreeSet<(u8, u16)>,
    names: BTreeMap<u8, String>,
}
fn plan(config: &Config, min: u8, per: u8, syms: &[u32], num: u16) -> Result<Plan> {
    let mut plan = Plan::default();
    let locks = lock_variants(num);
    for value in config
        .shortcuts
        .values()
        .map(String::as_str)
        .chain(["Escape", "1", "2", "3", "4", "5", "6", "7", "8", "9"])
    {
        if value.is_empty() {
            continue;
        }
        let chord = parse_shortcut(value)?;
        if chord.key == "Tab" && chord.modifiers.iter().any(|m| m == "Alt") {
            continue;
        }
        let Some(code) = key_code(&chord.key, min, per, syms) else {
            continue;
        };
        plan.names.insert(code, chord.key);
        let modifiers = modifier_mask(&chord.modifiers);
        for lock in &locks {
            plan.grabs.insert((code, modifiers | lock));
        }
    }
    Ok(plan)
}
fn action(
    policy: &ObservationPolicy,
    key: &str,
    state: u16,
    num: u16,
    app: &str,
) -> Option<Action> {
    let state = state & !(LOCK | num);
    if state & !KNOWN != 0 {
        return None;
    }
    // Canonical Chord order is Ctrl, Alt, Shift, Super.
    let mut modifiers = [""; 4];
    let mut count = 0;
    for (name, mask) in [
        ("Ctrl", CTRL),
        ("Alt", ALT),
        ("Shift", SHIFT),
        ("Super", SUPER),
    ] {
        if state & mask != 0 {
            modifiers[count] = name;
            count += 1;
        }
    }
    policy.key_action(key, &modifiers[..count], "linux", app, "")
}
#[derive(Default)]
struct Pending {
    time: Option<u32>,
    since: Option<Instant>,
}
impl Pending {
    fn set(&mut self, time: u32) {
        self.time = Some(time);
        self.since = Some(Instant::now());
    }
    fn dispatch(&mut self, time: u32, callback: impl FnOnce() -> bool) -> bool {
        self.set(time);
        // The synchronous passive grab owns this key until resolution. A
        // runtime input note here would revoke its cached shortcut claim.
        callback()
    }
    fn resolve(&mut self, accepted: bool, input: &InputState) -> Option<(Allow, u32)> {
        let time = self.time.take()?;
        self.since = None;
        if !accepted {
            // All rejection paths (including reconfiguration, timeout and
            // shutdown) invalidate once before replaying the original event.
            input.note(true);
        }
        Some((
            if accepted {
                Allow::ASYNC_KEYBOARD
            } else {
                Allow::REPLAY_KEYBOARD
            },
            time,
        ))
    }
}
fn thaw(
    connection: &RustConnection,
    pending: &mut Pending,
    accepted: bool,
    input: &InputState,
) -> Result<()> {
    if let Some((mode, time)) = pending.resolve(accepted, input) {
        connection.allow_events(mode, time)?;
        if accepted {
            connection.ungrab_keyboard(time)?;
        }
        connection.flush()?;
    }
    Ok(())
}
fn remove_grabs(
    connection: &RustConnection,
    root: u32,
    grabs: &mut BTreeSet<(u8, u16)>,
) -> Result<()> {
    for (code, mask) in std::mem::take(grabs) {
        connection.ungrab_key(code, root, ModMask::from(mask))?;
    }
    connection.ungrab_keyboard(x11rb::CURRENT_TIME)?;
    connection.flush()?;
    Ok(())
}
fn install_grabs(
    grabs: &BTreeSet<(u8, u16)>,
    mut grab: impl FnMut(u8, u16) -> Result<bool>,
    mut ungrab: impl FnMut(u8, u16) -> Result<()>,
) -> Result<bool> {
    let mut conflict = false;
    for &(code, mask) in grabs {
        match grab(code, mask) {
            Ok(true) => {}
            Ok(false) => conflict = true,
            Err(error) => {
                for &(code, mask) in grabs {
                    let _ = ungrab(code, mask);
                }
                return Err(error);
            }
        }
    }
    if conflict {
        for &(code, mask) in grabs {
            ungrab(code, mask)?;
        }
    }
    Ok(!conflict)
}

fn run(
    connection: &RustConnection,
    root: u32,
    mut config: Config,
    callback: ShortcutHandler,
    input: Arc<InputState>,
    rx: Receiver<Command>,
    status: &Mutex<Status>,
) -> Result<()> {
    let min = connection.setup().min_keycode;
    let max = connection.setup().max_keycode;
    let mapping = connection
        .get_keyboard_mapping(min, max - min + 1)?
        .reply()?;
    if mapping.keysyms_per_keycode == 0 {
        bail!("X11 keyboard mapping unavailable");
    }
    let num_code = key_code_for_symbol(0xff7f, min, mapping.keysyms_per_keycode, &mapping.keysyms);
    let modifiers = connection.get_modifier_mapping()?.reply()?;
    let per = modifiers.keycodes.len() / 8;
    let mut num = 0u16;
    if let Some(code) = num_code {
        if per > 0 {
            for (index, keys) in modifiers.keycodes.chunks(per).enumerate() {
                if keys.contains(&code) {
                    num |= 1 << index;
                }
            }
        }
    }
    let mut policy = ObservationPolicy::new(&config)?;
    let mut enabled = false;
    let mut dirty = true;
    let mut grabs = BTreeSet::new();
    let mut names = BTreeMap::new();
    let mut app = String::new();
    let mut pending = Pending::default();
    let result: Result<()> = (|| {
        loop {
            let command = match rx.recv_timeout(Duration::from_millis(8)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if let Some(command) = command {
                match command {
                    Command::Stop => break,
                    Command::Configure(next) => {
                        policy = ObservationPolicy::new(&next)?;
                        config = next;
                        dirty = true;
                    }
                    Command::Enable(next) => {
                        enabled = next;
                        dirty = true;
                    }
                    Command::Identity(next) => app = next,
                    Command::Finish(accepted, reply) => {
                        let active = pending.time.is_some();
                        thaw(connection, &mut pending, accepted, &input)?;
                        let _ = reply.send(active || !accepted);
                    }
                }
            }
            if dirty {
                thaw(connection, &mut pending, false, &input)?;
                remove_grabs(connection, root, &mut grabs)?;
                names.clear();
                if enabled {
                    let candidate = plan(
                        &config,
                        min,
                        mapping.keysyms_per_keycode,
                        &mapping.keysyms,
                        num,
                    )?;
                    // Install as a transaction: a single conflict releases the complete candidate set.
                    let installed = install_grabs(
                        &candidate.grabs,
                        |code, mask| {
                            Ok(connection
                                .grab_key(
                                    false,
                                    root,
                                    ModMask::from(mask),
                                    code,
                                    GrabMode::ASYNC,
                                    GrabMode::SYNC,
                                )?
                                .check()
                                .is_ok())
                        },
                        |code, mask| {
                            connection.ungrab_key(code, root, ModMask::from(mask))?;
                            Ok(())
                        },
                    )?;
                    if !installed {
                        connection.ungrab_keyboard(x11rb::CURRENT_TIME)?;
                        enabled = false;
                        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
                        s.conflict = true;
                        s.message="An X11 shortcut is already owned; all interception disabled. Release conflicts and pause/resume.".into();
                    } else {
                        grabs = candidate.grabs;
                        names = candidate.names;
                        status.lock().unwrap_or_else(|e| e.into_inner()).conflict = false;
                    }
                }
                status.lock().unwrap_or_else(|e| e.into_inner()).enabled = enabled;
                connection.flush()?;
                dirty = false;
            }
            // A lost controller must not leave the desktop frozen. Timeout always replays, never consumes.
            if pending
                .since
                .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
            {
                thaw(connection, &mut pending, false, &input)?;
                enabled = false;
                dirty = true;
                status.lock().unwrap_or_else(|e| e.into_inner()).message =
                "Controller did not resolve input; original key replayed and interception disabled"
                    .into();
            }
            while let Some(event) = connection.poll_for_event()? {
                match event {
                    Event::KeyPress(event) => {
                        let selected = if enabled {
                            names.get(&event.detail).and_then(|key| {
                                action(&policy, key, u16::from(event.state), num, &app)
                            })
                        } else {
                            None
                        };
                        let queued = pending.dispatch(event.time, || {
                            selected.is_some_and(|action| {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    callback(action)
                                }))
                                .unwrap_or_else(|_| {
                                    enabled = false;
                                    dirty = true;
                                    false
                                })
                            })
                        });
                        if !queued {
                            thaw(connection, &mut pending, false, &input)?;
                        }
                    }
                    Event::MappingNotify(_) => {
                        bail!(
                            "X11 keyboard mapping changed; restart shortcut owner before re-arming"
                        );
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    // Error exits must use the same invalidation/replay resolution as Stop;
    // the outer connection cleanup cannot notify the consumer on its own.
    let resolved = thaw(connection, &mut pending, false, &input);
    let removed = remove_grabs(connection, root, &mut grabs);
    result?;
    resolved?;
    removed?;
    Ok(())
}
fn key_code_for_symbol(symbol: u32, min: u8, per: u8, syms: &[u32]) -> Option<u8> {
    syms.chunks(per as usize)
        .position(|row| row.contains(&symbol))
        .and_then(|i| u8::try_from(i + min as usize).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passive_plan_deduplicates_lock_combinations_and_refuses_alt_tab() {
        let mut config = Config::default();
        config.shortcuts.clear();
        config.shortcuts.insert("all".into(), "Alt+Tab".into());
        config.shortcuts.insert("word".into(), "Ctrl+Tab".into());
        let plan = plan(&config, 8, 1, &[0xff09, 0xff1b, b'1' as u32], 16).unwrap();
        for lock in [0, LOCK, 16, LOCK | 16] {
            assert!(plan.grabs.contains(&(8, CTRL | lock)));
            assert!(!plan.grabs.contains(&(8, ALT | lock)));
        }
        assert_eq!(lock_variants(0), BTreeSet::from([0, LOCK]));
    }
    #[test]
    fn only_known_modifiers_dispatch_and_declined_keys_replay_original_time() {
        let policy = ObservationPolicy::new(&Config::default()).unwrap();
        assert_eq!(
            action(&policy, "Escape", LOCK | 16, 16, "editor"),
            Some(Action::Dismiss)
        );
        assert_eq!(action(&policy, "Escape", 32, 16, "editor"), None);
        let mut pending = Pending::default();
        let input = InputState::default();
        pending.set(77);
        assert_eq!(
            pending.resolve(false, &input),
            Some((Allow::REPLAY_KEYBOARD, 77))
        );
        assert_eq!(pending.resolve(true, &input), None);
        pending.set(78);
        assert_eq!(
            pending.resolve(true, &input),
            Some((Allow::ASYNC_KEYBOARD, 78))
        );
    }
    #[test]
    fn disconnected_owner_stop_preserves_no_join_handle() {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let thread = thread::spawn(move || {
            assert!(matches!(rx.recv().unwrap(), Command::Stop));
            done_tx.send(()).unwrap();
        });
        let mut hook = Hook {
            commands: tx,
            done,
            thread: Some(thread),
            status: Arc::new(Mutex::new(Status::default())),
        };
        hook.stop().unwrap();
        assert!(hook.thread.is_none());
        hook.stop().unwrap();
    }
    #[test]
    fn any_passive_grab_conflict_rolls_back_all_grabs_not_only_the_failed_one() {
        use std::cell::RefCell;
        let requested = BTreeSet::from([(8, 0), (8, LOCK), (9, CTRL)]);
        let owned = RefCell::new(BTreeSet::new());
        let installed = install_grabs(
            &requested,
            |code, mask| {
                if (code, mask) == (8, LOCK) {
                    Ok(false)
                } else {
                    owned.borrow_mut().insert((code, mask));
                    Ok(true)
                }
            },
            |code, mask| {
                owned.borrow_mut().remove(&(code, mask));
                Ok(())
            },
        )
        .unwrap();
        assert!(!installed);
        assert!(owned.borrow().is_empty());
    }
    #[test]
    fn failed_grab_request_also_releases_previously_installed_keys() {
        use std::cell::RefCell;
        let requested = BTreeSet::from([(8, 0), (9, 0)]);
        let owned = RefCell::new(BTreeSet::new());
        assert!(
            install_grabs(
                &requested,
                |code, mask| {
                    if code == 9 {
                        bail!("Disconnected request");
                    }
                    owned.borrow_mut().insert((code, mask));
                    Ok(true)
                },
                |code, mask| {
                    owned.borrow_mut().remove(&(code, mask));
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(owned.borrow().is_empty());
    }
    fn input_with_notifications() -> (InputState, Receiver<()>) {
        let input = InputState::default();
        let (tx, rx) = mpsc::channel();
        *input.callback.lock().unwrap() = Some(Arc::new(move || {
            tx.send(()).unwrap();
        }));
        (input, rx)
    }
    #[test]
    fn held_acceptance_and_explicit_actions_keep_the_consumers_origin_current() {
        use std::sync::atomic::Ordering;
        for selected in [
            Action::Word,
            Action::All,
            Action::Choice(1),
            Action::Force,
            Action::Alternatives,
            Action::Synonyms,
        ] {
            let (input, notes) = input_with_notifications();
            let mut pending = Pending::default();
            let (tx, rx) = mpsc::channel();
            assert!(pending.dispatch(91, || {
                tx.send((
                    selected.clone(),
                    input.revision.load(Ordering::Acquire),
                    input.user_revision.load(Ordering::Acquire),
                ))
                .unwrap();
                true
            }));
            assert_eq!(rx.try_recv().unwrap(), (selected, 0, 0));
            assert_eq!(pending.time, Some(91));
            assert_eq!(
                pending.resolve(true, &input),
                Some((Allow::ASYNC_KEYBOARD, 91))
            );
            // X11 owns release delivery after thaw/ungrab. Resolving again
            // must neither replay nor count a second physical input.
            assert_eq!(pending.resolve(false, &input), None);
            assert!(notes.try_recv().is_err());
            assert_eq!(input.revision.load(Ordering::Acquire), 0);
            assert_eq!(input.user_revision.load(Ordering::Acquire), 0);
            assert!(input.time.lock().unwrap().is_none());
        }
    }
    #[test]
    fn forwarded_typing_and_modifier_refusals_replay_and_wake_consumer_once() {
        use std::sync::atomic::Ordering;
        let policy = ObservationPolicy::new(&Config::default()).unwrap();
        for (key, mask) in [("a", 0), ("Tab", ALT), ("Escape", 32)] {
            let (input, notes) = input_with_notifications();
            let mut pending = Pending::default();
            let (tx, rx) = mpsc::channel();
            let selected = action(&policy, key, mask, 16, "editor");
            let queued = pending.dispatch(92, || {
                selected.is_some_and(|selected| {
                    tx.send(selected).unwrap();
                    true
                })
            });
            assert!(!queued);
            assert!(rx.try_recv().is_err());
            assert_eq!(
                pending.resolve(false, &input),
                Some((Allow::REPLAY_KEYBOARD, 92))
            );
            assert_eq!(input.revision.load(Ordering::Acquire), 1);
            assert_eq!(input.user_revision.load(Ordering::Acquire), 1);
            assert!(input.time.lock().unwrap().is_some());
            assert_eq!(notes.try_iter().count(), 1);
            assert_eq!(pending.resolve(false, &input), None);
            assert_eq!(notes.try_iter().count(), 0);
        }
    }
    #[test]
    fn queued_cancellation_and_failed_enqueue_invalidate_only_at_native_replay() {
        use std::sync::atomic::Ordering;
        let (input, notes) = input_with_notifications();
        let mut pending = Pending::default();
        // Both a refused enqueue and an accepted enqueue later cancelled by
        // Finish(false), dirty config, timeout or Stop share resolve(false).
        for (index, queued) in [false, true].into_iter().enumerate() {
            let time = 100 + index as u32;
            assert_eq!(pending.dispatch(time, || queued), queued);
            assert_eq!(input.user_revision.load(Ordering::Acquire), index as u64);
            assert_eq!(
                pending.resolve(false, &input),
                Some((Allow::REPLAY_KEYBOARD, time))
            );
            assert_eq!(pending.resolve(false, &input), None);
            assert_eq!(pending.resolve(true, &input), None);
            assert_eq!(
                input.user_revision.load(Ordering::Acquire),
                index as u64 + 1
            );
            assert_eq!(notes.try_iter().count(), 1);
        }
    }
}
