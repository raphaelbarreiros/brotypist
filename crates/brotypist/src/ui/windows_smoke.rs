//! Feature-gated real-Shell desktop smoke. Never a replacement UI or model fixture.
use super::*;
use anyhow::Context;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::{
    Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow},
    System::Threading::GetCurrentThreadId,
};
use windows::core::BOOL;

pub fn run(output: &Path) -> Result<i32> {
    ensure!(
        std::env::var_os("CI").is_some()
            || std::env::var("BROTYPIST_OWNED_DESKTOP_SMOKE").as_deref() == Ok("1"),
        "Native smoke requires CI or BROTYPIST_OWNED_DESKTOP_SMOKE=1."
    );
    let home = PathBuf::from(
        std::env::var_os("INKLOW_HOME")
            .context("Native smoke requires an explicit private INKLOW_HOME.")?,
    );
    ensure!(
        home.is_absolute()
            && home.is_dir()
            && !std::fs::symlink_metadata(&home)?.file_type().is_symlink(),
        "Native smoke requires an absolute, existing, non-symlink private profile."
    );
    // ui::smoke performs the empty/private profile check and acquires the sole instance.lock.
    ensure!(
        output.is_absolute(),
        "The native smoke output directory must be absolute."
    );
    std::fs::create_dir_all(output)?;
    ensure!(
        !std::fs::symlink_metadata(output)?.file_type().is_symlink(),
        "Native smoke output cannot be a symlink."
    );
    let mut receipt = json!({"schema":1,"platform":"windows","version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"outcome":"running","checks":[],"screenshots":[],"not_exercised":[],"normal_profile_used":false,"model_download_requested":false,"os_permission_changes":false,"startup_registration_requested":false});
    let result = exercise(output, &mut receipt);
    let exit = match result {
        Ok(true) => {
            receipt["outcome"] = json!("passed");
            0
        }
        Ok(false) => {
            receipt["outcome"] = json!("incomplete");
            2
        }
        Err(error) => {
            receipt["outcome"] = json!("failed");
            receipt["failure"] = json!(error.to_string());
            let inference=receipt["checks"].as_array().unwrap().iter().any(|c|c["name"]=="real-selected-gguf-inference-native-inline-visible-prefix-and-exact-owned-readback");
            if !inference {
                receipt["not_exercised"].as_array_mut().unwrap().push(json!({"scenario":"real-owned-preview-acceptance","reason":"A prerequisite or preceding native assertion failed; no successful inference/readback is claimed."}));
            }
            1
        }
    };
    std::fs::write(
        output.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    Ok(exit)
}
fn passed(receipt: &mut Value, name: &str) {
    receipt["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":name,"passed":true}));
}
fn field_id(key: &str) -> Result<usize> {
    Ok(FIELD_BASE
        + FIELDS
            .iter()
            .position(|f| f.key == key)
            .context("Missing native setting control")?)
}
fn owned(hwnd: HWND) -> Result<()> {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        ensure!(
            IsWindow(Some(hwnd)).as_bool() && pid == std::process::id(),
            "Smoke refuses a non-owned native handle."
        );
        Ok(())
    }
}
fn pump(shell: &mut Shell, clipboard: &crate::system::ClipboardService) -> Result<bool> {
    unsafe {
        let mut msg = MSG::default();
        let mut stopped = false;
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                stopped = true;
                continue;
            }
            if !msg.hwnd.0.is_null() {
                owned(msg.hwnd)?;
            }
            dispatch_message(shell, &msg)?;
        }
        clipboard.poll()?;
        let events = EVENTS.with(|q| q.borrow_mut().drain(..).collect::<Vec<_>>());
        for event in events {
            stopped |= dispatch_event(shell, event)?;
        }
        Ok(stopped)
    }
}
fn until(
    shell: &mut Shell,
    clipboard: &crate::system::ClipboardService,
    timeout: Duration,
    mut condition: impl FnMut(&Shell) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        pump(shell, clipboard)?;
        if condition(shell) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "The owned native UI did not reach the asserted state before its deadline."
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn reveal(
    shell: &mut Shell,
    id: usize,
    clipboard: &crate::system::ClipboardService,
) -> Result<HWND> {
    let hwnd = shell.h(id);
    owned(hwnd)?;
    if !unsafe { IsWindowVisible(hwnd).as_bool() } {
        let y = shell
            .controls
            .iter()
            .find(|c| c.id == id)
            .context("Missing owned control")?
            .y;
        let position = (y - 120).clamp(0, 32767) as usize;
        unsafe {
            SendMessageW(
                shell.window,
                WM_VSCROLL,
                Some(WPARAM((position << 16) | 4)),
                None,
            );
        }
        pump(shell, clipboard)?;
    }
    ensure!(
        unsafe { IsWindowVisible(hwnd).as_bool() },
        "The asserted native control is not visible."
    );
    Ok(hwnd)
}
fn click(shell: &mut Shell, id: usize, clipboard: &crate::system::ClipboardService) -> Result<()> {
    let hwnd = reveal(shell, id, clipboard)?;
    ensure!(
        unsafe { IsWindowEnabled(hwnd).as_bool() },
        "Smoke refuses to click a disabled native control."
    );
    unsafe {
        SendMessageW(hwnd, BM_CLICK, None, None);
    }
    pump(shell, clipboard)?;
    Ok(())
}
fn pane(
    shell: &mut Shell,
    index: usize,
    clipboard: &crate::system::ClipboardService,
) -> Result<()> {
    let selector = reveal(shell, ROOT_PANE, clipboard)?;
    set_selection(selector, index);
    unsafe {
        SendMessageW(
            shell.window,
            WM_COMMAND,
            Some(WPARAM(ROOT_PANE | ((CBN_SELCHANGE as usize) << 16))),
            Some(LPARAM(selector.0 as isize)),
        );
    }
    pump(shell, clipboard)?;
    ensure!(
        shell.pane == index && selection(selector) == index,
        "Native pane navigation did not apply."
    );
    Ok(())
}
fn edit(
    shell: &mut Shell,
    key: &str,
    value: &str,
    clipboard: &crate::system::ClipboardService,
) -> Result<()> {
    let hwnd = reveal(shell, field_id(key)?, clipboard)?;
    // WM_SETTEXT omits EN_CHANGE for multiline Edit controls. Use their actual
    // replacement transaction so canonical draft handling receives the native notification.
    let value_wide = wide(value);
    unsafe {
        SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        SendMessageW(
            hwnd,
            EM_REPLACESEL,
            Some(WPARAM(1)),
            Some(LPARAM(value_wide.as_ptr() as isize)),
        );
    }
    pump(shell, clipboard)?;
    ensure!(
        text(hwnd) == value,
        "The owned native Edit did not retain its entered value."
    );
    Ok(())
}
fn screenshot(shell: &Shell, output: &Path, name: &str, receipt: &mut Value) -> Result<()> {
    owned(shell.window)?;
    ensure!(
        unsafe { IsWindowVisible(shell.window).as_bool() },
        "No visible owned Shell to capture."
    );
    let path = output.join(format!("{name}.png"));
    capture(shell.window, &path)?;
    receipt["screenshots"].as_array_mut().unwrap().push(json!({"file":path.file_name().unwrap().to_string_lossy(),"method":"PrintWindow-owned-hwnd","pid":std::process::id()}));
    Ok(())
}
fn exercise(output: &Path, receipt: &mut Value) -> Result<bool> {
    let native = NativeEnvironment::new()?;
    let mut power = crate::power::PowerSampler::start()?;
    let clipboard = crate::system::ClipboardService::install()?;
    let mut config = Config::default();
    config.setup_completed = true;
    config.global_enabled = false;
    config.check_updates_automatically = false;
    config.launch_at_login = false;
    config.max_tokens = 12;
    config.context_chars = 256;
    config.completion_length = "short".into();
    let mut shell = Shell::new(
        config,
        &LaunchOptions {
            no_tray: true,
            ..Default::default()
        },
        native.instance,
    )?;
    let scenario = (|| -> Result<bool> {
        shell.show();
        pump(&mut shell, &clipboard)?;
        ensure!(
            unsafe { GetForegroundWindow() == shell.window },
            "The owned Windows desktop could not activate the Shell; desktop access was not granted."
        );
        ensure!(
            !shell.tray && !shell.tray_requested,
            "No-tray recovery unexpectedly registered an icon."
        );
        passed(receipt, "actual-owned-shell-visible-with-no-tray-recovery");
        for index in 0..PANES.len() {
            pane(&mut shell, index, &clipboard)?;
            screenshot(&shell, output, &format!("settings-{index}"), receipt)?;
        }
        passed(receipt, "six-native-settings-panes-navigate-and-render");
        modal(
            &mut shell,
            &clipboard,
            "No system tray",
            IDNO.0,
            None,
            |s, c| {
                owned(s.window)?;
                unsafe {
                    SendMessageW(s.window, WM_CLOSE, None, None);
                }
                pump(s, c)?;
                Ok(())
            },
        )?;
        ensure!(
            !shell.closing && unsafe { IsWindowVisible(shell.window).as_bool() },
            "Declining no-tray Quit lost the recovery window."
        );
        passed(
            receipt,
            "no-tray-close-decline-retains-visible-recovery-window",
        );
        pane(&mut shell, 3, &clipboard)?;
        edit(
            &mut shell,
            "custom_instructions",
            "Native smoke: concise English. 😀",
            &clipboard,
        )?;
        ensure!(
            shell.draft.is_dirty(),
            "The native edit did not reach the settings draft."
        );
        ensure!(
            shell.controller.config().custom_instructions.is_empty(),
            "Editing unexpectedly applied an unsaved draft."
        );
        pane(&mut shell, 0, &clipboard)?;
        pane(&mut shell, 3, &clipboard)?;
        ensure!(
            text(shell.h(field_id("custom_instructions")?)) == "Native smoke: concise English. 😀",
            "Pane changes lost the instruction editor."
        );
        modal(
            &mut shell,
            &clipboard,
            "Unsaved settings",
            IDCANCEL.0,
            None,
            |s, c| {
                owned(s.window)?;
                unsafe {
                    SendMessageW(s.window, WM_CLOSE, None, None);
                }
                pump(s, c)?;
                Ok(())
            },
        )?;
        ensure!(
            shell.draft.is_dirty()
                && shell.controller.config().custom_instructions.is_empty()
                && !shell.closing,
            "Dirty-close Cancel changed the active configuration or closed the UI."
        );
        passed(
            receipt,
            "native-dirty-close-cancel-retains-unsaved-instructions",
        );
        click(&mut shell, REVERT, &clipboard)?;
        ensure!(
            !shell.draft.is_dirty() && shell.controller.config().custom_instructions.is_empty(),
            "Native Revert did not discard only the draft."
        );
        passed(receipt, "draft-edit-pane-preservation-and-revert");
        edit(
            &mut shell,
            "custom_instructions",
            "Write concise English continuations.",
            &clipboard,
        )?;
        click(&mut shell, SAVE, &clipboard)?;
        ensure!(
            crate::config::load_config()?.custom_instructions
                == "Write concise English continuations.",
            "Native Save did not persist the private profile."
        );
        passed(receipt, "native-save-persists-private-profile");
        pane(&mut shell, 4, &clipboard)?;
        let consents = [
            "clipboard_context",
            "screen_context",
            "screenshot_context",
            "screenshot_appearance",
            "record_history",
            "terminal_completion",
        ];
        for key in consents {
            modal(
                &mut shell,
                &clipboard,
                "Optional local data access",
                IDYES.0,
                None,
                |s, c| click(s, field_id(key)?, c),
            )?;
            for other in consents {
                ensure!(
                    shell.draft.value(other) == Some(&json!(other == key)),
                    "One privacy consent enabled another source."
                );
            }
            ensure!(
                !shell.controller.config().record_history,
                "Draft-only history consent accessed live encryption state."
            );
            click(&mut shell, REVERT, &clipboard)?;
        }
        passed(
            receipt,
            "each-context-history-terminal-consent-is-independent-and-draft-only",
        );
        modal(
            &mut shell,
            &clipboard,
            "Optional local data access",
            IDYES.0,
            None,
            |s, c| click(s, field_id("screen_context")?, c),
        )?;
        click(&mut shell, SAVE, &clipboard)?;
        let saved = crate::config::load_config()?;
        ensure!(
            saved.screen_context
                && !saved.clipboard_context
                && !saved.screenshot_context
                && !saved.record_history,
            "Saved context consent changed another privacy switch."
        );
        passed(
            receipt,
            "saving-one-context-consent-does-not-enable-another",
        );
        // Revoke the saved source through the same native control before trying a model.
        click(&mut shell, field_id("screen_context")?, &clipboard)?;
        click(&mut shell, SAVE, &clipboard)?;
        pane(&mut shell, 5, &clipboard)?;
        click(&mut shell, WELCOME, &clipboard)?;
        ensure!(
            shell.welcome.as_ref().is_some_and(|w| w.step == 0),
            "Welcome did not open its actual first step."
        );
        for step in 0..2 {
            ensure!(
                shell.welcome.as_ref().unwrap().step == step,
                "Wrong Welcome step."
            );
            screenshot(&shell, output, &format!("welcome-{step}"), receipt)?;
            click(&mut shell, W_NEXT, &clipboard)?;
        }
        ensure!(
            !unsafe { IsWindowEnabled(shell.h(W_NEXT)).as_bool() },
            "Unreviewed access enabled Continue."
        );
        click(&mut shell, 88, &clipboard)?;
        screenshot(&shell, output, "welcome-2", receipt)?;
        click(&mut shell, W_NEXT, &clipboard)?;
        ensure!(
            shell.welcome.as_ref().unwrap().step == 3
                && !unsafe { IsWindowEnabled(shell.h(W_NEXT)).as_bool() },
            "No selected model borrowed readiness."
        );
        passed(
            receipt,
            "welcome-system-review-and-selected-model-readiness-refuse-unmet-prerequisites",
        );
        let model = match crate::ui::smoke_model_path() {
            Ok(path) if path.is_file() => Some(path),
            _ => None,
        };
        let Some(model) = model else {
            receipt["not_exercised"].as_array_mut().unwrap().push(json!({"scenario":"welcome-real-inference-visible-prefix-readback","reason":"No existing real GGUF smoke model is available; no model download was requested."}));
            screenshot(&shell, output, "welcome-model-unavailable", receipt)?;
            return Ok(false);
        };
        // Keep the Shell-facing DOS spelling: Windows canonicalization adds a
        // verbatim filesystem prefix which common dialog UI need not interpret.
        let model = std::path::absolute(model)?;
        let model_identity = model.canonicalize()?;
        modal(
            &mut shell,
            &clipboard,
            "Import GGUF model",
            IDOK.0,
            Some(model.clone()),
            |s, c| click(s, MODEL_IMPORT, c),
        )?;
        let selected_model = shell
            .draft
            .value("model_path")
            .and_then(Value::as_str)
            .context("The real native import dialog did not select a model.")?;
        ensure!(
            Path::new(selected_model).canonicalize()? == model_identity,
            "The real native import dialog did not return the selected GGUF."
        );
        ensure!(
            unsafe { IsWindowEnabled(shell.h(W_NEXT)).as_bool() },
            "The selected existing model did not establish readiness."
        );
        screenshot(&shell, output, "welcome-3", receipt)?;
        click(&mut shell, W_NEXT, &clipboard)?;
        ensure!(
            shell.welcome.as_ref().unwrap().step == 4,
            "Welcome did not open its owned Edit preview."
        );
        owned(shell.h(W_EDIT))?;
        unsafe {
            SetFocus(Some(shell.h(W_EDIT)))?;
        }
        until(&mut shell, &clipboard, Duration::from_secs(180), |s| {
            s.preview_ghost
                .as_ref()
                .is_some_and(|g| !g.visible().is_empty())
        })?;
        let preview_config = &shell.preview.as_ref().unwrap().config;
        ensure!(
            !preview_config.clipboard_context
                && !preview_config.screen_context
                && !preview_config.screenshot_context
                && !preview_config.screenshot_appearance
                && !preview_config.record_history
                && !preview_config.statistics_enabled,
            "The owned preview borrowed external context or recording consent."
        );
        let before = text(shell.h(W_EDIT));
        let visible = shell.preview_ghost.as_ref().unwrap().visible();
        ensure!(
            !visible.is_empty(),
            "No native visible prefix was rendered."
        );
        screenshot(&shell, output, "welcome-4-real-inline", receipt)?;
        click(&mut shell, W_WORD, &clipboard)?;
        let after = text(shell.h(W_EDIT));
        let suffix = after
            .strip_prefix(&before)
            .context("Preview acceptance rewrote the owned prefix")?;
        ensure!(
            !suffix.is_empty() && visible.starts_with(suffix.trim_end()),
            "Preview inserted text outside its actually visible prefix."
        );
        let preview = shell.preview.as_mut().unwrap().poll();
        ensure!(
            preview["accepted"].as_u64().unwrap_or(0) > 0,
            "Native owned Edit readback did not confirm acceptance."
        );
        ensure!(
            shell.controller.status()["preview"] == true,
            "Owned preview resumed external typing prematurely."
        );
        passed(
            receipt,
            "real-selected-gguf-inference-native-inline-visible-prefix-and-exact-owned-readback",
        );
        let language = reveal(&mut shell, W_LANGUAGE, &clipboard)?;
        // Merely tabbing through the language combo sends CBN_SETFOCUS and
        // CBN_KILLFOCUS, not a language selection. Neither may reset writing.
        unsafe {
            SetFocus(Some(language))?;
        }
        pump(&mut shell, &clipboard)?;
        ensure!(
            text(shell.h(W_EDIT)) == after,
            "Focusing the preview language combo reset the owned writing."
        );
        unsafe {
            SetFocus(Some(shell.h(W_EDIT)))?;
        }
        pump(&mut shell, &clipboard)?;
        ensure!(
            text(shell.h(W_EDIT)) == after
                && shell.preview.as_mut().unwrap().poll()["accepted"] == preview["accepted"],
            "Leaving the preview language combo reset writing or acceptance readback."
        );
        passed(
            receipt,
            "preview-language-focus-notifications-preserve-owned-writing",
        );
        set_selection(language, 1);
        unsafe {
            SendMessageW(
                shell.window,
                WM_COMMAND,
                Some(WPARAM(W_LANGUAGE | ((CBN_SELCHANGE as usize) << 16))),
                Some(LPARAM(language.0 as isize)),
            );
        }
        pump(&mut shell, &clipboard)?;
        let portuguese_before = text(shell.h(W_EDIT));
        ensure!(
            portuguese_before.starts_with("Amanhã"),
            "The native Portuguese preview seed did not preserve its Unicode text."
        );
        until(&mut shell, &clipboard, Duration::from_secs(180), |s| {
            s.preview_ghost
                .as_ref()
                .is_some_and(|g| !g.visible().is_empty())
        })?;
        let portuguese_visible = shell.preview_ghost.as_ref().unwrap().visible();
        screenshot(&shell, output, "welcome-4-portuguese-inline", receipt)?;
        click(&mut shell, W_ALL, &clipboard)?;
        let portuguese_after = text(shell.h(W_EDIT));
        let portuguese_suffix = portuguese_after
            .strip_prefix(&portuguese_before)
            .context("Full preview acceptance rewrote the Portuguese owned prefix")?;
        ensure!(
            !portuguese_suffix.is_empty()
                && portuguese_visible.starts_with(portuguese_suffix.trim_end()),
            "Full acceptance inserted beyond the native visible Portuguese preview prefix."
        );
        ensure!(
            shell.preview.as_mut().unwrap().poll()["accepted"]
                .as_u64()
                .unwrap_or(0)
                > preview["accepted"].as_u64().unwrap_or(0),
            "Portuguese owned Edit readback did not confirm full acceptance."
        );
        passed(
            receipt,
            "portuguese-owned-preview-renders-and-confirms-visible-full-acceptance",
        );
        click(&mut shell, W_NEXT, &clipboard)?;
        until(&mut shell, &clipboard, Duration::from_secs(30), |s| {
            s.welcome.as_ref().is_some_and(|w| w.step == 5) && s.preview.is_none()
        })?;
        screenshot(&shell, output, "welcome-5", receipt)?;
        click(&mut shell, W_NEXT, &clipboard)?;
        until(&mut shell, &clipboard, Duration::from_secs(10), |s| {
            s.welcome.is_none() && !s.draft.is_dirty()
        })?;
        ensure!(
            crate::config::load_config()?.setup_completed,
            "Finish did not save setup completion."
        );
        ensure!(
            !crate::config::load_config()?.record_history,
            "Welcome changed history consent."
        );
        let store = crate::storage::HistoryStore::open()?;
        ensure!(
            !store.is_unlocked() && store.history_info()?["count"] == 0,
            "Owned preview opened credentials or recorded writing history."
        );
        ensure!(
            store.statistics(0, "day")?["totals"]["completions"] == 0,
            "Owned preview changed private aggregate statistics."
        );
        let settings: Value =
            serde_json::from_slice(&std::fs::read(crate::config::config_path()?)?)?;
        ensure!(
            settings.as_object().unwrap().values().all(|value| ![
                before.as_str(),
                after.as_str(),
                portuguese_before.as_str(),
                portuguese_after.as_str()
            ]
            .contains(&value.as_str().unwrap_or(""))),
            "Ephemeral owned preview text was persisted as a setting."
        );
        passed(
            receipt,
            "six-step-welcome-closes-preview-before-resuming-and-persists-no-preview-text",
        );
        Ok(true)
    })();
    // Cleanup uses actual Quit even after a failed assertion, never process::exit.
    shell.draft.reset(&shell.controller.config())?;
    shell.new_rule();
    shell.refresh_controls()?;
    shell.welcome = None;
    shell.layout();
    let quit = click(&mut shell, QUIT, &clipboard).and_then(|_| {
        until(&mut shell, &clipboard, Duration::from_secs(45), |s| {
            s.controller.status()["shutdown_complete"] == true && s.preview.is_none()
        })
    });
    let update_stopped = shell.updates.close().is_ok();
    let shutdown =
        quit.is_ok() && update_stopped && shell.controller.status()["shutdown_complete"] == true;
    if shutdown {
        passed(
            receipt,
            "actual-native-quit-stops-controller-preview-update-owners-before-gui-guards",
        );
    }
    drop(shell);
    drop(clipboard);
    power.stop()?;
    quit?;
    ensure!(shutdown, "Native owners did not finish clean shutdown.");
    scenario
}

struct DialogRequest {
    owner: usize,
    title: String,
    button: i32,
    path: Option<Vec<u16>>,
    responded: bool,
    emergency: bool,
}
unsafe extern "system" fn dialog(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        let request = &mut *(param.0 as *mut DialogRequest);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != std::process::id() || !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let mut ancestor = hwnd;
        let mut ours = false;
        for _ in 0..8 {
            let Ok(owner) = GetWindow(ancestor, GW_OWNER) else {
                break;
            };
            if owner.0 as usize == request.owner {
                ours = true;
                break;
            }
            ancestor = owner;
        }
        if !ours {
            return BOOL(1);
        }
        let mut class = [0u16; 32];
        let n = GetClassNameW(hwnd, &mut class);
        if String::from_utf16_lossy(&class[..n.max(0) as usize]) != "#32770" {
            return BOOL(1);
        }
        if request.emergency {
            for button in [IDCANCEL.0, IDNO.0, IDOK.0] {
                if let Ok(control) = GetDlgItem(Some(hwnd), button) {
                    let _ = PostMessageW(Some(control), BM_CLICK, WPARAM(0), LPARAM(0));
                    break;
                }
            }
            return BOOL(1);
        }
        if request.responded || text(hwnd) != request.title {
            return BOOL(1);
        }
        if let Some(path) = &request.path {
            SendMessageW(
                hwnd,
                CDM_SETCONTROLTEXT,
                Some(WPARAM(0x480)),
                Some(LPARAM(path.as_ptr() as isize)),
            );
        }
        if let Ok(control) = GetDlgItem(Some(hwnd), request.button) {
            let _ = PostMessageW(Some(control), BM_CLICK, WPARAM(0), LPARAM(0));
            request.responded = true;
        }
        BOOL(1)
    }
}
fn modal<T>(
    shell: &mut Shell,
    clipboard: &crate::system::ClipboardService,
    title: &str,
    button: i32,
    path: Option<PathBuf>,
    action: impl FnOnce(&mut Shell, &crate::system::ClipboardService) -> Result<T>,
) -> Result<T> {
    owned(shell.window)?;
    let owner = shell.window.0 as usize;
    let thread_id = unsafe { GetCurrentThreadId() };
    let title = title.to_owned();
    let path = path.map(|p| wide(&p.to_string_lossy()));
    let done = Arc::new(AtomicBool::new(false));
    let stop = done.clone();
    let responder = std::thread::spawn(move || {
        let mut request = DialogRequest {
            owner,
            title,
            button,
            path,
            responded: false,
            emergency: false,
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while !stop.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                request.emergency = true;
            }
            unsafe {
                EnumThreadWindows(
                    thread_id,
                    Some(dialog),
                    LPARAM((&mut request as *mut DialogRequest) as isize),
                );
            }
            if request.emergency {
                std::thread::sleep(Duration::from_millis(100));
                return false;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        request.responded
    });
    let result = action(shell, clipboard);
    done.store(true, Ordering::Release);
    let response = responder
        .join()
        .map_err(|_| anyhow::anyhow!("The owned dialog responder failed."));
    // Preserve a real picker/action failure rather than replacing it with a
    // secondary missing-dialog assertion.
    let result = result?;
    let responded = response?;
    ensure!(
        responded,
        "The expected owned native dialog did not complete."
    );
    Ok(result)
}
struct Capture {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}
impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
fn capture(hwnd: HWND, path: &Path) -> Result<()> {
    unsafe {
        owned(hwnd)?;
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect)?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        ensure!(
            (1..=8192).contains(&width) && (1..=8192).contains(&height),
            "Owned screenshot bounds are invalid."
        );
        let dc = CreateCompatibleDC(None);
        ensure!(!dc.0.is_null(), "Owned screenshot DC is unavailable.");
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                DeleteDC(dc);
                return Err(error.into());
            }
        };
        let old = SelectObject(dc, bitmap.into());
        let _capture = Capture { dc, bitmap, old };
        ensure!(
            !bits.is_null() && !old.0.is_null() && old.0 as isize != -1,
            "The owned screenshot bitmap could not be selected."
        );
        ensure!(
            PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool(),
            "The actual owned Shell could not render its screenshot."
        );
        let bytes =
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), width as usize * height as usize * 4);
        ensure!(
            bytes.chunks_exact(4).any(|pixel| pixel[..3] != bytes[..3]),
            "The native screenshot is blank; no UI rendering proof is recorded."
        );
        // PrintWindow's BI_RGB DIB has BGRA color bytes with unspecified alpha.
        // Convert this owned offscreen bitmap in place; no desktop pixels are read.
        for pixel in bytes.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let file = std::fs::File::create(path)?;
        let mut encoder =
            png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(bytes)?;
        writer.finish()?;
        Ok(())
    }
}
