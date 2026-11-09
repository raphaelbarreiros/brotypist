//! Actual MacBridge acceptance against a private, separately spawned AppKit child.
//! Never compiled into a normal build. The original owned-AX mode never posts
//! input; separate owned-emoji permits only authenticated child-PID Unicode.
//! Owned-tab-row only sets source fixtures through the child's native widget.
//! None starts hooks, requests permission, or uses context/model/profile services.
//! Per-user OS services are not confined by HOME; see the receipt's explicit limit.
//! Receipts retain completed checks and raw requested/native widget observations
//! even on failure. Interior composed-character selections are probes, not assumed
//! host capabilities; exact acceptance uses the whole authored combining sequence.
use super::macos_smoke::write_new;
use super::*;
use crate::{
    core::Snapshot,
    platform::{
        EditOutcome, NativeBridge,
        macos::{MacBridge, ax},
    },
};
use objc2::runtime::Sel;
use objc2_app_kit::NSTextView;
use objc2_foundation::NSRange;
use rand::RngCore;
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{
            fs::{DirBuilderExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::Path,
    process::{Child, Command as ProcessCommand, Stdio},
};

const INITIAL: &str = "Café 🙂 e\u{301} fim";
const SECOND: &str = "Segundo campo — 日本語";
const CHANGED: &str = "Mudança 🙂 e\u{301} fim";
const REPLACED: &str = "Café 🚀 e\u{301} fim";
const EMOJI_ALIAS: &str = "🙂 e\u{301} :coder: o\u{301} 🚀";
const EMOJI_EMOTICON: &str = "🙂 e\u{301} :-) o\u{301} 🚀";
const EMOJI_ZWJ_RESULT: &str = "🙂 e\u{301} 👩‍💻 o\u{301} 🚀";
const EMOJI_FACE_RESULT: &str = "🙂 e\u{301} 🙂 o\u{301} 🚀";
const MAX_FRAME: usize = 16 * 1024;

fn send(stream: &mut UnixStream, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "Owned protocol frame too large");
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn receive(stream: &mut UnixStream) -> Result<Value> {
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    ensure!(
        size > 0 && size <= MAX_FRAME,
        "Invalid owned protocol frame length"
    );
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn timeout(stream: &UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    Ok(())
}
fn private_output(output: &Path) -> Result<macos_smoke::OwnedProfileOutput> {
    let paths = macos_smoke::owned_profile_output(output)?;
    write_new(
        &paths.home.join(".owned-ax-smoke"),
        b"Source-synthetic external AX fixture\n",
    )?;
    Ok(paths)
}

struct EditorChild {
    process: Child,
    stream: UnixStream,
    nonce: String,
    sequence: u64,
    observations: Vec<Value>,
}
impl EditorChild {
    fn request(&mut self, operation: &str) -> Result<Value> {
        ensure!(
            self.process.try_wait()?.is_none(),
            "Owned editor exited; refusing further access"
        );
        self.sequence += 1;
        self.observations
            .push(json!({"operation":operation,"sequence":self.sequence}));
        send(
            &mut self.stream,
            &json!({"nonce":self.nonce,"sequence":self.sequence,"operation":operation}),
        )?;
        let response = receive(&mut self.stream)?;
        self.observations.last_mut().unwrap()["response"] = response.clone();
        ensure!(
            response["nonce"] == self.nonce && response["sequence"] == self.sequence,
            "Owned protocol authentication/sequence mismatch"
        );
        ensure!(
            response["ok"] == true,
            "Owned editor refused: {}",
            response["error"]
        );
        Ok(response)
    }
}
impl Drop for EditorChild {
    fn drop(&mut self) {
        // Child owns only its synthetic windows. No foreign activation/restoration.
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if matches!(self.process.try_wait(), Ok(None)) {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
    }
}
fn spawn(output: &Path, home: &Path, emoji: bool, tab_row: bool) -> Result<EditorChild> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Nonce entropy unavailable"))?;
    let nonce: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let bundle_id = format!("org.brotypist.ownedax.{nonce}");
    let bundle = output.join("OwnedAxEditor.app");
    for path in [
        &bundle,
        &bundle.join("Contents"),
        &bundle.join("Contents/MacOS"),
    ] {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let executable = bundle.join("Contents/MacOS/OwnedAxEditor");
    fs::copy(std::env::current_exe()?, &executable)?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    write_new(&bundle.join("Contents/Info.plist"), format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string><key>CFBundleExecutable</key><string>OwnedAxEditor</string><key>CFBundleName</key><string>Owned AX Editor</string><key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/><key>NSPrincipalClass</key><string>NSApplication</string></dict></plist>"
    ).as_bytes())?;
    let (stream, child_stream) = UnixStream::pair()?;
    timeout(&stream)?;
    let parent_pid = std::process::id() as i32;
    let parent_started =
        MacBridge::owned_process_started(parent_pid).context("Parent incarnation unavailable")?;
    let runtime_home = home.join("child-home");
    let temporary = runtime_home.join("tmp");
    let cache = runtime_home.join("Library/Caches");
    let config = runtime_home.join("config");
    for path in [
        &runtime_home,
        &temporary,
        &runtime_home.join("Library"),
        &cache,
        &runtime_home.join("Library/Preferences"),
        &runtime_home.join("Library/Application Support"),
        &runtime_home.join("Library/Saved Application State"),
        &config,
    ] {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let process = ProcessCommand::new(&executable)
        .arg("--owned-ax-child")
        // No inherited DYLD, injection, service, proxy or user config variables.
        // These locations reduce ordinary writes, but are not an OS sandbox:
        // account-bound services can ignore them and use the login account.
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LANG", "en_US.UTF-8")
        .env("BROTYPIST_OWNED_DESKTOP_SMOKE", "1")
        .env("INKLOW_HOME", home)
        .env("HOME", &runtime_home)
        .env("CFFIXED_USER_HOME", &runtime_home)
        .env("TMPDIR", &temporary)
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_CONFIG_HOME", &config)
        .current_dir(&runtime_home)
        .stdin(Stdio::from(OwnedFd::from(child_stream)))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let pid = process.id() as i32;
    let mut child = EditorChild {
        process,
        stream,
        nonce: nonce.clone(),
        sequence: 0,
        observations: Vec::new(),
    };
    send(
        &mut child.stream,
        &json!({"nonce":nonce,"parent_pid":parent_pid,"parent_started":parent_started,
        "bundle_id":bundle_id,"child_pid":pid,"emoji":emoji,"tab_row":tab_row}),
    )?;
    let hello = receive(&mut child.stream)?;
    ensure!(
        hello["nonce"] == nonce && hello["pid"] == pid && hello["bundle_id"] == bundle_id,
        "Owned child handshake mismatch"
    );
    let started = MacBridge::owned_process_started(pid).context("Child incarnation unavailable")?;
    ensure!(
        hello["started"] == json!(started),
        "Child incarnation mismatch"
    );
    // Only the just-spawned, authenticated child is excluded. Never an arbitrary
    // user supplied PID, normal observer or production bundle identity.
    macos_smoke::refuse_typing_observers(Some(pid))?;
    child.request("authorize")?;
    Ok(child)
}
fn pump(bridge: &mut MacBridge) -> Result<()> {
    unsafe {
        ax::ffi::CFRunLoopRunInMode(ax::ffi::kCFRunLoopDefaultMode, 0.15, 0);
    }
    bridge.pump_events()
}
fn require_capture(bridge: &mut MacBridge, checks: &mut Value, stage: &str) -> Result<Snapshot> {
    let captured = bridge.capture();
    checks[stage] = match &captured {
        Ok(Some(snapshot)) => snapshot_receipt(snapshot),
        Ok(None) => json!({"capture":"refused","native_status":bridge.status()}),
        Err(error) => {
            json!({"capture":"error","error":format!("{error:#}"),"native_status":bridge.status()})
        }
    };
    captured?.with_context(|| format!("Actual AX capture refused: {}", bridge.status()))
}
fn snapshot_receipt(snapshot: &Snapshot) -> Value {
    json!({"text":snapshot.text.as_ref(),"caret_scalar":snapshot.caret,
        "selection_scalar":snapshot.selection_length,"field_id":snapshot.field_id,
        "window_id":snapshot.window_id,"window_pid":snapshot.window_pid,
        "can_replace":snapshot.can_replace,"can_replace_emoji":snapshot.can_replace_emoji,
        "caret_rect":snapshot.caret_rect,
        "field_rect":snapshot.field_rect,"inline_safe":snapshot.inline_safe()})
}
fn check_widget(
    response: &Value,
    field: usize,
    text: &str,
    location: usize,
    length: usize,
) -> Result<()> {
    let value = &response["widgets"][field];
    ensure!(
        value["text"] == text && value["location"] == location && value["length"] == length,
        "Independent child widget readback mismatch: {value}"
    );
    Ok(())
}
// Independent widget offsets are UTF-16; count scalar prefixes directly rather
// than using the production TextIndex conversion being exercised.
fn check_widget_mapping(response: &Value, snapshot: &Snapshot) -> Result<()> {
    let end = snapshot
        .caret
        .checked_add(snapshot.selection_length)
        .context("Captured scalar selection overflow")?;
    ensure!(
        end <= snapshot.text.chars().count(),
        "Captured scalar selection outside text"
    );
    let location: usize = snapshot
        .text
        .chars()
        .take(snapshot.caret)
        .map(char::len_utf16)
        .sum();
    let length: usize = snapshot
        .text
        .chars()
        .skip(snapshot.caret)
        .take(snapshot.selection_length)
        .map(char::len_utf16)
        .sum();
    check_widget(response, 0, snapshot.text.as_ref(), location, length)
}
fn require_rejected(
    child: &mut EditorChild,
    bridge: &mut MacBridge,
    snapshot: &Snapshot,
    checks: &mut Value,
    stage: &str,
) -> Result<Value> {
    let outcome = bridge.replace_range(snapshot, 5, 1, "🚀")?;
    checks[stage] = json!({"outcome":format!("{outcome:?}")});
    let readback = child.request("read")?;
    checks[stage]["independent_readback"] = readback.clone();
    ensure!(
        matches!(outcome, EditOutcome::Rejected),
        "{stage}: stale mutation was not refused"
    );
    Ok(readback)
}
fn exercise(child: &mut EditorChild, bridge: &mut MacBridge, checks: &mut Value) -> Result<()> {
    pump(bridge)?;
    child.request("read")?;
    let initial = require_capture(bridge, checks, "initial")?;
    let independent = child.request("read")?;
    checks["independent_initial_readback"] = independent.clone();
    ensure!(
        initial.text.as_ref() == INITIAL && initial.caret == 5 && initial.selection_length == 1,
        "AX UTF-16 to scalar selection mapping mismatch"
    );
    check_widget(&independent, 0, INITIAL, 5, 2)?;
    checks["initial_verified"] = json!(true);
    let baseline = bridge.owned_notification_counts();
    let first_revision = bridge.input_revision();
    checks["revision_before"] = json!(first_revision);

    child.request("second")?;
    pump(bridge)?;
    let second = require_capture(bridge, checks, "second_field")?;
    ensure!(
        second.text.as_ref() == SECOND
            && second.window_pid == initial.window_pid
            && second.window_id == initial.window_id
            && second.field_id != initial.field_id,
        "Same-process native field identity did not change"
    );
    checks["second_field_verified"] = json!(true);
    let readback = require_rejected(child, bridge, &initial, checks, "foreign_field_mutation")?;
    check_widget(&readback, 0, INITIAL, 5, 2)?;
    checks["foreign_field_refused"] = json!(true);

    child.request("first")?;
    pump(bridge)?;
    let before_value = require_capture(bridge, checks, "before_value")?;
    child.request("value")?;
    pump(bridge)?;
    let changed = require_capture(bridge, checks, "changed_value")?;
    ensure!(
        changed.text.as_ref() == CHANGED,
        "Fresh AX value read missed native mutation"
    );
    checks["changed_value_verified"] = json!(true);
    let readback = require_rejected(child, bridge, &before_value, checks, "stale_value_mutation")?;
    check_widget(&readback, 0, CHANGED, CHANGED.encode_utf16().count(), 0)?;
    checks["stale_value_refused"] = json!(true);
    // The collapsed caret exercises inline clipping and its repeated window
    // frame read. Neither pass shares host values with the other or its guards.
    checks["transaction_frame_reads"] = bridge.owned_frame_read_comparison()?;
    ensure!(
        checks["transaction_frame_reads"]["identical_capture"] == true,
        "Owned reuse/baseline captures differ or were refused"
    );
    checks["transaction_frame_reads"]["independent_readback"] = child.request("read")?;
    check_widget(
        &checks["transaction_frame_reads"]["independent_readback"],
        0,
        CHANGED,
        CHANGED.encode_utf16().count(),
        0,
    )?;

    child.request("reset")?;
    pump(bridge)?;
    let before_selection = require_capture(bridge, checks, "before_selection")?;
    child.request("selection")?;
    pump(bridge)?;
    let interior_readback = child.request("read")?;
    checks["interior_selection_readback"] = interior_readback.clone();
    let interior_exact = interior_readback["widgets"][0]["location"] == 9
        && interior_readback["widgets"][0]["length"] == 1;
    checks["interior_selection_support"] = json!({
        "requested_utf16":{"location":9,"length":1},
        "status":if interior_exact { "exact" } else { "native_snapped_or_refused" },
        "actual_widget":interior_readback["widgets"][0],
        "exact_requested_selection_supported":interior_exact,
    });
    let interior = require_capture(bridge, checks, "interior_selection")?;
    ensure!(
        interior.text.as_ref() == INITIAL,
        "Interior selection probe changed authored text"
    );
    check_widget_mapping(&interior_readback, &interior)?;
    checks["interior_actual_mapping_verified"] = json!(true);

    // UTF-16 9 is inside e + U+0301. NSTextView may expand that request
    // to the composed sequence. Exercise exact supported outer boundaries
    // separately, without normalizing text or claiming the interior succeeded.
    child.request("selection-cluster")?;
    pump(bridge)?;
    let selection_readback = child.request("read")?;
    checks["combining_selection_readback"] = selection_readback.clone();
    let selected = require_capture(bridge, checks, "combining_selection")?;
    check_widget(&selection_readback, 0, INITIAL, 8, 2)?;
    ensure!(
        selected.text.as_ref() == INITIAL && selected.caret == 7 && selected.selection_length == 2,
        "Whole combining-sequence native selection mapping mismatch"
    );
    checks["combining_selection_verified"] = json!(true);
    let readback = require_rejected(
        child,
        bridge,
        &before_selection,
        checks,
        "stale_selection_mutation",
    )?;
    check_widget(&readback, 0, INITIAL, 8, 2)?;
    checks["stale_selection_refused"] = json!(true);

    let old_geometry = selected.field_rect;
    child.request("geometry")?;
    pump(bridge)?;
    let geometry = require_capture(bridge, checks, "geometry")?;
    let geometry_changed = old_geometry.is_some()
        && geometry.field_rect.is_some()
        && old_geometry != geometry.field_rect;
    checks["geometry_changed"] = json!(geometry_changed);

    child.request("reset")?;
    pump(bridge)?;
    let replacement_before = require_capture(bridge, checks, "replacement_before")?;
    if replacement_before.can_replace {
        child.request("read")?;
        let outcome = bridge.replace_range(&replacement_before, 5, 1, "🚀")?;
        checks["native_replacement"] = json!({"outcome":format!("{outcome:?}")});
        let readback = child.request("read")?;
        checks["native_replacement"]["independent_readback"] = readback.clone();
        match &outcome {
            EditOutcome::Applied(after) => {
                checks["native_replacement"]["after"] = snapshot_receipt(after);
                check_widget(&readback, 0, REPLACED, 7, 0)?;
                ensure!(
                    after.text.as_ref() == REPLACED
                        && after.caret == 6
                        && after.selection_length == 0,
                    "Native replacement confirmation mismatch"
                );
                checks["native_replacement"]["status"] = json!("applied");
                let readback = require_rejected(
                    child,
                    bridge,
                    &replacement_before,
                    checks,
                    "repeated_old_mutation",
                )?;
                check_widget(&readback, 0, REPLACED, 7, 0)?;
                checks["native_replacement"]["repeated_old_snapshot_refused"] = json!(true);
            }
            EditOutcome::Rejected => {
                check_widget(&readback, 0, INITIAL, 5, 2)?;
                checks["native_replacement"]["status"] = json!("unsupported_or_refused");
            }
            _ => anyhow::bail!(
                "AX replacement was indeterminate/dispatched; never retried: {readback}"
            ),
        }
    } else {
        checks["native_replacement"] =
            json!({"status":"unsupported","reason":"AXSelectedText capability not advertised"});
    }

    child.request("readonly")?;
    pump(bridge)?;
    let readonly = bridge.capture()?;
    checks["readonly_capture"] = readonly
        .as_ref()
        .map(snapshot_receipt)
        .unwrap_or(Value::Null);
    ensure!(
        readonly.is_none(),
        "Read-only native field was captured as writable"
    );
    checks["readonly_refused"] = json!(true);
    let before_foreign = child.request("read")?;
    child.request("foreign-window")?;
    pump(bridge)?;
    let foreign = bridge.capture()?;
    checks["foreign_window_capture"] = foreign
        .as_ref()
        .map(snapshot_receipt)
        .unwrap_or(Value::Null);
    let foreground = bridge.foreground()?;
    checks["foreign_window_foreground"] = json!(foreground);
    ensure!(
        foreign.is_none() && foreground.is_none(),
        "Different owned window escaped the immutable enrollment fence"
    );
    let after_foreign = require_rejected(
        child,
        bridge,
        &replacement_before,
        checks,
        "foreign_window_mutation",
    )?;
    ensure!(
        before_foreign["widgets"] == after_foreign["widgets"],
        "Foreign-window refusal changed owned text"
    );
    checks["foreign_window_refused"] = json!(true);
    let counts = bridge.owned_notification_counts();
    let delta: [u64; 4] = std::array::from_fn(|i| counts[i].saturating_sub(baseline[i]));
    checks["notifications"] = json!({"order":["value","selection","geometry","focus"],"delta":delta,
        "all_observed":delta.iter().all(|count| *count > 0),
        "zero_means_unobserved_or_unsupported_not_acceptance":true,"synthetically_posted":false});
    checks["revision_after"] = json!(bridge.input_revision());
    checks["complete_native_acceptance"] = json!(
        geometry_changed
            && delta.iter().all(|count| *count > 0)
            && checks["native_replacement"]["status"] == "applied"
    );
    Ok(())
}

fn emoji_metadata(child: &EditorChild) -> Result<Value> {
    let app = ax::AxElement::application(child.process.id() as i32)
        .context("Owned AX application unavailable")?;
    let field = app
        .element("AXFocusedUIElement")
        .context("Owned AX field unavailable")?;
    ensure!(
        field.pid() == Some(child.process.id() as i32),
        "Foreign AX field refused"
    );
    let chrome_absent = field.attribute("ChromeAXNodeId")?.is_none();
    let metadata = json!({
        "role":field.string("AXRole"),
        "selected_text_writable":field.settable("AXSelectedText"),
        "selected_range_writable":field.settable("AXSelectedTextRange"),
        "value_writable":field.settable("AXValue"),
        "chromium_marker_absent":chrome_absent,
    });
    Ok(metadata)
}

fn exercise_emoji(
    child: &mut EditorChild,
    bridge: &mut MacBridge,
    checks: &mut Value,
) -> Result<()> {
    checks["fixture"] = json!({"alias":EMOJI_ALIAS,"emoticon":EMOJI_EMOTICON,
        "zwj_result":EMOJI_ZWJ_RESULT,"face_result":EMOJI_FACE_RESULT,
        "scope":"source-authored text only; no host text inspected",
        "readback_deadline_ms":3000,"stability_readbacks":3});
    checks["cases"] = json!({});
    for (case, source, glyph, expected, length, caret16, caret_scalar) in [
        ("alias", EMOJI_ALIAS, "👩‍💻", EMOJI_ZWJ_RESULT, 7, 11, 8),
        ("emoticon", EMOJI_EMOTICON, "🙂", EMOJI_FACE_RESULT, 3, 8, 6),
    ] {
        let row = &mut checks["cases"][case];
        *row = json!({"completed":false});
        let seeded = child.request(if case == "alias" {
            "emoji-alias"
        } else {
            "emoji-emoticon"
        })?;
        row["seeded"] = seeded.clone();
        check_widget(&seeded, 0, source, 6 + length, 0)?;
        pump(bridge)?;
        let before = require_capture(bridge, row, "before")?;
        row["native_metadata"] = emoji_metadata(child)?;
        let metadata = &row["native_metadata"];
        ensure!(
            metadata["role"] == "AXTextArea"
                && metadata["selected_text_writable"] == false
                && metadata["selected_range_writable"] == true
                && metadata["value_writable"] == true
                && metadata["chromium_marker_absent"] == true,
            "Actual native emoji fixture capabilities differ: {metadata}"
        );
        ensure!(
            before.text.as_ref() == source
                && before.caret == 5 + length
                && before.selection_length == 0
                && !before.can_replace
                && before.can_replace_emoji,
            "Emoji-only native capture differs from fixture"
        );
        let generic = bridge.replace_range(&before, 5, length, glyph)?;
        row["generic_outcome"] = json!(format!("{generic:?}"));
        let unchanged = child.request("read")?;
        row["generic_readback"] = unchanged.clone();
        ensure!(
            matches!(generic, EditOutcome::Rejected),
            "Generic replacement escaped emoji lane"
        );
        ensure!(
            unchanged["widgets"] == seeded["widgets"],
            "Generic refusal changed widget"
        );

        let armed = child.request("emoji-arm")?;
        row["armed"] = armed.clone();
        let changes = armed["widgets"][0]["text_changes"]
            .as_u64()
            .context("Missing native edit counter")?;
        // Exactly one invocation, including when AppKit delivery is asynchronous.
        let outcome = bridge.replace_emoji_range(&before, 5, length, glyph)?;
        row["outcome"] = json!(format!("{outcome:?}"));
        row["dispatch_calls"] = json!(1);
        if let EditOutcome::Applied(after) = &outcome {
            row["applied"] = snapshot_receipt(after);
            ensure!(
                after.text.as_ref() == expected
                    && after.caret == caret_scalar
                    && after.selection_length == 0,
                "Applied result was not exact"
            );
        }
        row["readbacks"] = json!([]);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            pump(bridge)?;
            let readback = child.request("read")?;
            row["readbacks"]
                .as_array_mut()
                .unwrap()
                .push(readback.clone());
            ensure!(
                matches!(&outcome, EditOutcome::Applied(_) | EditOutcome::Dispatched),
                "Native emoji replacement did not apply/dispatch; never retry"
            );
            if readback["widgets"][0]["text"] == expected
                && readback["emoji_events"]["key_down"] == 1
                && readback["emoji_events"]["key_up"] == 1
            {
                check_widget(&readback, 0, expected, caret16, 0)?;
                ensure!(
                    readback["widgets"][0]["text_changes"] == changes + 1,
                    "Native widget did not record exactly one text change"
                );
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "Bounded emoji delivery timed out; never redispatch"
            );
        }
        let after = require_capture(bridge, row, "confirmed")?;
        ensure!(
            after.text.as_ref() == expected
                && after.caret == caret_scalar
                && after.selection_length == 0
                && after.field_id == before.field_id
                && after.window_id == before.window_id
                && after.window_pid == before.window_pid
                && after.can_replace_emoji
                && !after.can_replace,
            "Final AX confirmation differs"
        );
        // A stale invocation must refuse rather than post again.
        let repeated = bridge.replace_emoji_range(&before, 5, length, glyph)?;
        row["stale_repeat_outcome"] = json!(format!("{repeated:?}"));
        ensure!(
            matches!(repeated, EditOutcome::Rejected),
            "Old emoji snapshot replay not refused"
        );
        row["stability_readbacks"] = json!([]);
        for _ in 0..3 {
            pump(bridge)?;
            let readback = child.request("read")?;
            row["stability_readbacks"]
                .as_array_mut()
                .unwrap()
                .push(readback.clone());
            check_widget(&readback, 0, expected, caret16, 0)?;
            ensure!(
                readback["widgets"][0]["text_changes"] == changes + 1
                    && readback["emoji_events"]["key_down"] == 1
                    && readback["emoji_events"]["key_up"] == 1,
                "Duplicate native mutation"
            );
        }
        child.request("emoji-disarm")?;
        row["completed"] = json!(true);
    }
    // Real responder/capability/window changes, all inside the same private child.
    let seed = child.request("emoji-alias")?;
    pump(bridge)?;
    let before = require_capture(bridge, checks, "before_refusals")?;
    checks["refusals"] = json!({});
    for operation in ["second", "readonly", "foreign-window"] {
        if operation == "readonly" {
            child.request("first")?;
            pump(bridge)?;
        }
        child.request(operation)?;
        pump(bridge)?;
        let outcome = bridge.replace_emoji_range(&before, 5, 7, "👩‍💻")?;
        let observed = child.request("read")?;
        checks["refusals"][operation] =
            json!({"outcome":format!("{outcome:?}"),"readback":observed});
        ensure!(
            matches!(outcome, EditOutcome::Rejected),
            "{operation}: emoji mutation not refused"
        );
        ensure!(
            observed["widgets"] == seed["widgets"],
            "{operation}: refusal changed widget"
        );
        let captured = bridge.capture()?;
        checks["refusals"][operation]["capture"] = captured
            .as_ref()
            .map(snapshot_receipt)
            .unwrap_or(Value::Null);
        if operation == "second" {
            let second = captured.context("Owned second field capture unavailable")?;
            ensure!(
                second.field_id != before.field_id && !second.can_replace_emoji,
                "Second field inherited enrolled emoji capability"
            );
        } else {
            ensure!(
                captured.is_none(),
                "{operation}: emoji enrollment accepted changed ownership/capability"
            );
        }
    }
    checks["complete_native_acceptance"] = json!(true);
    Ok(())
}
// Native view setters only: authored source fixtures, never input events.
fn tab_row_case(name: &str) -> Option<(&'static str, f64, f64)> {
    Some(match name {
        "row-baseline" => ("Native row calibration", 240.0, 680.0),
        "row-tab-near" => ("Native row calibration\t", 240.0, 680.0),
        "row-tab-far" => ("Native row calibration\t", 360.0, 680.0),
        "row-gap" => ("Native row calibration\t  ", 240.0, 680.0),
        "row-nfd" => ("e\u{301} Native row calibration\t", 360.0, 680.0),
        "row-unicode" => ("𝛼 e\u{301} Native row calibration\t", 360.0, 680.0),
        "row-wrap" => ("Native row calibration\t", 360.0, 180.0),
        "row-style" => ("Native row calibration\t", 240.0, 680.0),
        "row-suffix" => ("Native row calibration\tend", 240.0, 680.0),
        "row-hardline" => ("Native row calibration\n\t", 240.0, 680.0),
        _ => return None,
    })
}

fn seed_tab_row(editor: &AnyObject, name: &str) -> Result<NSRange> {
    let (text, stop, width) = tab_row_case(name).context("Unknown row fixture")?;
    unsafe {
        let font: *mut AnyObject = msg_send![AnyClass::get(c"NSFont").unwrap(),
            fontWithName:&*NSString::from_str("Helvetica"), size:16.0f64];
        ensure!(
            !font.is_null(),
            "Native Helvetica unavailable; no substitute font"
        );
        let paragraph = obj("NSMutableParagraphStyle");
        let stops = obj("NSMutableArray");
        let allocated: *mut AnyObject = msg_send![AnyClass::get(c"NSTextTab").unwrap(), alloc];
        let tab: *mut AnyObject = msg_send![allocated, initWithType:0usize, location:stop];
        let tab = Retained::from_raw(tab).context("Native text tab unavailable")?;
        let _: () = msg_send![&*stops, addObject:&*tab];
        let _: () = msg_send![&*paragraph, setTabStops:&*stops];
        let _: () = msg_send![&*paragraph, setDefaultTabInterval:stop];
        let _: () = msg_send![editor, setRichText:true];
        let _: () = msg_send![editor, setFrame:rect(20.0,170.0,width,110.0)];
        let _: () = msg_send![editor, setDefaultParagraphStyle:&*paragraph];
        set_string(editor, text);
        let _: () = msg_send![editor, setFont:font];
        let storage: *mut AnyObject = msg_send![editor, textStorage];
        ensure!(!storage.is_null(), "Native text storage unavailable");
        let full = NSRange::new(0, text.encode_utf16().count());
        let _: () = msg_send![storage, addAttribute:&*NSString::from_str("NSParagraphStyle"),
            value:&*paragraph, range:full];
        if name == "row-style" {
            let changed: *mut AnyObject = msg_send![AnyClass::get(c"NSFont").unwrap(),
                fontWithName:&*NSString::from_str("Helvetica"), size:22.0f64];
            ensure!(!changed.is_null(), "Native changed font unavailable");
            let _: () = msg_send![storage, addAttribute:&*NSString::from_str("NSFont"),
                value:changed, range:NSRange::new(full.length - 1, 1)];
        }
        let selection = NSRange::new(
            if name == "row-suffix" {
                "Native row calibration".len()
            } else {
                full.length
            },
            0,
        );
        let _: () = msg_send![editor, setSelectedRange:selection];
        let layout: *mut AnyObject = msg_send![editor, layoutManager];
        let container: *mut AnyObject = msg_send![editor, textContainer];
        ensure!(
            !layout.is_null() && !container.is_null(),
            "Native layout unavailable"
        );
        let _: () = msg_send![layout, ensureLayoutForTextContainer:container];
        let _: () = msg_send![editor, displayIfNeeded];
        Ok(selection)
    }
}

// Independent NSTextInputClient/NSLayoutManager readback, not AXBoundsForRange.
// Convert using the primary native screen, never an estimated text/tab advance.
fn tab_row_receipt(editor: &AnyObject) -> Result<Value> {
    unsafe {
        let text = string(editor);
        let selection: NSRange = msg_send![editor, selectedRange];
        let mut units = 0;
        let prefix: String = text
            .chars()
            .take_while(|ch| {
                units += ch.len_utf16();
                units <= selection.location
            })
            .collect();
        let sample_end = prefix
            .trim_end_matches(char::is_whitespace)
            .encode_utf16()
            .count();
        let screens: *mut AnyObject = msg_send![AnyClass::get(c"NSScreen").unwrap(), screens];
        ensure!(!screens.is_null(), "Native screen list unavailable");
        let count: usize = msg_send![screens, count];
        ensure!(count > 0, "Native screen unavailable");
        let primary: *mut AnyObject = msg_send![screens, objectAtIndex:0usize];
        let screen: NSRect = msg_send![primary, frame];
        let screen_top = screen.origin.y + screen.size.height;
        let native_rect = |offset: usize| -> Value {
            let mut actual = NSRange::new(usize::MAX, 0);
            let value: NSRect = msg_send![editor,
                firstRectForCharacterRange:NSRange::new(offset, 0),
                actualRange:&mut actual as *mut NSRange];
            json!({"requested_utf16":offset,
                "actual_utf16":{"location":actual.location,"length":actual.length},
                "appkit_rect":[value.origin.x,value.origin.y,value.size.width,value.size.height],
                "ax_coordinates":[value.origin.x,screen_top-value.origin.y-value.size.height,
                    value.size.width,value.size.height]})
        };
        let storage: *mut AnyObject = msg_send![editor, textStorage];
        ensure!(
            !storage.is_null() && sample_end > 0,
            "Native sample storage unavailable"
        );
        let font_at = |offset: usize| -> Result<Value> {
            let mut range = NSRange::new(usize::MAX, 0);
            let font: *mut AnyObject = msg_send![storage,
                attribute:&*NSString::from_str("NSFont"), atIndex:offset,
                effectiveRange:&mut range as *mut NSRange];
            ensure!(!font.is_null(), "Native font attribute unavailable");
            let name: *mut NSString = msg_send![font, fontName];
            ensure!(!name.is_null(), "Native font name unavailable");
            let size: f64 = msg_send![font, pointSize];
            let coverage: *mut AnyObject = msg_send![font, coveredCharacterSet];
            ensure!(!coverage.is_null(), "Native font coverage unavailable");
            let covered = prefix.chars().filter(|ch| !ch.is_whitespace()).all(|ch| {
                let present: bool = msg_send![coverage, longCharacterIsMember:ch as u32];
                present
            });
            Ok(
                json!({"name":(&*name).to_string(),"size":size,"prefix_covered":covered,
                "effective_utf16":{"location":range.location,"length":range.length}}),
            )
        };
        let layout: *mut AnyObject = msg_send![editor, layoutManager];
        ensure!(!layout.is_null(), "Native layout manager unavailable");
        let glyph: usize = msg_send![layout, glyphIndexForCharacterAtIndex:sample_end - 1];
        let fragment: NSRect = msg_send![layout, lineFragmentRectForGlyphAtIndex:glyph,
            effectiveRange:std::ptr::null_mut::<NSRange>()];
        let location: NSPoint = msg_send![layout, locationForGlyphAtIndex:glyph];
        let origin: NSPoint = msg_send![editor, textContainerOrigin];
        let baseline = NSPoint::new(
            origin.x + fragment.origin.x + location.x,
            origin.y + fragment.origin.y + location.y,
        );
        let in_window: NSPoint = msg_send![editor, convertPoint:baseline,
            toView:std::ptr::null_mut::<AnyObject>()];
        let window: *mut AnyObject = msg_send![editor, window];
        ensure!(!window.is_null(), "Owned native window unavailable");
        let in_screen: NSPoint = msg_send![window, convertPointToScreen:in_window];
        let paragraph: *mut AnyObject = msg_send![editor, defaultParagraphStyle];
        ensure!(!paragraph.is_null(), "Native paragraph style unavailable");
        let stops: *mut AnyObject = msg_send![paragraph, tabStops];
        ensure!(!stops.is_null(), "Native tab stops unavailable");
        let count: usize = msg_send![stops, count];
        let mut locations = Vec::with_capacity(count);
        for index in 0..count {
            let tab: *mut AnyObject = msg_send![stops, objectAtIndex:index];
            let location: f64 = msg_send![tab, location];
            locations.push(location);
        }
        Ok(
            json!({"caret":native_rect(selection.location),"sample_end":native_rect(sample_end),
            "baseline_ax":screen_top-in_screen.y,"sample_font":font_at(sample_end-1)?,
            "caret_font":font_at(selection.location.saturating_sub(1))?,
            "tab_stops":locations,"coordinate_system":"primary-screen AX top-left"}),
        )
    }
}

fn row_rect(value: &Value) -> Result<[f64; 4]> {
    let rect: [f64; 4] = serde_json::from_value(value["ax_coordinates"].clone())
        .context("Native widget rect unavailable")?;
    let requested = value["requested_utf16"]
        .as_u64()
        .context("Missing native offset")?;
    let actual = value["actual_utf16"]["location"]
        .as_u64()
        .context("Missing actual offset")?;
    let length = value["actual_utf16"]["length"]
        .as_u64()
        .context("Missing actual length")?;
    ensure!(
        actual <= requested
            && actual
                .checked_add(length)
                .is_some_and(|end| requested <= end),
        "Native rect did not cover requested caret"
    );
    ensure!(
        rect.iter().all(|v| v.is_finite()) && rect[2] >= 0.0 && rect[3] > 0.0,
        "Native widget returned invalid geometry"
    );
    Ok(rect)
}

// Raw AX observations use the authenticated native-smoke seam, not range_rect:
// that ordinary API refuses zero-length ranges before making an AX query.
fn tab_row_bounds(bridge: &mut MacBridge, snapshot: &Snapshot) -> Result<Value> {
    let sample_end = snapshot
        .text
        .chars()
        .take(snapshot.caret)
        .enumerate()
        .filter(|(_, ch)| !ch.is_whitespace())
        .map(|(index, _)| index + 1)
        .last()
        .unwrap_or(0);
    bridge.owned_tab_row_bounds(snapshot, sample_end)
}

fn exercise_tab_row(
    child: &mut EditorChild,
    bridge: &mut MacBridge,
    checks: &mut Value,
) -> Result<()> {
    let cases = [
        "row-baseline",
        "row-tab-near",
        "row-tab-far",
        "row-gap",
        "row-nfd",
        "row-unicode",
        "row-wrap",
        "row-style",
        "row-suffix",
        "row-hardline",
    ];
    for name in cases {
        checks[name] = json!({"status":"not_run"});
    }
    let mut near_caret = None;
    for name in cases {
        checks[name]["status"] = json!("started");
        let seeded = child.request(name)?;
        checks[name]["seed_readback"] = seeded;
        pump(bridge)?;
        let snapshot = require_capture(bridge, &mut checks[name], "capture")?;
        checks[name]["native_status"] = json!(bridge.status());
        checks[name]["inline_render"] = json!(snapshot.inline_render);
        let raw_bounds = tab_row_bounds(bridge, &snapshot);
        checks[name]["raw_owned_ax_bounds"] = match &raw_bounds {
            Ok(receipt) => receipt.clone(),
            Err(error) => json!({"status":"raw_bounds_failed","error":format!("{error:#}")}),
        };
        raw_bounds?;
        let readback = child.request("read")?;
        checks[name]["independent_readback"] = readback.clone();
        check_widget_mapping(&readback, &snapshot)?;
        let (expected, stop, _) = tab_row_case(name).context("Missing authored case")?;
        ensure!(
            snapshot.text.as_ref() == expected && snapshot.selection_length == 0,
            "{name}: source text or native selection changed"
        );
        let expected_caret = if name == "row-suffix" {
            "Native row calibration".chars().count()
        } else {
            expected.chars().count()
        };
        ensure!(
            snapshot.caret == expected_caret,
            "{name}: native caret moved"
        );
        let native = &readback["tab_row"];
        ensure!(
            native["tab_stops"] == json!([stop]),
            "{name}: native tab stop not installed"
        );
        let caret = row_rect(&native["caret"])?;
        let sample = row_rect(&native["sample_end"])?;
        let same_row = (caret[1] - sample[1]).abs() <= 1.0 && (caret[3] - sample[3]).abs() <= 1.0;
        checks[name]["independent_same_row"] = json!(same_row);
        if matches!(name, "row-nfd" | "row-unicode")
            && native["sample_font"]["prefix_covered"] != true
        {
            checks[name]["status"] = json!("not_run_font_coverage");
            continue;
        }
        if name == "row-wrap" && same_row {
            checks[name]["status"] = json!("not_run_native_did_not_wrap_gap");
            continue;
        }
        if matches!(
            name,
            "row-wrap" | "row-style" | "row-suffix" | "row-hardline"
        ) {
            if name == "row-style" {
                ensure!(
                    native["sample_font"]["size"] == 16.0 && native["caret_font"]["size"] == 22.0,
                    "Native gap style-change precondition unavailable"
                );
            }
            ensure!(
                !snapshot.inline_safe() && snapshot.inline_render.is_none(),
                "{name}: unproved row/style/suffix was accepted"
            );
            checks[name]["status"] = json!("native_refusal_verified");
            continue;
        }
        ensure!(
            same_row,
            "{name}: native sample and actual caret are not on one row"
        );
        ensure!(
            snapshot.inline_safe(),
            "{name}: actual direct AX inline evidence unavailable"
        );
        let render = snapshot
            .inline_render
            .as_ref()
            .context("No actual InlineRender")?;
        ensure!(
            render.source == "macos-ax-attributed-run",
            "{name}: not direct attributed AX evidence: {}",
            render.source
        );
        ensure!(
            snapshot.field_rect.is_some() && snapshot.window_bounds.is_some(),
            "{name}: native field/window geometry unavailable"
        );
        let captured = snapshot.caret_rect.context("Actual AX caret unavailable")?;
        ensure!(
            captured
                .iter()
                .zip(caret)
                .all(|(a, b)| (a - b).abs() <= 1.5),
            "{name}: AX origin/rect differs from independently queried native caret"
        );
        ensure!(
            native["sample_font"]["name"] == render.font_name
                && native["caret_font"]["name"] == render.font_name
                && native["sample_font"]["size"]
                    .as_f64()
                    .is_some_and(|size| (size - render.font_size).abs() <= 0.01)
                && native["caret_font"]["size"] == native["sample_font"]["size"],
            "{name}: native typography does not match captured render"
        );
        let baseline = native["baseline_ax"]
            .as_f64()
            .context("Native baseline unavailable")?;
        ensure!(
            baseline.is_finite() && (render.baseline - baseline).abs() <= 1.5,
            "{name}: captured baseline differs from native glyph layout"
        );
        if name != "row-baseline" {
            ensure!(
                caret[0] > sample[0] + 1.5,
                "{name}: no independently measured native trailing-gap advance"
            );
        }
        if name == "row-tab-near" {
            near_caret = Some(caret);
        } else if name == "row-tab-far" {
            let near = near_caret.context("Near-stop proof missing")?;
            ensure!(
                caret[0] > near[0] + 1.5 && (caret[1] - near[1]).abs() <= 1.0,
                "Real paragraph tab stop change did not move the actual caret on the same row"
            );
        }
        checks[name]["status"] = json!("direct_native_row_verified");
    }
    checks["fixture"]["status"] = json!("completed_with_explicit_coverage");
    checks["complete_native_acceptance"] = json!(cases.iter().all(|name| matches!(
        checks[*name]["status"].as_str(),
        Some("direct_native_row_verified" | "native_refusal_verified")
    )));
    Ok(())
}

pub(super) fn run(output: &Path) -> Result<i32> {
    run_mode(output, false, false)
}
pub(super) fn run_emoji(output: &Path) -> Result<i32> {
    run_mode(output, true, false)
}
pub(super) fn run_tab_row(output: &Path) -> Result<i32> {
    run_mode(output, false, true)
}
fn run_mode(output: &Path, emoji: bool, tab_row: bool) -> Result<i32> {
    let paths = private_output(output)?;
    let home = &paths.home;
    let output = paths.output.as_path();
    let mut owned_child = None;
    let mut enrolled = false;
    let mut acceptance = json!({"checks":{
        "complete_native_acceptance":false,
        "fixture":{"text":INITIAL,"utf8":INITIAL.as_bytes(),
            "utf16":INITIAL.encode_utf16().collect::<Vec<_>>()},
        "secure_field":"not exercised: do not toggle global Secure Input in a shared desktop",
        "fresh_reads":"actual transaction-scoped MacBridge recapture and independent child widgets",
    }});
    if emoji {
        acceptance["checks"]["fixture"] = json!({"alias":EMOJI_ALIAS,"emoticon":EMOJI_EMOTICON,
            "zwj_result":EMOJI_ZWJ_RESULT,"face_result":EMOJI_FACE_RESULT});
    }
    if tab_row {
        acceptance["checks"]["fixture"] = json!({
            "source_only":true,"font":"Helvetica","size":16.0,
            "baseline_required_before_tabs":true,
            "positive_cases":["row-baseline","row-tab-near","row-tab-far","row-gap","row-nfd","row-unicode"],
            "negative_cases":["row-wrap","row-style","row-suffix","row-hardline"],
            "status":"not_run"
        });
    }
    let result = (|| -> Result<()> {
        ensure!(
            ax::trusted(),
            "Existing AX trust required for this observer executable; no permission prompt"
        );
        owned_child = Some(spawn(output, home, emoji, tab_row)?);
        let child = owned_child.as_mut().context("Owned child unavailable")?;
        let pid = child.process.id() as i32;
        let started = MacBridge::owned_process_started(pid).context("Owned process disappeared")?;
        if emoji {
            acceptance["checks"]["enrollment_metadata"] = emoji_metadata(child)?;
        }
        let mut bridge = if emoji {
            MacBridge::new_owned_emoji(pid, started, child.nonce.clone())?
        } else {
            MacBridge::new_owned(pid, started, child.nonce.clone())?
        };
        enrolled = true;
        let hello = child.request("read")?;
        let app_id = hello["bundle_id"]
            .as_str()
            .context("Owned bundle identity missing")?;
        let mut config = Config {
            enabled: false,
            global_enabled: true,
            launch_at_login: false,
            check_updates_automatically: false,
            clipboard_context: false,
            screen_context: false,
            screenshot_context: false,
            screenshot_appearance: false,
            terminal_completion: false,
            record_history: false,
            use_history: false,
            record_unaccepted: false,
            personalization_strength: 0,
            statistics_enabled: false,
            ..Config::default()
        };
        config.app_rules.clear();
        config.app_rules.insert(
            format!("app:{app_id}"),
            BTreeMap::from([
                ("enabled".into(), json!(true)),
                ("improve_compatibility".into(), json!(false)),
            ]),
        );
        bridge.configure(&config)?;
        let result = if emoji {
            exercise_emoji(child, &mut bridge, &mut acceptance["checks"])
        } else if tab_row {
            exercise_tab_row(child, &mut bridge, &mut acceptance["checks"])
        } else {
            exercise(child, &mut bridge, &mut acceptance["checks"])
        };
        acceptance["native_status"] = json!(bridge.status());
        let stopped = bridge.stop();
        acceptance["stop_error"] = json!(stopped.as_ref().err().map(|error| format!("{error:#}")));
        result?;
        stopped?;
        Ok(())
    })();
    // Acknowledge owned close if the child still serves; never reclaim focus.
    let close = owned_child
        .as_mut()
        .map(|child| child.request("close"))
        .transpose();
    acceptance["protocol_observations"] =
        json!(owned_child.as_ref().map(|child| &child.observations));
    let incomplete_tab_row = tab_row && acceptance["checks"]["complete_native_acceptance"] != true;
    let mut receipt = json!({"schema":if emoji {"brotypist-owned-emoji/1"} else if tab_row {"brotypist-owned-tab-row/1"} else {"brotypist-owned-external-ax/1"},"passed":result.is_ok() && close.is_ok() && !incomplete_tab_row,
        "scope":if emoji {"real MacBridge emoji transaction and independent AppKit component readback only; not WhatsApp, chooser/Controller, physical typing or global hooks"} else if tab_row {"real MacBridge direct AX geometry and independent owned NSTextView readback; no keyboard posting, model, WhatsApp, Controller or rapid-Tab guard proof"} else {"actual hookless MacBridge capture/direct AX mutation, not Controller, global hooks, physical typing, browser, terminal or screen-reader acceptance"},
        "safety":{"owned_pid_incarnation_window_nonce":enrolled,"private_authenticated_child":owned_child.is_some(),
            "known_observer_preflight":true,"fresh_private_profile":true,"text_assistance_disabled":owned_child.is_some(),
            "global_hooks":false,"keyboard_injection":emoji,"global_keyboard_posting":false,
            "input_scope":if emoji {"fixed source glyphs to enrolled child PID only"} else {"no keyboard posting"},
            "permission_requests":false,
            "clipboard":false,"history":false,"model":false,"application_normal_profile_access":false,
            "child_environment":"cleared; fixed PATH/LANG/authorization plus canonical private INKLOW_HOME/HOME/CFFIXED_USER_HOME/TMPDIR/TMP/TEMP/XDG_CACHE_HOME/XDG_CONFIG_HOME",
            "window_restoration":"process-local volatile defaults and non-restorable windows",
            "filesystem_policy":"canonical root/current-UID-owned chains; no extended ACL; group/world writable ancestors require sticky bit; root and same UID trusted",
            "os_framework_write_confinement":false,
            "remaining_os_side_effect_limit":"AppKit/Foundation, LaunchServices, cfprefsd and text-service daemons may write account-wide preferences, caches, registration or service state outside these directories; HOME is not a sandbox. No host-wide no-write claim. Use a genuinely disposable macOS login account when normal-account side effects are unacceptable."},
        "acceptance":acceptance,"error":result.as_ref().err().map(|error|format!("{error:#}")),
        "owned_close":owned_child.is_some() && close.is_ok(),"close_error":close.as_ref().err().map(|error|format!("{error:#}"))});
    if tab_row {
        receipt["outcome"] = json!(if result.is_err() || close.is_err() {
            "failed"
        } else if incomplete_tab_row {
            "partial_native_coverage"
        } else {
            "complete_native_acceptance"
        });
    }
    write_new(
        &output.join("receipt.json"),
        &serde_json::to_vec_pretty(&receipt)?,
    )?;
    result?;
    close?;
    // Missing native coverage is not a pass, even when cleanup succeeded.
    Ok(if incomplete_tab_row { 2 } else { 0 })
}

struct EmojiEditorIvars {
    changes: Cell<u64>,
}
define_class!(
    #[unsafe(super = NSTextView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = EmojiEditorIvars]
    struct OwnedEmojiTextView;
    unsafe impl NSObjectProtocol for OwnedEmojiTextView {}
    impl OwnedEmojiTextView {
        // Native accessibility policy, not a bridge capability override.
        // Ordinary keyboard editing remains NSTextView's implementation.
        #[unsafe(method(accessibilityIsAttributeSettable:))]
        fn attribute_settable(&self, attribute: &NSString) -> bool {
            if attribute.to_string() == "AXSelectedText" {
                false
            } else {
                unsafe { msg_send![super(self), accessibilityIsAttributeSettable:attribute] }
            }
        }
        #[unsafe(method(isAccessibilitySelectorAllowed:))]
        fn selector_allowed(&self, selector: Sel) -> bool {
            if selector == sel!(setAccessibilitySelectedText:) {
                false
            } else {
                unsafe { msg_send![super(self), isAccessibilitySelectorAllowed:selector] }
            }
        }
        #[unsafe(method(accessibilitySetValue:forAttribute:))]
        fn set_attribute(&self, value: &AnyObject, attribute: &NSString) {
            if attribute.to_string() != "AXSelectedText" {
                unsafe { let _: () = msg_send![super(self), accessibilitySetValue:value, forAttribute:attribute]; }
            }
        }
        #[unsafe(method(didChangeText))]
        fn did_change_text(&self) {
            self.ivars().changes.set(self.ivars().changes.get().saturating_add(1));
            unsafe { let _: () = msg_send![super(self), didChangeText]; }
        }
        #[unsafe(method(ownedEmojiTextChanges))]
        fn text_changes(&self) -> u64 {
            self.ivars().changes.get()
        }
    }
);
fn emoji_editor(mtm: MainThreadMarker) -> Obj {
    let this = OwnedEmojiTextView::alloc(mtm).set_ivars(EmojiEditorIvars {
        changes: Cell::new(0),
    });
    let editor: Retained<OwnedEmojiTextView> =
        unsafe { msg_send![super(this), initWithFrame:rect(20.,170.,680.,110.)] };
    unsafe {
        let _: () = msg_send![&*editor,setRichText:false];
        let _: () = msg_send![&*editor,setAllowsUndo:false];
        let _: () = msg_send![&*editor,setHorizontallyResizable:false];
    }
    identify(
        &editor,
        "owned.ax.first",
        "Readonly selected-text emoji fixture",
    );
    unsafe { Retained::from_raw(Retained::into_raw(editor).cast::<AnyObject>()).unwrap() }
}

#[derive(Default)]
struct EmojiEvents {
    glyph: Option<&'static str>,
    armed_at: Option<Instant>,
    key_down: usize,
    key_up: usize,
}
impl EmojiEvents {
    fn receipt(&self) -> Value {
        json!({"armed":self.glyph.is_some(),"key_down":self.key_down,"key_up":self.key_up})
    }
}
// The original owned-AX event pump remains untouched. This opt-in child accepts
// only one marked, parent-originated, child-targeted pair for its armed glyph.
fn emoji_app_events(
    parent_pid: i32,
    window: &AnyObject,
    editor: &AnyObject,
    state: &mut EmojiEvents,
) -> Result<bool> {
    unsafe {
        let app: *mut AnyObject =
            msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
        for _ in 0..32 {
            let date: *mut AnyObject =
                msg_send![AnyClass::get(c"NSDate").unwrap(),dateWithTimeIntervalSinceNow:0.0f64];
            let event: *mut AnyObject = msg_send![app,nextEventMatchingMask:usize::MAX,untilDate:date,
                inMode:&*NSString::from_str("kCFRunLoopDefaultMode"),dequeue:true];
            if event.is_null() {
                break;
            }
            let kind: usize = msg_send![event, type];
            if !matches!(kind, 13..=17) {
                ensure!(
                    matches!(kind, 10 | 11),
                    "Physical input interrupted owned emoji child"
                );
                let native: *mut objc2_core_graphics::CGEvent = msg_send![event, CGEvent];
                let raw: ax::ffi::Native = native.cast();
                ensure!(!raw.is_null(), "Unattributed key input refused");
                // Authenticate before reading Unicode: physical/foreign text is
                // neither inspected nor exported.
                ensure!(
                    ax::ffi::CGEventGetIntegerValueField(raw, 41) == i64::from(parent_pid)
                        && ax::ffi::CGEventGetIntegerValueField(raw, 40)
                            == i64::from(std::process::id())
                        && ax::ffi::CGEventGetIntegerValueField(raw, 42) == 0x4252_4f54,
                    "Physical or foreign key input interrupted owned emoji child"
                );
                let glyph = state
                    .glyph
                    .context("Unarmed native Unicode event refused")?;
                ensure!(
                    state
                        .armed_at
                        .is_some_and(|start| start.elapsed() < Duration::from_secs(3)),
                    "Native Unicode arming expired"
                );
                let active: bool = msg_send![app, isActive];
                let key: *mut AnyObject = msg_send![app, keyWindow];
                let responder: *mut AnyObject = msg_send![window, firstResponder];
                ensure!(
                    active
                        && key == window as *const _ as *mut _
                        && responder == editor as *const _ as *mut _,
                    "Owned Unicode focus/field changed"
                );
                ensure!(
                    ax::ffi::CGEventGetFlags(raw) == 0
                        && ax::ffi::CGEventGetIntegerValueField(raw, 9) == 0,
                    "Unexpected native key modifiers/code"
                );
                let mut units = [0u16; 20];
                let mut length = 0usize;
                ax::ffi::CGEventKeyboardGetUnicodeString(
                    raw,
                    units.len(),
                    &mut length,
                    units.as_mut_ptr(),
                );
                ensure!(
                    length <= units.len()
                        && units[..length].iter().copied().eq(glyph.encode_utf16()),
                    "Authenticated native payload differs from source fixture"
                );
                if kind == 10 {
                    ensure!(
                        state.key_down == 0 && state.key_up == 0,
                        "Duplicate Unicode key down"
                    );
                    state.key_down += 1;
                } else {
                    ensure!(
                        state.key_down == 1 && state.key_up == 0,
                        "Duplicate/out-of-order Unicode key up"
                    );
                    state.key_up += 1;
                }
            }
            let _: () = msg_send![app,sendEvent:event];
        }
        let _: () = msg_send![app, updateWindows];
        Ok(msg_send![app, isActive])
    }
}

/// Volatile argument-domain values affect only this process, never normal prefs.
/// Apply before NSApplication initialization; no defaults write/synchronize call.
fn disable_window_restoration() -> Result<()> {
    unsafe {
        let dictionary: Obj = {
            let raw: *mut AnyObject =
                msg_send![AnyClass::get(c"NSMutableDictionary").unwrap(), new];
            Retained::from_raw(raw).context("Volatile defaults dictionary unavailable")?
        };
        for (key, value) in [
            ("ApplePersistenceIgnoreState", true),
            ("NSApplePersistenceIgnoreState", true),
            ("NSQuitAlwaysKeepsWindows", false),
        ] {
            let key = NSString::from_str(key);
            let value: *mut AnyObject =
                msg_send![AnyClass::get(c"NSNumber").unwrap(),numberWithBool:value];
            let _: () = msg_send![&*dictionary, setObject:value, forKey:&*key];
        }
        let defaults: *mut AnyObject = msg_send![
            AnyClass::get(c"NSUserDefaults").unwrap(),
            standardUserDefaults
        ];
        ensure!(!defaults.is_null(), "Process-local defaults unavailable");
        let domain = NSString::from_str("NSArgumentDomain");
        let _: () = msg_send![defaults, setVolatileDomain:&*dictionary, forName:&*domain];
        for (key, expected) in [
            ("ApplePersistenceIgnoreState", true),
            ("NSApplePersistenceIgnoreState", true),
            ("NSQuitAlwaysKeepsWindows", false),
        ] {
            let key = NSString::from_str(key);
            let actual: bool = msg_send![defaults,boolForKey:&*key];
            ensure!(
                actual == expected,
                "Window restoration defaults were not disabled"
            );
        }
    }
    Ok(())
}

fn disable_assistance(editor: &AnyObject) -> Result<()> {
    macro_rules! disable {
        ($setter:ident, $getter:ident) => { unsafe {
            let supported: bool = msg_send![editor,respondsToSelector:sel!($setter:)];
            if supported {
                let _: () = msg_send![editor,$setter:false];
                let enabled: bool = msg_send![editor,$getter];
                ensure!(!enabled, "Owned text service could not be disabled: {}", stringify!($setter));
            }
        }};
    }
    disable!(
        setContinuousSpellCheckingEnabled,
        isContinuousSpellCheckingEnabled
    );
    disable!(setGrammarCheckingEnabled, isGrammarCheckingEnabled);
    disable!(
        setAutomaticSpellingCorrectionEnabled,
        isAutomaticSpellingCorrectionEnabled
    );
    disable!(
        setAutomaticTextReplacementEnabled,
        isAutomaticTextReplacementEnabled
    );
    disable!(
        setAutomaticQuoteSubstitutionEnabled,
        isAutomaticQuoteSubstitutionEnabled
    );
    disable!(
        setAutomaticDashSubstitutionEnabled,
        isAutomaticDashSubstitutionEnabled
    );
    disable!(
        setAutomaticLinkDetectionEnabled,
        isAutomaticLinkDetectionEnabled
    );
    disable!(
        setAutomaticDataDetectionEnabled,
        isAutomaticDataDetectionEnabled
    );
    disable!(
        setAutomaticTextCompletionEnabled,
        isAutomaticTextCompletionEnabled
    );
    disable!(
        setAutomaticInlinePredictionEnabled,
        isAutomaticInlinePredictionEnabled
    );
    Ok(())
}
fn widget_receipts(editors: &[Obj; 2]) -> Value {
    Value::Array(editors.iter().map(|editor| {
        let selection: NSRange = unsafe { msg_send![&**editor,selectedRange] };
        let tracked: bool = unsafe { msg_send![&**editor,respondsToSelector:sel!(ownedEmojiTextChanges)] };
        let mut receipt = json!({"text":string(editor),"location":selection.location,"length":selection.length});
        if tracked {
            let changes: u64 = unsafe { msg_send![&**editor,ownedEmojiTextChanges] };
            receipt["text_changes"] = json!(changes);
            let text = receipt["text"].as_str().unwrap();
            let utf8 = text.as_bytes().to_vec();
            let utf16 = text.encode_utf16().collect::<Vec<_>>();
            receipt["utf8"] = json!(utf8);
            receipt["utf16"] = json!(utf16);
        }
        receipt
    }).collect())
}
struct OwnedWindows(Vec<Obj>);
impl Drop for OwnedWindows {
    fn drop(&mut self) {
        for window in &self.0 {
            unsafe {
                let _: () = msg_send![&**window, close];
            }
        }
    }
}

pub(super) fn child() -> Result<i32> {
    ensure!(
        std::env::var("BROTYPIST_OWNED_DESKTOP_SMOKE").as_deref() == Ok("1"),
        "Child not authorized"
    );
    // A CLI invocation cannot supply a PID or token. The OS-inherited connected
    // socket is the capability; the parent holds its sole other endpoint.
    let fd = unsafe { libc::dup(libc::STDIN_FILENO) };
    ensure!(fd >= 0, "Missing inherited socket");
    let mut stream = unsafe { UnixStream::from_raw_fd(fd) };
    timeout(&stream)?;
    stream
        .peer_addr()
        .context("Child stdin is not a connected private socket")?;
    let hello = receive(&mut stream)?;
    let emoji = hello["emoji"]
        .as_bool()
        .context("Missing authenticated child mode")?;
    let tab_row = hello["tab_row"]
        .as_bool()
        .context("Missing authenticated Tab-row mode")?;
    ensure!(!(emoji && tab_row), "Conflicting owned modes");
    let nonce = hello["nonce"]
        .as_str()
        .context("Missing parent nonce")?
        .to_owned();
    ensure!(
        nonce.len() == 64 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid nonce"
    );
    let parent_pid = unsafe { libc::getppid() };
    ensure!(
        hello["parent_pid"] == parent_pid && hello["child_pid"] == std::process::id(),
        "Parent/child PID mismatch"
    );
    ensure!(
        hello["parent_started"]
            == json!(
                MacBridge::owned_process_started(parent_pid)
                    .context("Parent incarnation missing")?
            ),
        "Parent incarnation mismatch"
    );
    let parent_started = MacBridge::owned_process_started(parent_pid).context("Parent exited")?;
    let bundle_id = unsafe {
        let bundle: *mut AnyObject = msg_send![AnyClass::get(c"NSBundle").unwrap(), mainBundle];
        let identifier: *mut NSString = msg_send![bundle, bundleIdentifier];
        ensure!(
            !identifier.is_null(),
            "Child must run in its private diagnostic bundle"
        );
        (&*identifier).to_string()
    };
    ensure!(
        bundle_id == format!("org.brotypist.ownedax.{nonce}") && hello["bundle_id"] == bundle_id,
        "Child is not the unique diagnostic bundle"
    );
    send(
        &mut stream,
        &json!({"nonce":nonce,"pid":std::process::id(),"bundle_id":bundle_id,
        "started":MacBridge::owned_process_started(std::process::id() as i32).context("Child incarnation unavailable")?}),
    )?;
    let authorization = receive(&mut stream)?;
    ensure!(
        authorization["nonce"] == nonce
            && authorization["sequence"] == 1
            && authorization["operation"] == "authorize",
        "No authenticated pre-seeding authorization"
    );
    disable_window_restoration()?;
    let mtm = MainThreadMarker::new().context("AppKit main thread required")?;
    let app: Obj = unsafe {
        let pointer: *mut AnyObject =
            msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
        Retained::retain(pointer).context("AppKit unavailable")?
    };
    unsafe {
        let _: bool = msg_send![&*app,setActivationPolicy:1isize];
        let _: () = msg_send![&*app, disableRelaunchOnLogin];
        let _: () = msg_send![&*app, finishLaunching];
    }
    let kit = Kit::new(mtm, true);
    let window = kit.window(&nonce, "owned.ax.window", 720., 320., false);
    let other = kit.window(
        "Owned non-enrolled window",
        "owned.ax.other",
        300.,
        120.,
        false,
    );
    for owned_window in [&window, &other] {
        unsafe {
            let _: () = msg_send![&**owned_window,setRestorable:false];
            let restorable: bool = msg_send![&**owned_window, isRestorable];
            ensure!(
                !restorable,
                "Owned window restoration could not be disabled"
            );
        }
    }
    let _windows = OwnedWindows(vec![window.clone(), other.clone()]);
    let root = macos_smoke::content(&window)?;
    let editors = [
        if emoji {
            emoji_editor(mtm)
        } else {
            kit.editor(
                "owned.ax.first",
                "First synthetic field",
                rect(20., 170., 680., 110.),
                "",
            )
        },
        kit.editor(
            "owned.ax.second",
            "Second synthetic field",
            rect(20., 30., 680., 110.),
            "",
        ),
    ];
    for editor in &editors {
        disable_assistance(editor)?;
        add(&root, editor);
    }
    // Assistance is disabled before any source fixture enters the native editor.
    set_string(&editors[0], if emoji { EMOJI_ALIAS } else { INITIAL });
    set_string(&editors[1], SECOND);
    let initial_selection = if tab_row {
        seed_tab_row(&editors[0], "row-baseline")?
    } else if emoji {
        NSRange::new(13, 0)
    } else {
        NSRange::new(5, 2)
    };
    unsafe {
        let _: () = msg_send![&*editors[0],setSelectedRange:initial_selection];
        let _: bool = msg_send![&*window,makeFirstResponder:&*editors[0]];
    }
    present_window(&window, true); // The one permitted initial owned activation.
    let activate_deadline = Instant::now() + Duration::from_secs(3);
    while !macos_smoke::choice_app_events(None)? {
        ensure!(
            Instant::now() < activate_deadline,
            "Owned initial activation timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    send(
        &mut stream,
        &json!({"nonce":nonce,"sequence":1,"ok":true,
        "requested_selection_utf16":{"location":initial_selection.location,"length":initial_selection.length},
        "widgets":widget_receipts(&editors)}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut sequence = 1u64;
    let mut emoji_events = EmojiEvents::default();
    let mut emoji_glyph = "👩‍💻";
    loop {
        ensure!(
            Instant::now() < deadline,
            "Owned child protocol deadline elapsed"
        );
        ensure!(
            unsafe { libc::getppid() } == parent_pid
                && MacBridge::owned_process_started(parent_pid) == Some(parent_started),
            "Authenticated parent exited or changed"
        );
        ensure!(
            if emoji {
                emoji_app_events(parent_pid, &window, &editors[0], &mut emoji_events)?
            } else {
                macos_smoke::choice_app_events(None)?
            },
            "External focus interrupted owned AX child; never reclaim"
        );
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, 5) };
        ensure!(ready >= 0, "Owned socket polling failed");
        if ready == 0 {
            continue;
        }
        let request = receive(&mut stream)?;
        sequence += 1;
        ensure!(
            request["nonce"] == nonce && request["sequence"] == sequence,
            "Invalid owned request sequence"
        );
        let operation = request["operation"]
            .as_str()
            .context("Missing owned operation")?;
        if operation == "close" {
            send(
                &mut stream,
                &json!({"nonce":nonce,"sequence":sequence,"ok":true}),
            )?;
            return Ok(0);
        }
        let mut requested_selection = None;
        unsafe {
            match operation {
                "read" => {}
                name if tab_row && tab_row_case(name).is_some() => {
                    requested_selection = Some(seed_tab_row(&editors[0], name)?);
                }
                "emoji-alias" | "emoji-emoticon" if emoji => {
                    ensure!(
                        emoji_events.glyph.is_none(),
                        "Cannot reseed an armed Unicode transaction"
                    );
                    let alias = operation == "emoji-alias";
                    set_string(
                        &editors[0],
                        if alias { EMOJI_ALIAS } else { EMOJI_EMOTICON },
                    );
                    let range = NSRange::new(if alias { 13 } else { 9 }, 0);
                    let _: () = msg_send![&*editors[0],setSelectedRange:range];
                    requested_selection = Some(range);
                    emoji_glyph = if alias { "👩‍💻" } else { "🙂" };
                }
                "emoji-arm" if emoji => {
                    ensure!(
                        emoji_events.glyph.is_none(),
                        "Unicode transaction already armed"
                    );
                    emoji_events = EmojiEvents {
                        glyph: Some(emoji_glyph),
                        armed_at: Some(Instant::now()),
                        key_down: 0,
                        key_up: 0,
                    };
                }
                "emoji-disarm" if emoji => {
                    ensure!(
                        emoji_events.key_down == 1 && emoji_events.key_up == 1,
                        "Cannot acknowledge incomplete native event pair"
                    );
                    emoji_events.glyph = None;
                    emoji_events.armed_at = None;
                }
                "first" | "second" => {
                    let index = usize::from(operation == "second");
                    let focused: bool = msg_send![&*window,makeFirstResponder:&*editors[index]];
                    ensure!(focused, "Owned field refused first responder");
                }
                "value" => {
                    let _: () = msg_send![&*editors[0],insertText:&*NSString::from_str(CHANGED),replacementRange:NSRange::new(0, INITIAL.encode_utf16().count())];
                }
                "reset" => {
                    set_string(&editors[0], INITIAL);
                    let range = NSRange::new(5, 2);
                    requested_selection = Some(range);
                    let _: () = msg_send![&*editors[0],setSelectedRange:range];
                }
                "selection" | "selection-cluster" => {
                    let range = if operation == "selection" {
                        NSRange::new(9, 1)
                    } else {
                        NSRange::new(8, 2)
                    };
                    requested_selection = Some(range);
                    let _: () = msg_send![&*editors[0],setSelectedRange:range];
                }
                "geometry" => {
                    let _: () = msg_send![&*editors[0],setFrame:rect(35.,175.,650.,100.)];
                    let previous: NSRect = msg_send![&*window, frame];
                    let _: () = msg_send![&*window,setFrameOrigin:NSPoint::new(previous.origin.x+12.,previous.origin.y+8.)];
                }
                "readonly" => {
                    let _: () = msg_send![&*editors[0],setEditable:false];
                }
                "foreign-window" => {
                    present_window(&other, false);
                }
                _ => anyhow::bail!("Unknown owned operation"),
            }
        }
        let native_widgets = widget_receipts(&editors);
        ensure!(
            if emoji {
                emoji_app_events(parent_pid, &window, &editors[0], &mut emoji_events)?
            } else {
                macos_smoke::choice_app_events(None)?
            },
            "External focus interrupted owned operation"
        );
        send(
            &mut stream,
            &json!({"nonce":nonce,"sequence":sequence,"ok":true,"bundle_id":bundle_id,
            "requested_selection_utf16":requested_selection.map(|range|json!({"location":range.location,"length":range.length})),
            "native_widgets_before_event_drain":native_widgets,"widgets":widget_receipts(&editors),
            "tab_row":if tab_row {
                tab_row_receipt(&editors[0]).unwrap_or_else(|error|
                    json!({"status":"native_readback_unavailable","error":format!("{error:#}")}))
            } else {Value::Null},
            "emoji_events":if emoji {emoji_events.receipt()} else {Value::Null}}),
        )?;
    }
}
