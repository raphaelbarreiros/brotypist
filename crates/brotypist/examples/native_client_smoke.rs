//! Owned Linux client acceptance (synthetic input, real patched plugins and broker).
//! NOT RUN: build/run prerequisites are intentionally mandatory, never skips.
//! 1. Privately stage reviewed fcitx5-gtk 5.1.7 + static patched GClient using
//!    native/linux-client/gtk/brotypist-source.cmake and its adjacent patch.
//! 2. Privately stage reviewed fcitx5-qt 5.1.16 using the exact Qt6.12.0
//!    GuiPrivate SDK and native/linux-client/qt/BUILDING.txt + adjacent patch.
//!    Qt6.4 syntax checks or directly linked adapter copies are not acceptance.
//! 3. Have non-root Linux, Xvfb, openbox, dbus-run-session, fcitx5, GTK3.24,
//!    the compatible Fcitx daemon/batch protocol, fonts and both private plugins.
//!    The valid 160-letter middle row must naturally measure wider than the
//!    owned desktop; GTK requisitions / Qt font metrics prove that precondition.
//!    Inadequate font geometry fails rather than resizing fonts or skipping.
//! Commands for the integration owner, NOT RUN:
//!   cmake -S crates/brotypist/native/linux-client/tests -B "$QA/build" \
//!     -DCMAKE_PREFIX_PATH="$QT612"
//!   cmake --build "$QA/build"
//!   cargo build -p brotypist --features native-smoke --example native_client_smoke
//!   GTK_IM_MODULE_FILE="$GTK_STAGE/immodules.cache" GTK_PATH="$GTK_STAGE/lib/gtk-3.0" \
//!     QT_PLUGIN_PATH="$QT_STAGE/plugins" LD_LIBRARY_PATH="$QT612/lib:$GTK_STAGE/lib" \
//!     target/debug/examples/native_client_smoke "$QA/build/brotypist-gtk-fixture" \
//!       "$QA/build/brotypist-qt-fixture"
//! With the same private environment, isolate diagnostics with:
//!   target/debug/examples/native_client_smoke --client gtk "$QA/build/brotypist-gtk-fixture"
//!   target/debug/examples/native_client_smoke --client qt "$QA/build/brotypist-qt-fixture"
//! Default / `--client both` still require both fixtures and both private plugins.
//! Subset receipts identify requested/covered clients and are not acceptance
//! of both clients. Present failures report only fixed scenario/lane/stage
//! labels, allowlisted refusal reasons and native precondition booleans.
//! GTK additionally refuses deferred choice presentation when native before-paint
//! focus/layout changes invalidate the mapped rows, with one terminal wire reply.
//! The GTK cache must be generated with gtk-query-immodules-3.0 for the owned
//! patched module; QT_PLUGIN_PATH must contain the private platforminputcontexts.
//! The runner creates fresh HOME/XDG/INKLOW_HOME, D-Bus and Xvfb; it never installs
//! plugins, kills existing observers, downloads models or accesses the clipboard.
//! Evidence proves owned component/plugin + broker behavior, not physical typing,
//! model quality, Wayland, arbitrary applications or full application UI/controller.
//! Qt additionally exercises synchronous owned query-handler withdrawal, UTF16
//! selected/collapsed replacement, synthetic XTest-delivered keys, geometry and
//! broker-owner loss. Keys require the exact owned X11 focus and use xdotool
//! without --window (XSendEvent/zero timestamps are not platform-key evidence).
//! Qt modifier-release ownership also uses a temporary private Shift+A policy:
//! only the broker action is observed; no Core toggle or persistent config runs.
//! Failure diagnostics contain only fixed text-state/broker/transport labels,
//! bounded owned widget/edit/report counts, native caret/selection state, actual
//! spawned-fixture exit code/signal, and fixed stderr classes (never raw stderr).
//! Peer Metadata retains its closed production schema, without diagnostic fields.
//! Selected Snapshot carets are scalar range starts, not the native active
//! cursor: Unicode cases prove both positions independently.
//! Withheld popup painting is NOT an unexposed-window claim; the separate
//! minimized target case requires observed native isExposed=false.

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result, bail, ensure};
    use brotypist::{
        config::Config,
        core::Snapshot,
        features::match_choice_subsequence,
        platform::{
            Action, NativeClientAcceptance, NativeClientOffer, NativeHostEvent,
            NativeHostPresentation,
            client::smoke::{Harness, Wire},
        },
    };
    use std::{
        io::{BufRead, BufReader, Read, Write},
        os::unix::{fs::PermissionsExt, process::ExitStatusExt},
        path::{Path, PathBuf},
        process::{Child, ChildStdin, Command, Stdio},
        sync::{Arc, Mutex, mpsc},
        thread,
        time::{Duration, Instant},
    };
    use zbus::zvariant::{OwnedValue, Value};

    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn lines(reader: impl std::io::Read + Send + 'static) -> mpsc::Receiver<String> {
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if line.len() > 512 || send.send(line).is_err() {
                    break;
                }
            }
        });
        receive
    }
    #[derive(Debug, Default)]
    struct StderrEvidence {
        bytes: u32,
        qt_info: bool,
        qt_warning: bool,
        qt_critical: bool,
        qt_fatal: bool,
        qt_assertion: bool,
        other: bool,
        read_error: bool,
        eof: bool,
    }
    impl StderrEvidence {
        fn line(&mut self, line: &[u8]) {
            match line {
                b"owned-smoke:qt-info" => self.qt_info = true,
                b"owned-smoke:qt-warning" => self.qt_warning = true,
                b"owned-smoke:qt-critical" => self.qt_critical = true,
                b"owned-smoke:qt-fatal" => self.qt_fatal = true,
                b"owned-smoke:qt-assertion" => self.qt_assertion = true,
                _ => self.other = true,
            }
        }
    }
    fn fixture_stderr(
        mut reader: impl Read + Send + 'static,
    ) -> (Arc<Mutex<StderrEvidence>>, mpsc::Receiver<()>) {
        let evidence = Arc::new(Mutex::new(StderrEvidence::default()));
        let captured = Arc::clone(&evidence);
        let (done, receive) = mpsc::sync_channel(1);
        thread::spawn(move || {
            // Keep draining, but never retain or print arbitrary stderr. Both
            // the parser's storage and the reported byte count are bounded.
            let mut chunk = [0; 512];
            let mut line = [0; 64];
            let mut used = 0;
            let mut overflow = false;
            loop {
                let read = reader.read(&mut chunk);
                if matches!(&read, Err(error) if error.kind() == std::io::ErrorKind::Interrupted) {
                    continue;
                }
                let Ok(mut state) = captured.lock() else {
                    break;
                };
                match read {
                    Ok(0) => {
                        state.eof = true;
                        state.other |= used != 0 || overflow;
                        break;
                    }
                    Ok(count) => {
                        state.bytes = (state.bytes + count as u32).min(65536);
                        for &byte in &chunk[..count] {
                            if byte == b'\n' {
                                if overflow {
                                    state.other = true;
                                } else {
                                    state.line(&line[..used]);
                                }
                                used = 0;
                                overflow = false;
                            } else if used < line.len() {
                                line[used] = byte;
                                used += 1;
                            } else {
                                overflow = true;
                            }
                        }
                    }
                    Err(_) => {
                        state.read_error = true;
                        break;
                    }
                }
            }
            let _ = done.send(());
        });
        (evidence, receive)
    }
    struct Host {
        child: Owned,
        input: ChildStdin,
        output: mpsc::Receiver<String>,
        stderr: Arc<Mutex<StderrEvidence>>,
        stderr_done: mpsc::Receiver<()>,
    }
    #[derive(Debug)]
    struct State {
        text: String,
        caret: i32,
        second: String,
        edits: u32,
        deferred: u32,
        rows: u32,
        qt: Option<QtState>,
    }
    #[derive(Debug)]
    struct QtState {
        exposed_popups: u32,
        unexposed_popups: u32,
        selection: i32,
        query_invalidations: u32,
        target_exposed: bool,
        window: u32,
        widget_key_presses: u32,
        widget_key_releases: u32,
        // Bounded post-Fcitx widget observations, not XTest source metadata.
        widget_zero_time_keys: u32,
        widget_repeat_presses: u32,
    }
    fn unhex(s: &str) -> Result<String> {
        if s == "destroyed" {
            return Ok(s.into());
        }
        ensure!(s.len() % 2 == 0, "Malformed host text");
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(String::from_utf8(bytes)?)
    }
    impl Host {
        fn spawn(path: &Path, display: &str) -> Result<Self> {
            let mut child = Command::new(path)
                .env("DISPLAY", display)
                .env("GTK_IM_MODULE", "fcitx")
                .env("QT_IM_MODULE", "fcitx")
                .env("QT_QPA_PLATFORM", "xcb")
                .env("GDK_BACKEND", "x11")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let input = child.stdin.take().context("Host stdin")?;
            let output = lines(child.stdout.take().context("Host stdout")?);
            let (stderr, stderr_done) = fixture_stderr(child.stderr.take().context("Host stderr")?);
            Ok(Self {
                child: Owned(child),
                input,
                output,
                stderr,
                stderr_done,
            })
        }
        fn failure_evidence(&mut self) -> String {
            // Inspect the actual spawned process before Owned::drop kills it.
            // A still-running peer is not reported as an exit or as a timeout.
            let (state, code, signal) = match self.child.0.try_wait() {
                Ok(Some(status)) => {
                    // Bounded drain barrier, not a transport/startup retry.
                    let _ = self.stderr_done.recv_timeout(Duration::from_millis(100));
                    ("exited", status.code(), status.signal())
                }
                Ok(None) => ("running", None, None),
                Err(_) => ("status-unavailable", None, None),
            };
            match self.stderr.lock() {
                Ok(stderr) => format!(
                    "Owned fixture failure: process={state} exit_code={code:?} signal={signal:?} stderr={stderr:?}"
                ),
                Err(_) => format!(
                    "Owned fixture failure: process={state} exit_code={code:?} signal={signal:?} stderr=unavailable"
                ),
            }
        }
        fn command(&mut self, command: &str) -> Result<State> {
            writeln!(self.input, "{command}")?;
            self.input.flush()?;
            let line = self
                .output
                .recv_timeout(Duration::from_secs(3))
                .context("Owned fixture stalled")?;
            let fields: Vec<_> = line.split('\t').collect();
            ensure!(
                fields.len() == 6 || fields.len() == 16,
                "Malformed fixture response"
            );
            Ok(State {
                text: unhex(fields[0])?,
                caret: fields[1].parse()?,
                second: unhex(fields[2])?,
                edits: fields[3].parse()?,
                deferred: fields[4].parse()?,
                rows: fields[5].parse()?,
                qt: if fields.len() == 16 {
                    Some(QtState {
                        exposed_popups: fields[6].parse()?,
                        unexposed_popups: fields[7].parse()?,
                        selection: fields[8].parse()?,
                        query_invalidations: fields[9].parse()?,
                        target_exposed: fields[10] == "1",
                        window: fields[11].parse()?,
                        widget_key_presses: fields[12].parse::<u32>()?.min(4096),
                        widget_key_releases: fields[13].parse::<u32>()?.min(4096),
                        widget_zero_time_keys: fields[14].parse::<u32>()?.min(4096),
                        widget_repeat_presses: fields[15].parse::<u32>()?.min(4096),
                    })
                } else {
                    None
                },
            })
        }
        fn geometry(&mut self) -> Result<[f64; 4]> {
            writeln!(self.input, "geometry")?;
            self.input.flush()?;
            let line = self
                .output
                .recv_timeout(Duration::from_secs(3))
                .context("Owned geometry probe stalled")?;
            let fields: Vec<_> = line.split('\t').collect();
            ensure!(
                fields.len() == 5 && fields[0] == "geometry",
                "Malformed geometry response"
            );
            let measured = [
                fields[1].parse::<f64>()?,
                fields[2].parse()?,
                fields[3].parse()?,
                fields[4].parse()?,
            ];
            ensure!(
                measured
                    .iter()
                    .all(|value| value.is_finite() && *value > 0.0)
                    && measured[0] > measured[1]
                    && measured[2] + 24.0 < measured[1]
                    && measured[3] + 24.0 < measured[1],
                "Fixture must naturally exceed actual desktop width while first/third fit: {measured:?}"
            );
            Ok(measured)
        }
        fn cancellation(&mut self, expected: &str) -> Result<()> {
            writeln!(self.input, "cancellation")?;
            self.input.flush()?;
            let line = self
                .output
                .recv_timeout(Duration::from_secs(3))
                .context("Owned cancellation probe stalled")?;
            ensure!(
                line.strip_prefix("cancellation\t") == Some(expected),
                "Native before-paint cancellation was not observed: case={expected}"
            );
            Ok(())
        }
        fn query_withdrawn(&mut self, command: &str, state: &State) -> Result<()> {
            ensure!(
                matches!(command, "query-hide" | "query-destroy"),
                "Unexpected query withdrawal"
            );
            writeln!(self.input, "query-result")?;
            self.input.flush()?;
            let line = self
                .output
                .recv_timeout(Duration::from_secs(3))
                .context("Owned query witness stalled")?;
            let fields: Vec<_> = line.split('\t').collect();
            ensure!(
                fields.len() == 6
                    && fields[0] == "query-result"
                    && fields[1] == command
                    && unhex(fields[2])? == "seed "
                    && fields[3] == "5"
                    && fields[4] == "0"
                    && fields[5] == if command == "query-hide" { "0" } else { "1" },
                "Native pre-withdrawal state changed or wrong withdrawal occurred"
            );
            let qt = state.qt.as_ref().context("Missing Qt state")?;
            ensure!(
                qt.query_invalidations == 1
                    && state.edits == 0
                    && state.second == "other"
                    && state.rows == 0
                    && if command == "query-hide" {
                        state.text == "seed "
                            && state.caret == 5
                            && qt.selection == 0
                            && !qt.target_exposed
                    } else {
                        state.text == "destroyed" && state.caret == -1 && qt.selection == -1
                    },
                "Query withdrawal retained rows or mutated native fields"
            );
            Ok(())
        }
        fn unchanged(&mut self) -> Result<()> {
            let state = self.command("state")?;
            ensure!(
                state.text == "seed "
                    && state.caret == 5
                    && state.second == "other"
                    && state.edits == 0,
                "Refused/unarmed operation edited owned fields: {state:?}"
            );
            Ok(())
        }
    }
    fn settle(h: &mut Harness, milliseconds: u64) -> Result<()> {
        let until = Instant::now() + Duration::from_millis(milliseconds);
        while Instant::now() < until {
            h.pump()?;
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
    fn capture(h: &mut Harness) -> Result<Snapshot> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            h.pump()?;
            if let Some(snapshot) = h.bridge().capture()? {
                return Ok(snapshot);
            }
            ensure!(
                Instant::now() < until,
                "Mandatory real client capture unavailable (plugin/IME/focus/policy)"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn choices() -> Result<Vec<String>> {
        // Exactly 160 ordinary letters, one word, no controls/format characters:
        // geometry, not malformed-content rejection, must remove original row 2.
        let values = vec!["first".into(), "W".repeat(160), "third".into()];
        ensure!(
            values
                .iter()
                .all(|value| brotypist::features::safe_choice(value)),
            "Geometry fixture violates production choice validity"
        );
        Ok(values)
    }
    fn present(h: &mut Harness, ghost: bool, scenario: &str) -> Result<NativeHostPresentation> {
        let snapshot = capture(h).map_err(|_| {
            anyhow::anyhow!(
                "Mandatory native capture failed: scenario={scenario} lane={}",
                if ghost { "completion" } else { "alternatives" }
            )
        })?;
        let revision = h.bridge().user_input_revision();
        let values = choices()?;
        let result = h.bridge().present_client_offer(&NativeClientOffer {
            snapshot: &snapshot,
            user_input_revision: revision,
            mode: if ghost { "completion" } else { "alternatives" },
            text: if ghost { "ghost" } else { "" },
            values: if ghost { &[] } else { &values },
            start: 5,
            length: 0,
        });
        match result {
            Ok(Some(receipt)) => Ok(receipt),
            Ok(None) => bail!(
                "Mandatory native presentation refused: scenario={scenario} lane={} {:?}",
                if ghost { "completion" } else { "alternatives" },
                h.presentation_diagnostic()
            ),
            Err(_) => bail!(
                "Mandatory native presentation failed: scenario={scenario} lane={} {:?}",
                if ghost { "completion" } else { "alternatives" },
                h.presentation_diagnostic()
            ),
        }
    }
    fn arm(h: &mut Harness, receipt: &NativeHostPresentation) -> Result<()> {
        ensure!(
            h.bridge().arm_client_offer(&NativeClientAcceptance {
                presentation: receipt,
                word_text: "ghost",
                all_text: "ghost",
            })?,
            "Exact broker Arm refused"
        );
        Ok(())
    }
    fn clone_wire(wire: &Wire) -> Result<Wire> {
        wire.iter()
            .map(|(key, value)| Ok((key.clone(), value.try_clone()?)))
            .collect()
    }
    fn number(wire: &mut Wire, key: &str, value: u64) {
        wire.insert(key.into(), OwnedValue::from(value));
    }
    fn text(wire: &mut Wire, key: &str, value: &str) -> Result<()> {
        wire.insert(key.into(), OwnedValue::try_from(Value::from(value))?);
        Ok(())
    }
    fn presented(wire: &Wire) -> Result<bool> {
        Ok(bool::try_from(
            wire.get("presented")
                .context("Missing terminal Present result")?,
        )?)
    }
    fn monotonic_ns() -> u64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
            return 0;
        }
        time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
    }
    fn pending_result(
        handle: thread::JoinHandle<Result<brotypist::platform::client::smoke::Reply>>,
        expected: bool,
        deadline: u64,
    ) -> Result<u32> {
        let result = handle
            .join()
            .map_err(|_| anyhow::anyhow!("Present worker panicked"))??;
        let observed_ns = monotonic_ns();
        ensure!(
            presented(&result.value)? == expected,
            "Unexpected terminal Present result"
        );
        ensure!(
            observed_ns != 0 && (!expected || observed_ns < deadline),
            "Successful deferred receipt observed outside original deadline"
        );
        if expected {
            let capability = u64::try_from(
                result
                    .value
                    .get("capability_id")
                    .context("Missing capability")?,
            )?;
            let receipt_deadline = u64::try_from(
                result
                    .value
                    .get("expires_ns")
                    .context("Missing receipt deadline")?,
            )?;
            ensure!(
                capability != 0 && observed_ns < receipt_deadline && receipt_deadline <= deadline,
                "Deferred receipt extended its lease or returned no live capability"
            );
        }
        println!(
            "{}",
            serde_json::json!({
                "toolkit":"qt", "request_serial":result.serial,
                "offered_deadline_ns":deadline, "terminal_observed_ns":observed_ns,
                "presented":expected, "synthetic":true
            })
        );
        Ok(result.serial)
    }
    fn pending_cases(h: &mut Harness, host: &mut Host, config: &Config) -> Result<()> {
        let reports = h.accepted_reports();
        let mut replies = Vec::new();
        for case in [
            "resume-paint",
            "withheld-paint-expiry",
            "inline-preserves-pending",
            "stale-preserves-pending",
            "superseded",
            "hide",
            "focus2",
            "move",
            "minimize",
            "policy",
            "query-paint-hide",
            "query-paint-destroy",
            "query-final-hide",
            "query-final-destroy",
            "destroy",
        ] {
            host.command("reset")?;
            capture(h)?;
            host.command("defer")?;
            let lease = if case == "withheld-paint-expiry" {
                140
            } else {
                1000
            };
            let (offer, pending) = h.pending(lease, &choices()?)?;
            let deadline = u64::try_from(offer.get("expires_ns").context("Missing deadline")?)?;
            thread::sleep(Duration::from_millis(30));
            let state = host.command("state")?;
            ensure!(
                state.deferred > 0 && !pending.is_finished(),
                "Present resolved before real owner paint: {case}"
            );
            let qt = state
                .qt
                .as_ref()
                .context("Missing Qt native observations")?;
            let mut early = clone_wire(&offer)?;
            number(&mut early, "capability_id", 0);
            text(&mut early, "shown_text", "")?;
            text(&mut early, "word_text", "ghost")?;
            text(&mut early, "all_text", "ghost")?;
            early.insert(
                "shown_values".into(),
                OwnedValue::try_from(Value::from(vec!["first", "third"]))?,
            );
            ensure!(
                !h.arm_raw(early)?,
                "Pending presentation armed before paint"
            );
            host.command("choice2")?;
            host.unchanged()?;
            match case {
                "resume-paint" | "inline-preserves-pending" | "stale-preserves-pending" => {
                    if case == "inline-preserves-pending" {
                        let ghost = present(h, true, case)?;
                        arm(h, &ghost)?;
                    } else if case == "stale-preserves-pending" {
                        let mut stale = clone_wire(&offer)?;
                        number(&mut stale, "revision", u64::MAX);
                        replies.push(pending_result(h.present_raw(stale)?, false, deadline)?);
                    }
                    ensure!(
                        !pending.is_finished(),
                        "Unrelated offer revoked pending chooser"
                    );
                    host.unchanged()?;
                    host.command("resume")?;
                    replies.push(pending_result(pending, true, deadline)?);
                }
                "withheld-paint-expiry" => {
                    replies.push(pending_result(pending, false, deadline)?);
                }
                "superseded" => {
                    let (new_offer, newer) = h.pending(1000, &choices()?)?;
                    let new_deadline =
                        u64::try_from(new_offer.get("expires_ns").context("Missing deadline")?)?;
                    replies.push(pending_result(pending, false, deadline)?);
                    host.command("resume")?;
                    replies.push(pending_result(newer, true, new_deadline)?);
                }
                "policy" => {
                    let mut disabled = config.clone();
                    disabled.global_enabled = false;
                    h.bridge().configure(&disabled)?;
                    replies.push(pending_result(pending, false, deadline)?);
                    h.bridge().configure(config)?;
                }
                _ if case.starts_with("query-") => {
                    host.command(case)?;
                    host.command("resume")?;
                    replies.push(pending_result(pending, false, deadline)?);
                    ensure!(
                        host.command("state")?
                            .qt
                            .context("Missing Qt state")?
                            .query_invalidations
                            == 1,
                        "Owned final native query handler did not invalidate target: {case}"
                    );
                }
                _ => {
                    host.command(case)?;
                    if case == "minimize" {
                        let until = Instant::now() + Duration::from_secs(2);
                        loop {
                            if !host
                                .command("state")?
                                .qt
                                .context("Missing Qt state")?
                                .target_exposed
                            {
                                break;
                            }
                            ensure!(
                                Instant::now() < until,
                                "Target never became genuinely unexposed"
                            );
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                    replies.push(pending_result(pending, false, deadline)?);
                }
            }
            h.clear(None)?;
            host.command("resume")?;
            settle(h, 180)?;
            for serial in &replies {
                ensure!(
                    h.reply_count(*serial)? == 1,
                    "Present returned duplicate/missing wire replies"
                );
            }
            ensure!(
                h.accepted_reports() == reports,
                "Pending/refused Present emitted a completion"
            );
            let final_state = host.command("state")?;
            ensure!(
                final_state.edits == 0 && final_state.second == "other",
                "Query withdrawal or pending operation committed an edit"
            );
            if !case.ends_with("destroy") {
                host.command("reset")?;
                host.unchanged()?;
            }
            println!(
                "{}",
                serde_json::json!({
                    "toolkit":"qt", "case":case, "blocked_owner_events":state.deferred,
                    "observed_exposed_popups":qt.exposed_popups,
                    "observed_unexposed_popups":qt.unexposed_popups,
                    "target_observed_unexposed":case == "minimize",
                    "query_invalidations":final_state.qt.as_ref().map(|state| state.query_invalidations),
                    "armed_before_paint":false, "synthetic":true
                })
            );
        }
        Ok(())
    }
    fn gtk_pending_cases(h: &mut Harness, host: &mut Host) -> Result<()> {
        let reports = h.accepted_reports();
        let mut replies = Vec::new();
        for (command, cancellation) in [
            ("cancel-choice-focus", "focus"),
            ("cancel-choice-layout", "layout"),
            ("cancel-choice-row-destroy", "row-destroy"),
        ] {
            host.command("reset")?;
            capture(h)?;
            host.command(command)?;
            let (mut offer, pending) = h.pending(1000, &choices()?)?;
            let result = pending
                .join()
                .map_err(|_| anyhow::anyhow!("Present worker panicked"))??;
            ensure!(
                !presented(&result.value)?,
                "GTK deferred presentation survived native cancellation: case={cancellation}"
            );
            host.cancellation(cancellation)?;
            replies.push(result.serial);
            number(&mut offer, "capability_id", 0);
            text(&mut offer, "shown_text", "")?;
            text(&mut offer, "word_text", "ghost")?;
            text(&mut offer, "all_text", "ghost")?;
            offer.insert(
                "shown_values".into(),
                OwnedValue::try_from(Value::from(vec!["first", "third"]))?,
            );
            ensure!(
                !h.arm_raw(offer)?,
                "GTK refused pending presentation remained armable: case={cancellation}"
            );
            host.command("choice2")?;
            settle(h, 180)?;
            host.unchanged()?;
            ensure!(
                host.command("state")?.rows == 0
                    && h.accepted_reports() == reports
                    && !h
                        .bridge()
                        .drain_host_events()
                        .iter()
                        .any(|event| matches!(event, NativeHostEvent::Applied(_))),
                "GTK pending cancellation retained rows or reported an edit: case={cancellation}"
            );
            for serial in &replies {
                ensure!(
                    h.reply_count(*serial)? == 1,
                    "GTK deferred Present returned duplicate/missing wire replies"
                );
            }
            h.clear(None)?;
            println!(
                "{}",
                serde_json::json!({
                    "toolkit":"gtk", "case":command, "native_before_paint_cancellation":cancellation,
                    "request_serial":result.serial, "presented":false, "terminal_replies":1,
                    "native_edits":0, "completion_reports":0, "remaining_rows":0, "synthetic":true
                })
            );
        }
        Ok(())
    }
    fn native_key(host: &mut Host, display: &str, symbol: &str) -> Result<()> {
        native_key_delivery(host, display, symbol, "key", None)
    }
    fn native_key_delivery(
        host: &mut Host,
        display: &str,
        symbol: &str,
        delivery: &str,
        withdrawal: Option<&str>,
    ) -> Result<()> {
        use x11rb::{
            connection::Connection,
            protocol::xproto::{AtomEnum, ConnectionExt, MapState},
        };
        ensure!(
            matches!(delivery, "key" | "keydown" | "keyup")
                && matches!(
                    symbol,
                    "Down" | "Up" | "Return" | "Escape" | "ctrl+z" | "Shift_L" | "a"
                )
                && (withdrawal.is_none() || delivery == "key"),
            "Unexpected owned key"
        );
        let before = host.command("state")?;
        let qt = before.qt.context("Missing Qt window")?;
        let window = qt.window;
        if withdrawal.is_some() {
            ensure!(
                qt.query_invalidations == 0
                    && before.text == "seed "
                    && before.caret == 5
                    && qt.selection == 0
                    && before.edits == 0,
                "Query withdrawal happened before the native key"
            );
        }
        let (connection, screen) = x11rb::connect(Some(display))?;
        ensure!(
            connection.get_input_focus()?.reply()?.focus == window,
            "XTest key target is not the owned focused window"
        );
        // Without --window, xdotool uses XTest on the private server. Unlike
        // XSendEvent, this traverses the real platform input path with server
        // timestamps. Never focus another window or substitute widget methods.
        let status = Command::new("xdotool")
            .env("DISPLAY", display)
            .args([delivery, "--delay", "12", symbol])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("Private XTest key delivery unavailable")?;
        ensure!(status.success(), "Private XTest key delivery failed");
        if let Some(command) = withdrawal {
            // The owned query callback, not arbitrary focus loss, must account
            // for this exception. Never activate/reclaim a window after input.
            let after = host.command("state")?;
            host.query_withdrawn(command, &after)?;
            let focus = connection.get_input_focus()?.reply()?.focus;
            if command == "query-hide" {
                let root = connection.setup().roots[screen].root;
                let neutral_focus = if matches!(focus, 0 | 1) || focus == root {
                    true
                } else {
                    // Openbox may focus its EWMH support window when the last
                    // application hides. Require both sides of that identity;
                    // another application's window is never an allowed sink.
                    let atom = connection
                        .intern_atom(true, b"_NET_SUPPORTING_WM_CHECK")?
                        .reply()?
                        .atom;
                    if atom == 0 {
                        false
                    } else {
                        let root_check = connection
                            .get_property(false, root, atom, AtomEnum::WINDOW, 0, 2)?
                            .reply()?;
                        let self_check = connection
                            .get_property(false, focus, atom, AtomEnum::WINDOW, 0, 2)?
                            .reply()?;
                        let points_to_focus =
                            |reply: &x11rb::protocol::xproto::GetPropertyReply| {
                                reply.type_ == u32::from(AtomEnum::WINDOW)
                                    && reply.bytes_after == 0
                                    && reply.value32().is_some_and(|mut values| {
                                        values.next() == Some(focus) && values.next().is_none()
                                    })
                            };
                        points_to_focus(&root_check) && points_to_focus(&self_check)
                    }
                };
                ensure!(
                    connection.get_window_attributes(window)?.reply()?.map_state
                        == MapState::UNMAPPED
                        && neutral_focus,
                    "Query hide retargeted XTest focus to another window"
                );
            } else {
                ensure!(
                    focus == window,
                    "Query destruction unexpectedly changed XTest window focus"
                );
            }
        } else {
            ensure!(
                connection.get_input_focus()?.reply()?.focus == window,
                "Owned focus changed during XTest key delivery"
            );
        }
        Ok(())
    }
    fn unicode_cases(h: &mut Harness, host: &mut Host, display: &str) -> Result<()> {
        for case in ["unicode-selected", "unicode-collapsed"] {
            let selected = case == "unicode-selected";
            host.command("reset")?;
            capture(h)?;
            let before = host.command(case)?;
            let native_selection = before.qt.context("Missing Qt state")?.selection;
            ensure!(
                before.text == "A😀B"
                    && before.caret == 3
                    && native_selection == if selected { 2 } else { 0 },
                "Native UTF16 fixture precondition failed: case={case} text_matches={} caret={} selection={native_selection}",
                before.text == "A😀B",
                before.caret
            );
            let snapshot = capture(h)?;
            // Snapshot::selected_text/selection_suffix use [caret, caret + length).
            // Witness::snapshot converts each native endpoint, then uses min/abs_diff.
            // The active UTF16 cursor stays 3 above; selected scalar range [1, 2)
            // starts at 1, while the collapsed scalar cursor is 2.
            ensure!(
                snapshot.text.as_ref() == "A😀B"
                    && snapshot.caret == if selected { 1 } else { 2 }
                    && snapshot.selection_length == if selected { 1 } else { 0 }
                    && snapshot.prefix() == if selected { "A" } else { "A😀" }
                    && snapshot.selected_text() == if selected { "😀" } else { "" }
                    && snapshot.selection_suffix() == "B",
                "Broker scalar fixture precondition failed: case={case} text_matches={} caret={} selection={} prefix_matches={} selected_text_matches={} suffix_matches={}",
                snapshot.text.as_ref() == "A😀B",
                snapshot.caret,
                snapshot.selection_length,
                snapshot.prefix() == if selected { "A" } else { "A😀" },
                snapshot.selected_text() == if selected { "😀" } else { "" },
                snapshot.selection_suffix() == "B"
            );
            let reports = h.accepted_reports();
            let revision = h.bridge().user_input_revision();
            // Deliberately decomposed: neither rendering nor acceptance may
            // normalize the combining mark or split the non-BMP scalar.
            let values = vec!["first".to_owned(), "e\u{301}🚀 ".to_owned()];
            let mode = if selected { "synonyms" } else { "alternatives" };
            let receipt = h
                .bridge()
                .present_client_offer(&NativeClientOffer {
                    snapshot: &snapshot,
                    user_input_revision: revision,
                    mode,
                    text: "",
                    values: &values,
                    start: 1,
                    length: 1,
                })?
                .context("Mandatory Unicode native presentation refused")?;
            let NativeHostPresentation::Choices(rows) = &receipt else {
                bail!("Not a Unicode chooser")
            };
            ensure!(rows.values == values, "Unicode native rows were changed");
            arm(h, &receipt)?;
            if selected {
                for key in ["Down", "Up", "Down"] {
                    native_key(host, display, key)?;
                    settle(h, 20)?;
                    let navigation = host.command("state")?;
                    let keys = navigation
                        .qt
                        .as_ref()
                        .context("Missing Qt navigation state")?;
                    ensure!(
                        navigation.text == "A😀B"
                            && navigation.caret == 3
                            && navigation.edits == 0
                            && navigation.rows == 1
                            && keys.selection == 2
                            && keys.widget_key_presses == 0
                            && keys.widget_key_releases == 0
                            && h.accepted_reports() == reports
                            && h.bridge().drain_host_events().is_empty(),
                        "Owned Unicode navigation leaked/revoked or edited the selection: key={key} state={navigation:?}"
                    );
                }
                native_key(host, display, "Return")?;
            } else {
                host.command("choice2")?;
            }
            settle(h, 900)?;
            let events = h.bridge().drain_host_events();
            // Read the real owned widget BEFORE any report assertion: an absent
            // broker proof is not evidence that native dispatch/mutation occurred.
            let state = host.command("state")?;
            let qt = state.qt.as_ref().context("Missing Qt state")?;
            let report_delta = h.accepted_reports().saturating_sub(reports);
            let mut event_kinds = [0usize; 4];
            for event in &events {
                let kind = match event {
                    NativeHostEvent::Applied(_) => 0,
                    NativeHostEvent::Indeterminate(_) => 1,
                    NativeHostEvent::Dismissed(_) => 2,
                    NativeHostEvent::Action { .. } => 3,
                };
                event_kinds[kind] = event_kinds[kind].saturating_add(1).min(4096);
            }
            let text_state = match state.text.as_str() {
                "Ae\u{301}🚀 B" => "expected",
                "A😀B" => "unchanged",
                _ => "other",
            };
            eprintln!(
                "Unicode confirmation: case={case} events_applied={} events_indeterminate={} events_dismissed={} report_delta={} native_text={text_state} native_caret={} native_selection={} native_edits={} widget_keys={}/{}/{} broker_outcome={} broker_validation={}",
                event_kinds[0],
                event_kinds[1],
                event_kinds[2],
                report_delta.min(4096),
                state.caret,
                qt.selection,
                state.edits.min(4096),
                qt.widget_key_presses,
                qt.widget_key_releases,
                qt.widget_zero_time_keys,
                if report_delta == 0 {
                    "none"
                } else {
                    h.confirmation_diagnostic().outcome
                },
                if report_delta == 0 {
                    "not-received"
                } else {
                    h.confirmation_diagnostic().validation
                },
            );
            ensure!(
                events.len() == 1 && h.accepted_reports() == reports + 1,
                "Unicode replacement did not report exactly once: case={case}"
            );
            let NativeHostEvent::Applied(edit) = &events[0] else {
                bail!("No Unicode applied proof")
            };
            ensure!(
                edit.action == Action::Choice(1)
                    && edit.text == "e\u{301}🚀 "
                    && edit.after.text.as_ref() == "Ae\u{301}🚀 B"
                    && edit.after.caret == 5
                    && edit.after.selection_length == 0,
                "Wrong scalar Unicode replacement proof"
            );
            ensure!(
                state.text == "Ae\u{301}🚀 B"
                    && state.caret == 6
                    && state.edits == 1
                    && state.second == "other"
                    && qt.selection == 0,
                "Wrong native UTF16 Unicode replacement"
            );
            if selected {
                ensure!(
                    qt.widget_key_presses == 0 && qt.widget_key_releases == 0,
                    "Accepted Unicode key sequence leaked to the native widget"
                );
            }
            host.command("choice2")?;
            settle(h, 100)?;
            ensure!(
                host.command("state")?.edits == 1
                    && h.accepted_reports() == reports + 1
                    && h.bridge().drain_host_events().is_empty(),
                "Unicode replacement duplicated"
            );
            native_key(host, display, "ctrl+z")?;
            let undo_deadline = Instant::now() + Duration::from_millis(750);
            let undone = loop {
                h.pump()?;
                let current = host.command("state")?;
                if current.text == "A😀B" {
                    break current;
                }
                ensure!(
                    Instant::now() < undo_deadline,
                    "Native Unicode undo was not observed"
                );
                thread::sleep(Duration::from_millis(5));
            };
            ensure!(
                undone.text == "A😀B" && undone.second == "other" && undone.edits == 1,
                "One native undo did not restore the Unicode replacement"
            );
            settle(h, 100)?;
            ensure!(
                h.accepted_reports() == reports + 1 && h.bridge().drain_host_events().is_empty(),
                "Native undo duplicated the acceptance report"
            );
            println!(
                "{}",
                serde_json::json!({
                    "toolkit":"qt", "case":case, "mode":mode, "native_utf16_caret":6, "broker_scalar_caret":5,
                    "before_native_utf16_caret":before.caret, "before_native_utf16_selection":native_selection,
                    "before_broker_scalar_caret":snapshot.caret, "before_broker_scalar_selection":snapshot.selection_length,
                    "snapshot_caret_is_range_start":true,
                    "collapsed_selection":true, "native_commits":1, "applied_reports":1,
                    "key_delivery":if selected { "private-x11-xtest" } else { "owned-popup-click" },
                    "widget_key_presses":qt.widget_key_presses,
                    "widget_key_releases":qt.widget_key_releases,
                    "widget_zero_time_keys":qt.widget_zero_time_keys,
                    "widget_key_metadata_scope":"post-fcitx-widget-delivery",
                    "widget_key_counter_limit":4096,
                    "exact_replacement":true, "nfd_preserved":true, "native_undo":true, "synthetic":true
                })
            );
        }
        Ok(())
    }
    fn key_ownership_cases(h: &mut Harness, host: &mut Host, display: &str) -> Result<()> {
        let reports = h.accepted_reports();
        for case in [
            "unmatched-release",
            "held-return-repeat",
            "focus-roundtrip-release",
        ] {
            host.command("reset")?;
            capture(h)?;
            let focus_roundtrip = case == "focus-roundtrip-release";
            if !focus_roundtrip {
                // The initial press belongs to the native editor, not a chooser.
                // A later Arm must not claim its release or accept its repeats.
                native_key_delivery(host, display, "Return", "keydown", None)?;
                let deadline = Instant::now() + Duration::from_millis(500);
                loop {
                    h.pump()?;
                    let state = host.command("state")?;
                    let qt = state.qt.context("Missing Qt state")?;
                    if qt.widget_key_presses == 1 {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Initial native Return press was not observed"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
            }
            let receipt = present(h, false, case)?;
            arm(h, &receipt)?;
            let wire = h.arm_wire(&receipt)?;
            if focus_roundtrip {
                native_key_delivery(host, display, "Down", "keydown", None)?;
                settle(h, 50)?;
                let state = host.command("state")?;
                let consumed = state.qt.context("Missing Qt state")?;
                ensure!(
                    state.rows == 1
                        && consumed.widget_key_presses == 0
                        && consumed.widget_key_releases == 0,
                    "Owned navigation press leaked or lost its chooser before focus transition"
                );
                // Return to the same object before releasing: comparing only
                // identities at the next key misses this intervening transition.
                host.command("focus2")?;
                host.command("focus1")?;
            } else if case == "held-return-repeat" {
                let armed = host.command("state")?;
                let qt = armed.qt.context("Missing Qt state")?;
                ensure!(
                    armed.rows == 1 && qt.widget_key_presses == 1 && qt.widget_key_releases == 0,
                    "Return repeated before the chooser was armed"
                );
                // Exercise the X server's real repeat stream without changing
                // keyboard settings or manufacturing Qt auto-repeat events.
                // Fcitx replay can reconstruct QKeyEvent with isAutoRepeat=false.
                // One injected keydown followed by additional delivered presses,
                // before any injected keyup, is the native repeat evidence.
                let deadline = Instant::now() + Duration::from_millis(1500);
                loop {
                    h.pump()?;
                    let state = host.command("state")?;
                    if state.qt.context("Missing Qt state")?.widget_key_presses > 1 {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Native Return auto-repeat was not observed before lease expiry"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
            }
            let releases_before_keyup = host
                .command("state")?
                .qt
                .context("Missing Qt state")?
                .widget_key_releases;
            native_key_delivery(
                host,
                display,
                if focus_roundtrip { "Down" } else { "Return" },
                "keyup",
                None,
            )?;
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                h.pump()?;
                let state = host.command("state")?;
                if state
                    .qt
                    .as_ref()
                    .context("Missing Qt state")?
                    .widget_key_releases
                    > releases_before_keyup
                {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "Unowned native key release was swallowed"
                );
                thread::sleep(Duration::from_millis(5));
            }
            settle(h, 100)?;
            let state = host.command("state")?;
            let qt = state.qt.context("Missing Qt state")?;
            ensure!(
                if focus_roundtrip {
                    qt.widget_key_presses == 0 && qt.widget_key_releases == 1
                } else if case == "unmatched-release" {
                    qt.widget_key_presses == 1 && qt.widget_key_releases == 1
                } else {
                    qt.widget_key_presses > 1 && qt.widget_key_releases > releases_before_keyup
                },
                "{case}: native key ownership counts: presses={}, releases={}, repeats={}, zero_time={}",
                qt.widget_key_presses,
                qt.widget_key_releases,
                qt.widget_repeat_presses,
                qt.widget_zero_time_keys
            );
            host.unchanged()?;
            ensure!(
                state.rows == 0
                    && qt.selection == 0
                    && !h.arm_raw(wire)?
                    && h.accepted_reports() == reports
                    && h.bridge().drain_host_events().is_empty(),
                "Unowned release/repeat/focus transition retained authority or dispatched an edit"
            );
            h.clear(None)?;
            println!(
                "{}",
                serde_json::json!({
                    "toolkit":"qt", "case":case, "key_delivery":"private-x11-xtest",
                    "widget_key_presses":qt.widget_key_presses,
                    "widget_key_releases":qt.widget_key_releases,
                    "widget_repeat_presses":qt.widget_repeat_presses,
                    "widget_zero_time_keys":qt.widget_zero_time_keys,
                    "widget_key_metadata_scope":"post-fcitx-widget-delivery",
                    "widget_key_counter_limit":4096,
                    "injected_key_presses":1, "injected_key_releases":1,
                    "held_key_repeat_sequence":case == "held-return-repeat",
                    "native_commits":0, "completion_reports":0,
                    "native_readback_unchanged":true, "synthetic":true
                })
            );
        }
        Ok(())
    }
    fn modifier_release_ownership_case(
        h: &mut Harness,
        host: &mut Host,
        display: &str,
        config: &Config,
    ) -> Result<()> {
        host.command("reset")?;
        capture(h)?;
        ensure!(
            h.bridge().drain_host_events().is_empty(),
            "Unexpected prior broker action"
        );
        let reports = h.accepted_reports();
        let mut private_policy = config.clone();
        private_policy
            .shortcuts
            .insert("toggle".into(), "Shift+A".into());
        let mut a_held = false;
        let mut shift_held = false;
        // Always restore the private broker policy and release owned held keys,
        // including on an assertion/transport failure. Never invoke Core::toggle
        // or save Config: this harness has no application/controller attached.
        let result = (|| -> Result<()> {
            h.bridge().configure(&private_policy)?;
            capture(h)?;
            shift_held = true;
            native_key_delivery(host, display, "Shift_L", "keydown", None)?;
            a_held = true;
            native_key_delivery(host, display, "a", "keydown", None)?;
            native_key_delivery(host, display, "Shift_L", "keyup", None)?;
            shift_held = false;
            // Same physical A scan code, now resolved to lowercase by XKB.
            native_key_delivery(host, display, "a", "keyup", None)?;
            a_held = false;
            settle(h, 100)?;
            let consumed = host.command("state")?;
            let consumed_keys = consumed.qt.as_ref().context("Missing Qt state")?;
            let events = h.bridge().drain_host_events();
            ensure!(
                matches!(
                    events.as_slice(),
                    [NativeHostEvent::Action {
                        action: Action::Toggle,
                        ..
                    }]
                ),
                "Modifier-release shortcut did not emit exactly one broker toggle action: events={}",
                events.len().min(4096)
            );

            // Do not Reset, refocus, reconfigure, or clear between the two
            // sequences: those could erase the stale consumed-press ownership.
            a_held = true;
            native_key_delivery(host, display, "a", "keydown", None)?;
            let deadline = Instant::now() + Duration::from_millis(500);
            let ordinary_press = loop {
                h.pump()?;
                let state = host.command("state")?;
                if state.text == "seed a" {
                    break state;
                }
                ensure!(
                    Instant::now() < deadline,
                    "Ordinary native A press was not delivered"
                );
                thread::sleep(Duration::from_millis(5));
            };
            shift_held = true;
            native_key_delivery(host, display, "Shift_L", "keydown", None)?;
            // The ordinary press was lowercase, but this release is uppercase:
            // a stale Shift+A entry must not steal this widget-owned release.
            native_key_delivery(host, display, "a", "keyup", None)?;
            a_held = false;
            native_key_delivery(host, display, "Shift_L", "keyup", None)?;
            shift_held = false;
            settle(h, 100)?;
            let ordinary_release = host.command("state")?;
            let pressed = ordinary_press.qt.as_ref().context("Missing Qt state")?;
            let released = ordinary_release.qt.as_ref().context("Missing Qt state")?;
            let text_unchanged = consumed.text == "seed "
                && consumed.caret == 5
                && consumed.second == "other"
                && consumed_keys.selection == 0;
            let ordinary_typing = [&ordinary_press, &ordinary_release].iter().all(|state| {
                state.text == "seed a"
                    && state.caret == 6
                    && state.second == "other"
                    && state.qt.as_ref().is_some_and(|qt| qt.selection == 0)
            });
            let no_commits = [&consumed, &ordinary_press, &ordinary_release]
                .iter()
                .all(|state| state.edits == 0 && state.rows == 0)
                && h.accepted_reports() == reports
                && h.bridge().drain_host_events().is_empty();
            // Counters include Shift. Correct totals are 1/1 after the shortcut,
            // 2/1 after ordinary A down, and 3/3 after the second Shift pair.
            // Final totals alone are insufficient: the leaked first release
            // and stolen second release cancel each other out.
            ensure!(
                consumed_keys.widget_key_presses == 1
                    && consumed_keys.widget_key_releases == 1
                    && pressed.widget_key_presses == 2
                    && pressed.widget_key_releases == 1
                    && released.widget_key_presses == 3
                    && released.widget_key_releases == 3
                    && text_unchanged
                    && ordinary_typing
                    && no_commits,
                "Modifier-release ownership: consumed_presses={} consumed_releases={} ordinary_presses={} ordinary_releases={} final_presses={} final_releases={} shortcut_text_unchanged={text_unchanged} ordinary_typing={ordinary_typing} no_commits={no_commits}",
                consumed_keys.widget_key_presses,
                consumed_keys.widget_key_releases,
                pressed.widget_key_presses,
                pressed.widget_key_releases,
                released.widget_key_presses,
                released.widget_key_releases
            );
            println!(
                "{}",
                serde_json::json!({
                    "toolkit":"qt", "case":"modifier-release-ownership",
                    "key_delivery":"private-x11-xtest", "broker_toggle_actions":1,
                    "core_toggle_invoked":false, "persistent_config_written":false,
                    "consumed_widget_key_presses":consumed_keys.widget_key_presses,
                    "consumed_widget_key_releases":consumed_keys.widget_key_releases,
                    "ordinary_press_widget_key_presses":pressed.widget_key_presses,
                    "ordinary_press_widget_key_releases":pressed.widget_key_releases,
                    "widget_key_presses":released.widget_key_presses,
                    "widget_key_releases":released.widget_key_releases,
                    "widget_key_metadata_scope":"post-fcitx-widget-delivery",
                    "widget_key_counter_limit":4096,
                    "injected_key_presses":4, "injected_key_releases":4,
                    "native_commits":0, "completion_reports":0,
                    "shortcut_native_readback_unchanged":true,
                    "ordinary_native_text_and_caret":true, "synthetic":true
                })
            );
            Ok(())
        })();
        // Each cleanup injection still requires the exact owned X11 focus;
        // never retarget a key to another window to recover from a failure.
        let release_a = if a_held {
            native_key_delivery(host, display, "a", "keyup", None)
        } else {
            Ok(())
        };
        let release_shift = if shift_held {
            native_key_delivery(host, display, "Shift_L", "keyup", None)
        } else {
            Ok(())
        };
        let restore = h.bridge().configure(config);
        ensure!(
            release_a.is_ok() && release_shift.is_ok() && restore.is_ok(),
            "Modifier-release owned cleanup failed: release_a={} release_shift={} restore_policy={}; scenario={}",
            release_a.is_ok(),
            release_shift.is_ok(),
            restore.is_ok(),
            result
                .as_ref()
                .err()
                .map_or_else(|| "passed".into(), ToString::to_string)
        );
        result
    }
    fn live_query_cases(h: &mut Harness, host: &mut Host, display: &str) -> Result<()> {
        let reports = h.accepted_reports();
        for action in ["arm", "down", "return"] {
            for command in ["query-hide", "query-destroy"] {
                host.command("reset")?;
                capture(h)?;
                let receipt = present(h, false, "live-query-withdrawal")?;
                let wire = h.arm_wire(&receipt)?;
                if action != "arm" {
                    arm(h, &receipt)?;
                }
                host.command(command)?;
                if action == "arm" {
                    ensure!(
                        !h.arm_raw(clone_wire(&wire)?)?,
                        "Query-withdrawn presentation armed"
                    );
                } else {
                    native_key_delivery(
                        host,
                        display,
                        if action == "down" { "Down" } else { "Return" },
                        "key",
                        Some(command),
                    )?;
                }
                settle(h, 100)?;
                let state = host.command("state")?;
                host.query_withdrawn(command, &state)?;
                ensure!(
                    h.accepted_reports() == reports && !h.arm_raw(clone_wire(&wire)?)?,
                    "Query-withdrawn presentation reported or remained armable"
                );
                ensure!(
                    h.bridge().drain_host_events().is_empty(),
                    "Query-withdrawn presentation emitted an applied event"
                );
                h.clear(None)?;
                println!(
                    "{}",
                    serde_json::json!({
                        "toolkit":"qt", "case":"live-query-withdrawal", "action":action,
                        "withdrawal":command, "native_commits":0, "synthetic":true,
                        "completion_reports":0, "remaining_rows":0, "native_readback_unchanged":true,
                        "key_delivery":if action == "arm" { "none" } else { "private-x11-xtest" }
                    })
                );
            }
        }
        host.command("reset")?;
        capture(h)?;
        let receipt = present(h, false, "native-escape")?;
        arm(h, &receipt)?;
        let wire = h.arm_wire(&receipt)?;
        native_key(host, display, "Escape")?;
        settle(h, 100)?;
        ensure!(!h.arm_raw(wire)?, "Escape left chooser armable");
        host.unchanged()?;
        ensure!(
            host.command("state")?.rows == 0 && h.accepted_reports() == reports,
            "Escape did not dismiss native chooser"
        );
        h.bridge().drain_host_events();
        h.clear(None)?;
        Ok(())
    }
    fn exercise(path: &Path, display: &str, qt: bool) -> Result<()> {
        let config = Config {
            enabled: true,
            global_enabled: true,
            alternatives_enabled: true,
            record_history: false,
            use_history: false,
            ..Config::default()
        };
        let mut h = Harness::open(&config)?;
        h.bridge().start_shortcuts(Arc::new(|_| false))?;
        h.bridge().set_shortcuts_enabled(true);
        let mut host = Host::spawn(path, display)?;
        // Keep the broker alive while collecting failure evidence: teardown
        // must not manufacture the peer exit that we are trying to diagnose.
        let mut harness = Some(h);
        let result = exercise_host(&mut harness, &mut host, display, qt, config);
        result.with_context(|| host.failure_evidence())
    }
    fn exercise_host(
        harness: &mut Option<Harness>,
        mut host: &mut Host,
        display: &str,
        qt: bool,
        config: Config,
    ) -> Result<()> {
        let mut h = harness.as_mut().context("Owned broker missing")?;
        if !qt {
            host.command("ready")?;
        }
        let until = Instant::now() + Duration::from_secs(10);
        while !h.bind(
            host.child.0.id(),
            if qt {
                "qt6-fcitx-client/1"
            } else {
                "gtk3-fcitx-client/1"
            },
        )? {
            ensure!(
                host.child.0.try_wait()?.is_none() && Instant::now() < until,
                "Mandatory patched plugin registration missing"
            );
            thread::sleep(Duration::from_millis(25));
        }
        host.command("reset")?;
        capture(&mut h)?;
        let measured = host.geometry()?;
        let ghost = present(&mut h, true, "concurrent-lanes")?;
        let choice = present(&mut h, false, "concurrent-lanes")?;
        let NativeHostPresentation::Choices(rows) = &choice else {
            bail!("Not a chooser")
        };
        ensure!(
            rows.values == ["first", "third"],
            "Native compact rows differ"
        );
        let offered = choices()?;
        let map = match_choice_subsequence(&offered, rows.values.iter().map(String::as_str))
            .context("Actual native choice receipt is not an exact subsequence")?;
        ensure!(
            map.original_index(1) == Some(2),
            "Displayed choice2 must map to original3"
        );
        let state = host.command("state")?;
        ensure!(
            state.rows == if qt { 1 } else { 2 },
            "Actual native chooser missing: {state:?}"
        );
        host.command("choice2")?;
        host.unchanged()?;
        // Every Arm binding member is independently mismatched against the real receipt.
        for key in [
            "offer_id",
            "capability_id",
            "policy_epoch",
            "expires_ns",
            "shown_text",
            "shown_values",
        ] {
            let mut bad = h.arm_wire(&choice)?;
            match key {
                "shown_text" => text(&mut bad, key, "different")?,
                "shown_values" => {
                    bad.insert(
                        key.into(),
                        OwnedValue::try_from(Value::from(vec!["third", "first"]))?,
                    );
                }
                _ => number(&mut bad, key, u64::MAX),
            }
            ensure!(!h.arm_raw(bad)?, "Mismatched {key} armed native chooser");
        }
        // Clearing a chooser must not clear the concurrent native ghost.
        h.clear(Some(&choice))?;
        arm(&mut h, &ghost)?;
        let choice = present(&mut h, false, "clear-chooser-preserves-ghost")?;
        arm(&mut h, &choice)?;
        // Clearing the ghost must likewise preserve chooser authority.
        h.clear(Some(&ghost))?;
        host.command("choice2")?;
        settle(&mut h, 900)?;
        let events = h.bridge().drain_host_events();
        ensure!(
            events.len() == 1 && h.accepted_reports() == 1,
            "Completion was not reported exactly once"
        );
        let NativeHostEvent::Applied(edit) = &events[0] else {
            bail!("Native edit did not yield applied proof")
        };
        ensure!(
            edit.action == Action::Choice(1)
                && edit.text == "third"
                && edit.after.text.as_ref() == "seed third"
                && edit.after.caret == 10
                && edit.after.selection_length == 0,
            "Wrong broker native readback"
        );
        let state = host.command("state")?;
        ensure!(
            state.text == "seed third"
                && state.caret == 10
                && state.second == "other"
                && state.edits == 1,
            "Real widget did not commit exactly once: {state:?}"
        );
        println!(
            "{}",
            serde_json::json!({"toolkit":if qt {"qt"} else {"gtk"},
            "displayed_choice":2,"original_choice":3,"text":state.text,"caret":state.caret,
            "native_edits":state.edits,"completion_reports":h.accepted_reports(),"synthetic":true,
            "middle_row_valid":true,"middle_row_native_width":measured[0],"desktop_available_width":measured[1],
            "first_row_native_width":measured[2],"third_row_native_width":measured[3]})
        );
        if !qt {
            gtk_pending_cases(&mut h, &mut host)?;
        }
        for case in [
            "expired",
            "superseded",
            "hidden",
            "focus",
            "policy",
            "geometry",
            "target-destroyed",
        ] {
            if (qt && case == "target-destroyed") || (!qt && case == "geometry") {
                continue;
            } // Qt destruction is exercised during synchronous queries and pending paint.
            host.command("reset")?;
            capture(&mut h)?;
            let receipt = present(&mut h, false, case)?;
            let wire = h.arm_wire(&receipt)?;
            match case {
                "expired" => thread::sleep(Duration::from_millis(2100)),
                "superseded" => {
                    present(&mut h, false, "superseding-offer")?;
                }
                "hidden" => {
                    host.command("hide-popup")?;
                }
                "focus" => {
                    host.command("focus2")?;
                }
                "geometry" => {
                    host.command("move")?;
                }
                "policy" => {
                    let mut disabled = config.clone();
                    disabled.global_enabled = false;
                    h.bridge().configure(&disabled)?;
                }
                "target-destroyed" => {
                    host.command("destroy")?;
                }
                _ => unreachable!(),
            }
            ensure!(!h.arm_raw(wire)?, "{case} receipt remained armable");
            host.command("choice2")?;
            settle(&mut h, 100)?;
            ensure!(
                h.accepted_reports() == 1,
                "Refusal emitted an additional completion"
            );
            if case != "target-destroyed" {
                host.unchanged()?;
            }
            h.clear(None)?;
            h.bridge().configure(&config)?;
            println!(
                "{}",
                serde_json::json!({"toolkit":if qt {"qt"} else {"gtk"},"refusal":case,"synthetic":true})
            );
        }
        if qt {
            unicode_cases(&mut h, &mut host, display)?;
            live_query_cases(&mut h, &mut host, display)?;
            key_ownership_cases(&mut h, &mut host, display)?;
            modifier_release_ownership_case(&mut h, &mut host, display, &config)?;
            pending_cases(&mut h, &mut host, &config)?;
            host.command("reset")?;
            capture(&mut h)?;
            let receipt = present(&mut h, false, "orderly-broker-disconnect")?;
            arm(&mut h, &receipt)?;
            // This exercises real owned broker shutdown/connection loss, not
            // isolated abrupt-owner-loss: orderly shutdown first sends Clear.
            drop(harness.take());
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                if host.command("state")?.rows == 0 {
                    break;
                }
                ensure!(
                    Instant::now() < until,
                    "Disconnected broker left native chooser visible"
                );
                thread::sleep(Duration::from_millis(10));
            }
            host.command("choice2")?;
            host.unchanged()?;
            println!(
                "{}",
                serde_json::json!({
                    "toolkit":"qt", "case":"orderly-broker-disconnect",
                    "native_commits":0, "synthetic":true
                })
            );
        }
        Ok(())
    }
    fn private_directory(root: &Path, name: &str) -> Result<PathBuf> {
        let path = root.join(name);
        std::fs::create_dir_all(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }
    pub fn run() -> Result<()> {
        ensure!(unsafe { libc::geteuid() } != 0, "No root UI execution");
        let mut args: Vec<_> = std::env::args_os().skip(1).collect();
        let private = args.first().is_some_and(|value| value == "--private");
        if private {
            args.remove(0);
            ensure!(
                std::env::var("BROTYPIST_OWNED_CLIENT_SMOKE").as_deref() == Ok("1"),
                "Private runner requires owned-client opt-in"
            );
        }
        let requested = if args.first().is_some_and(|value| value == "--client") {
            ensure!(args.len() >= 2, "Missing --client gtk|qt|both selection");
            args.remove(0);
            match args.remove(0).to_str() {
                Some("gtk") => "gtk",
                Some("qt") => "qt",
                Some("both") => "both",
                _ => bail!("Invalid --client selection; expected gtk|qt|both"),
            }
        } else {
            "both"
        };
        let gtk = requested != "qt";
        let qt = requested != "gtk";
        ensure!(
            args.len() == if gtk && qt { 2 } else { 1 },
            "Usage: native_client_smoke [--client both] <private-gtk-fixture> <private-qt-fixture>; --client gtk|qt <private-fixture>"
        );
        for key in ["GTK_IM_MODULE_FILE", "GTK_PATH", "QT_PLUGIN_PATH"] {
            if (key == "QT_PLUGIN_PATH" && qt) || (key != "QT_PLUGIN_PATH" && gtk) {
                ensure!(
                    std::env::var_os(key).is_some_and(|v| !v.is_empty()),
                    "Missing private integration input {key}"
                );
            }
        }
        if !private {
            let fixtures = args
                .iter()
                .map(std::fs::canonicalize)
                .collect::<std::io::Result<Vec<_>>>()?;
            let root = tempfile::tempdir()?;
            let home = private_directory(root.path(), "home")?;
            let config = private_directory(root.path(), "config")?;
            let fcitx = private_directory(&config, "fcitx5")?;
            std::fs::write(
                fcitx.join("profile"),
                "[Groups/0]\nName=Default\nDefault Layout=us\nDefaultIM=keyboard-us\n\n[Groups/0/Items/0]\nName=keyboard-us\nLayout=\n\n[GroupOrder]\n0=Default\n",
            )?;
            let status = Command::new("dbus-run-session")
                .arg("--")
                .arg(std::env::current_exe()?)
                .arg("--private")
                .args(["--client", requested])
                .args(fixtures)
                .env_remove("DBUS_SESSION_BUS_ADDRESS")
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env("HOME", home)
                .env("XDG_CONFIG_HOME", config)
                .env("XDG_CACHE_HOME", private_directory(root.path(), "cache")?)
                .env("XDG_DATA_HOME", private_directory(root.path(), "data")?)
                .env(
                    "XDG_RUNTIME_DIR",
                    private_directory(root.path(), "runtime")?,
                )
                .env("INKLOW_HOME", private_directory(root.path(), "inklow")?)
                .env("BROTYPIST_OWNED_CLIENT_SMOKE", "1")
                .env(
                    "BROTYPIST_NATIVE_BUS",
                    "org.brotypist.Native1.OwnedAcceptance",
                )
                .status()
                .context("Starting private D-Bus session")?;
            ensure!(
                status.success(),
                "Mandatory native client acceptance failed: {status}"
            );
            return Ok(());
        }
        let mut xvfb = Owned(
            Command::new("Xvfb")
                .args([
                    "-displayfd",
                    "1",
                    "-screen",
                    "0",
                    "1280x800x24",
                    "-nolisten",
                    "tcp",
                ])
                .stdout(Stdio::piped())
                .spawn()
                .context("Starting mandatory private Xvfb")?,
        );
        let number = lines(xvfb.0.stdout.take().context("Xvfb display pipe")?)
            .recv_timeout(Duration::from_secs(5))
            .context("Xvfb did not allocate a display")?;
        let display = format!(":{}", number.parse::<u32>()?);
        let _wm = Owned(
            Command::new("openbox")
                .env("DISPLAY", &display)
                .spawn()
                .context("Starting private window manager")?,
        );
        let _im = Owned(
            Command::new("fcitx5")
                .env("DISPLAY", &display)
                .spawn()
                .context("Starting private Fcitx daemon")?,
        );
        thread::sleep(Duration::from_millis(500));
        let mut coverage = Vec::new();
        if gtk {
            exercise(Path::new(&args[0]), &display, false).context("GTK mandatory coverage")?;
            coverage.push("gtk3-patched-plugin");
        }
        if qt {
            exercise(Path::new(&args[usize::from(gtk)]), &display, true)
                .context("Qt6.12 mandatory coverage")?;
            coverage.push("qt6.12-patched-plugin");
        }
        println!(
            "{}",
            serde_json::json!({"result":"passed","requested":requested,"coverage":coverage,
            "canonical_both":gtk && qt,
            "input":"owned synthetic widget dispatch","physical_input":false,"model":false})
        );
        Ok(())
    }
}
fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    match linux::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Native client acceptance failed: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!(
            "Native client acceptance requires non-root Linux; no mandatory coverage was run."
        );
        std::process::ExitCode::FAILURE
    }
}
