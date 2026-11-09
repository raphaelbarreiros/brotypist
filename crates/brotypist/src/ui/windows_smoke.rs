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
fn preview_caret(shell: &Shell) -> Result<(u32, u32)> {
    let edit = shell.h(W_EDIT);
    owned(edit)?;
    let (mut start, mut end) = (0u32, 0u32);
    unsafe {
        SendMessageW(
            edit,
            EM_GETSEL,
            Some(WPARAM((&mut start as *mut u32) as usize)),
            Some(LPARAM((&mut end as *mut u32) as isize)),
        );
    }
    Ok((start, end))
}
fn assert_preview_end_caret(shell: &Shell) -> Result<()> {
    let end = text(shell.h(W_EDIT)).encode_utf16().count() as u32;
    ensure!(
        preview_caret(shell)? == (end, end),
        "The owned preview seed did not leave a collapsed caret at its UTF-16 document end."
    );
    Ok(())
}
fn wait_preview(
    shell: &mut Shell,
    clipboard: &crate::system::ClipboardService,
    output: &Path,
    receipt: &mut Value,
    stage: &str,
) -> Result<()> {
    if let Err(error) = until(shell, clipboard, Duration::from_secs(180), |s| {
        s.preview_ghost
            .as_ref()
            .is_some_and(|g| !g.visible().is_empty())
    }) {
        let state = shell.preview.as_mut().map(|preview| preview.poll());
        let selection = preview_caret(shell)?;
        let edit = shell.h(W_EDIT);
        receipt["preview_failure"] = json!({
            "stage":stage,
            "state":state.as_ref().map(|v|&v["state"]),
            "message":state.as_ref().map(|v|&v["message"]),
            "suggestion_scalars":state.as_ref().and_then(|v|v["suggestion"].as_str()).map(|s|s.chars().count()),
            "document_utf16":text(edit).encode_utf16().count(),
            "selection_utf16":[selection.0,selection.1],
            "owner_released":shell.status["preview_suspended"],
            "focused":unsafe{GetFocus()==edit},
            "visible":unsafe{IsWindowVisible(edit).as_bool()},
        });
        if let Err(capture_error) = screenshot(shell, output, stage, receipt) {
            receipt["preview_failure"]["capture_error"] = json!(capture_error.to_string());
        }
        return Err(error);
    }
    Ok(())
}
// Constrain real WM_PAINT update regions before the production paint procedure
// runs. Disconnected strips deliberately have the same bounding box as Full.
#[derive(Clone, Copy)]
enum PaintDamage {
    Partial,
    Disconnected,
    Full,
}
struct DamageScope {
    window: HWND,
    damage: Box<std::cell::Cell<PaintDamage>>,
}
impl DamageScope {
    fn new(window: HWND, damage: PaintDamage) -> Result<Self> {
        owned(window)?;
        let scope = Self {
            window,
            damage: Box::new(std::cell::Cell::new(damage)),
        };
        ensure!(
            unsafe {
                SetWindowSubclass(
                    window,
                    Some(damage_proc),
                    0x42525044,
                    (&*scope.damage as *const std::cell::Cell<PaintDamage>) as usize,
                )
                .as_bool()
            },
            "Could not constrain the owned native paint damage."
        );
        Ok(scope)
    }
    fn repaint(&self, damage: PaintDamage) -> Result<()> {
        self.damage.set(damage);
        unsafe {
            ensure!(
                InvalidateRect(Some(self.window), None, false).as_bool()
                    && UpdateWindow(self.window).as_bool(),
                "Owned damage did not reach WM_PAINT."
            );
        }
        Ok(())
    }
}
impl Drop for DamageScope {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveWindowSubclass(self.window, Some(damage_proc), 0x42525044);
        }
    }
}
unsafe extern "system" fn damage_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_PAINT {
            let damage = &*(data as *const std::cell::Cell<PaintDamage>);
            let mut rect = RECT::default();
            if GetClientRect(hwnd, &mut rect).is_ok() && rect.right >= 4 {
                let _ = ValidateRect(Some(hwnd), None);
                match damage.get() {
                    PaintDamage::Full => {
                        let _ = InvalidateRect(Some(hwnd), Some(&rect), false);
                    }
                    PaintDamage::Partial | PaintDamage::Disconnected => {
                        let left = RECT {
                            right: rect.right / 4,
                            ..rect
                        };
                        let _ = InvalidateRect(Some(hwnd), Some(&left), false);
                        if matches!(damage.get(), PaintDamage::Disconnected) {
                            let right = RECT {
                                left: rect.right * 3 / 4,
                                ..rect
                            };
                            let _ = InvalidateRect(Some(hwnd), Some(&right), false);
                        }
                    }
                }
            }
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}
fn paint_coverage_fixture(instance: HINSTANCE, receipt: &mut Value) -> Result<()> {
    let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = observations.clone();
    let reporter: PaintReporter = Arc::new(move |paint| {
        recorded
            .lock()
            .unwrap()
            .push((paint.trace_id, paint.lane, paint.view_serial));
    });
    let mut work = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut work as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )?;
    }
    let snapshot = crate::core::Snapshot::default();
    let mut event = json!({
        "key": snapshot.key(), "authority": snapshot.authority(),
        "revision": 1, "text": "", "mode": "alternatives",
        "rect": [work.left + 8, work.top + 8, 1, 16], "render": null,
        "choices": ["first complete row", "second complete row"],
        "diagnostic": {"trace_id":81,"lane":"chooser","view_serial":31}
    });
    let mut chooser = ChoiceWindow::new(instance)?;
    chooser.set_paint_reporter(Some(reporter.clone()));
    let first = DamageScope::new(chooser.labels[0], PaintDamage::Partial)?;
    let second = DamageScope::new(chooser.labels[1], PaintDamage::Disconnected)?;
    ensure!(
        matches!(chooser.show(&event)?, Command::PresentationResult { visible_text, .. }
            if visible_text == "first complete row\nsecond complete row"),
        "Diagnostic coverage changed normal chooser acceptance."
    );
    ensure!(
        observations.lock().unwrap().is_empty(),
        "Partial/disconnected rows consumed a chooser identity."
    );
    first.repaint(PaintDamage::Full)?;
    ensure!(
        observations.lock().unwrap().is_empty(),
        "One complete row credited the entire chooser."
    );
    second.repaint(PaintDamage::Disconnected)?;
    ensure!(
        observations.lock().unwrap().is_empty(),
        "Disconnected damage completed the remaining row."
    );
    second.repaint(PaintDamage::Full)?;
    ensure!(
        *observations.lock().unwrap() == vec![(81, PresentationLane::Chooser, 31)],
        "Full damage of every row did not produce exactly one chooser observation."
    );
    first.damage.set(PaintDamage::Partial);
    second.damage.set(PaintDamage::Partial);
    event["diagnostic"] = json!({"trace_id":82,"lane":"chooser","view_serial":32});
    let _ = chooser.show(&event)?;
    first.repaint(PaintDamage::Full)?;
    // Leave an old-view update pending; replacement must clear both its identity
    // and its accumulated complete-row mask.
    unsafe {
        let _ = InvalidateRect(Some(chooser.labels[1]), None, false);
    }
    first.damage.set(PaintDamage::Partial);
    event["diagnostic"] = json!({"trace_id":83,"lane":"chooser","view_serial":33});
    let _ = chooser.show(&event)?;
    second.repaint(PaintDamage::Full)?;
    ensure!(
        observations.lock().unwrap().len() == 1,
        "Replacement inherited stale row coverage or identity."
    );
    first.repaint(PaintDamage::Full)?;
    ensure!(
        *observations.lock().unwrap()
            == vec![
                (81, PresentationLane::Chooser, 31),
                (83, PresentationLane::Chooser, 33)
            ],
        "Replacement chooser did not require its own complete rows."
    );
    let _ = chooser.hide();
    unsafe {
        SendMessageW(chooser.labels[0], WM_PAINT, None, None);
    }
    ensure!(
        observations.lock().unwrap().len() == 2,
        "Hidden chooser consumed a diagnostic identity."
    );
    // Build verified native font metrics, not a model or UI-rendering mock.
    let foreground = unsafe { GetForegroundWindow() };
    let dpi = unsafe { GetDpiForWindow(foreground) };
    ensure!(
        dpi > 0,
        "No foreground DPI for the owned inline paint fixture."
    );
    let mut metrics = TEXTMETRICW::default();
    let mut face = [0u16; 256];
    let measured = unsafe {
        let font = CreateFontW(
            -(12.0 * dpi as f64 / 72.0).round_ties_even() as i32,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            0,
            w!("Segoe UI"),
        );
        ensure!(!font.0.is_null(), "No native inline fixture font.");
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            let _ = DeleteObject(font.into());
            anyhow::bail!("No native inline fixture metrics DC.");
        }
        let old = SelectObject(dc, font.into());
        let ok =
            GetTextMetricsW(dc, &mut metrics).as_bool() && GetTextFaceW(dc, Some(&mut face)) > 0;
        let _ = SelectObject(dc, old);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(font.into());
        ok
    };
    ensure!(measured, "No native inline fixture font metrics.");
    let face =
        String::from_utf16_lossy(&face[..face.iter().position(|c| *c == 0).unwrap_or(face.len())]);
    let x = f64::from(work.left + 8);
    let y = f64::from(work.top + 8);
    event["mode"] = json!("completion");
    event["text"] = json!("complete inline frame");
    event["rect"] = json!([x, y, 1, metrics.tmHeight]);
    event["render"] = json!({
        "font_name":face, "font_size":12.0, "font_weight":400,
        "baseline":y + f64::from(metrics.tmAscent),
        "clip_rect":[x, y, 440, metrics.tmHeight],
        "ascent":metrics.tmAscent, "descent":metrics.tmDescent,
        "direction":"ltr", "source":"windows-uia-font-run"
    });
    event["diagnostic"] = json!({"trace_id":84,"lane":"inline","view_serial":34});
    let mut inline = overlay::Overlay::new(instance)?;
    inline.set_paint_reporter(Some(reporter));
    let damage = DamageScope::new(inline.smoke_window(), PaintDamage::Partial)?;
    ensure!(
        matches!(inline.show(&event)?, Command::PresentationResult { visible_text, .. }
            if visible_text == "complete inline frame"),
        "Diagnostic coverage changed normal inline acceptance."
    );
    ensure!(
        observations.lock().unwrap().len() == 2,
        "Partial inline damage consumed an identity."
    );
    damage.repaint(PaintDamage::Disconnected)?;
    ensure!(
        observations.lock().unwrap().len() == 2,
        "Disconnected inline damage consumed an identity."
    );
    damage.repaint(PaintDamage::Full)?;
    damage.repaint(PaintDamage::Full)?;
    ensure!(
        observations.lock().unwrap().len() == 3,
        "Full inline frame did not report exactly once."
    );
    damage.damage.set(PaintDamage::Partial);
    event["diagnostic"] = json!({"trace_id":85,"lane":"inline","view_serial":35});
    let _ = inline.show(&event)?;
    unsafe {
        let _ = InvalidateRect(Some(inline.smoke_window()), None, false);
    }
    event["text"] = json!("replacement inline frame");
    event["diagnostic"] = json!({"trace_id":86,"lane":"inline","view_serial":36});
    let _ = inline.show(&event)?;
    damage.repaint(PaintDamage::Disconnected)?;
    ensure!(
        observations.lock().unwrap().len() == 3,
        "Replacement inline view consumed incomplete/stale damage."
    );
    damage.repaint(PaintDamage::Full)?;
    let _ = inline.hide();
    unsafe {
        SendMessageW(inline.smoke_window(), WM_PAINT, None, None);
    }
    ensure!(
        *observations.lock().unwrap()
            == vec![
                (81, PresentationLane::Chooser, 31),
                (83, PresentationLane::Chooser, 33),
                (84, PresentationLane::Inline, 34),
                (86, PresentationLane::Inline, 36)
            ],
        "Inline full damage, replacement identity, or hidden guard failed."
    );
    passed(
        receipt,
        "native-update-region-partial-disconnected-full-all-rows-and-stale-replacement-paint-coverage",
    );
    Ok(())
}
fn chooser_fixture(instance: HINSTANCE, output: &Path, receipt: &mut Value) -> Result<()> {
    let mut chooser = ChoiceWindow::new(instance)?;
    let paints = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = paints.clone();
    chooser.set_paint_reporter(Some(Arc::new(move |observation| {
        observed.lock().unwrap().push((
            observation.trace_id,
            observation.lane,
            observation.view_serial,
        ));
    })));
    owned(chooser.window)?;
    let mut work = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut work as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )?;
    }
    let snapshot = crate::core::Snapshot::default();
    let event = json!({
        "key": snapshot.key(),
        "authority": snapshot.authority(),
        "revision": 1,
        "text": "",
        "mode": "alternatives",
        "diagnostic": {"trace_id": 71, "lane": "chooser", "view_serial": 19},
        "rect": [work.left + 8, work.top + 8, 1, 16],
        "render": null,
        "choices": ["  café & tea  ", "W".repeat(1024), "bad\trow", "", " 😀 e\u{301} "]
    });
    let foreground = unsafe { GetForegroundWindow() };
    let ack = chooser.show(&event)?;
    ensure!(
        matches!(ack, Command::PresentationResult { visible_text, .. }
            if visible_text == "  café & tea  \n 😀 e\u{301} "),
        "The chooser did not acknowledge the exact eligible ordered subsequence."
    );
    ensure!(
        unsafe { GetForegroundWindow() == foreground }
            && text(chooser.labels[0]) == "1.   café & tea  "
            && text(chooser.labels[1]) == "2.  😀 e\u{301} ",
        "The chooser changed focus, whitespace, Unicode, or dense numeric labels."
    );
    ensure!(
        *paints.lock().unwrap() == vec![(71, PresentationLane::Chooser, 19)],
        "Chooser did not report actual drawing using independent chooser diagnostic IDs."
    );
    unsafe {
        for label in chooser.labels.iter().take(2) {
            let _ = InvalidateRect(Some(*label), None, false);
            let _ = UpdateWindow(*label);
        }
    }
    ensure!(
        paints.lock().unwrap().len() == 1,
        "Repainting counted one chooser view twice."
    );
    let path = output.join("chooser-eligible-subsequence.png");
    capture(chooser.window, &path)?;
    receipt["screenshots"].as_array_mut().unwrap().push(json!({"file":"chooser-eligible-subsequence.png","method":"PrintWindow-owned-hwnd","pid":std::process::id()}));
    unsafe {
        for (i, label) in chooser.labels.iter().enumerate() {
            owned(*label)?;
            ensure!(
                GetDlgCtrlID(*label) == (i + 1) as i32
                    && IsWindowVisible(*label).as_bool() == (i < 2),
                "The chooser exposed a hidden row or used a non-dense control ID."
            );
        }
        SendMessageW(
            chooser.labels[1],
            WM_LBUTTONDOWN,
            Some(WPARAM(1)),
            Some(LPARAM(4 | (4 << 16))),
        );
        SendMessageW(
            chooser.labels[1],
            WM_LBUTTONUP,
            None,
            Some(LPARAM(4 | (4 << 16))),
        );
    }
    ensure!(
        chooser.take_actions() == vec![1],
        "Clicking visible row 2 did not return displayed ordinal 1."
    );
    // A queued click must not survive replacement or hiding.
    unsafe {
        SendMessageW(
            chooser.window,
            WM_COMMAND,
            Some(WPARAM(2 | ((STN_CLICKED as usize) << 16))),
            Some(LPARAM(chooser.labels[1].0 as isize)),
        );
    }
    let mut replacement = event.clone();
    replacement["revision"] = json!(2);
    replacement["choices"] = json!(["replacement"]);
    replacement["diagnostic"] = json!({"trace_id":72,"lane":"chooser","view_serial":20});
    let _ = chooser.show(&replacement)?;
    ensure!(
        chooser.take_actions().is_empty(),
        "A stale click survived replacing the chooser presentation."
    );
    ensure!(
        *paints.lock().unwrap()
            == vec![
                (71, PresentationLane::Chooser, 19),
                (72, PresentationLane::Chooser, 20)
            ],
        "Replacement paint used stale diagnostic identity."
    );
    unsafe {
        for (id, label) in [
            (2, chooser.labels[1]),
            (0, chooser.labels[0]),
            (1, chooser.labels[1]),
        ] {
            SendMessageW(
                chooser.window,
                WM_COMMAND,
                Some(WPARAM(id | ((STN_CLICKED as usize) << 16))),
                Some(LPARAM(label.0 as isize)),
            );
        }
    }
    ensure!(
        chooser.take_actions().is_empty(),
        "A hidden row, invalid control ID, or mismatched sender generated a choice."
    );
    let _ = chooser.hide();
    unsafe {
        SendMessageW(
            chooser.window,
            WM_COMMAND,
            Some(WPARAM(1 | ((STN_CLICKED as usize) << 16))),
            Some(LPARAM(chooser.labels[0].0 as isize)),
        );
    }
    ensure!(
        chooser.take_actions().is_empty(),
        "A hidden chooser accepted a stale click."
    );
    replacement["revision"] = json!(3);
    replacement["choices"] = json!(["W".repeat(1024), "bad\nrow"]);
    ensure!(
        matches!(chooser.show(&replacement)?, Command::PresentationResult { visible_text, .. }
            if visible_text.is_empty())
            && !unsafe { IsWindowVisible(chooser.window).as_bool() }
            && chooser.take_actions().is_empty(),
        "An entirely ineligible offer left a visible or actionable chooser."
    );
    ensure!(
        paints.lock().unwrap().len() == 2,
        "Hidden or refused choices fabricated a paint observation."
    );
    replacement["choices"] = json!(["metadata unavailable"]);
    replacement["diagnostic"] = Value::Null;
    let _ = chooser.show(&replacement)?;
    ensure!(
        paints.lock().unwrap().len() == 2,
        "Missing diagnostic metadata fabricated a trace."
    );
    let _ = chooser.hide();
    passed(
        receipt,
        "chooser-wm-paint-observations-use-independent-lane-ids-no-repaint-hidden-or-missing-metadata-credit",
    );
    passed(
        receipt,
        "native-chooser-exact-eligible-subsequence-dense-click2-and-stale-hidden-refusal",
    );
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
        chooser_fixture(native.instance, output, receipt)?;
        paint_coverage_fixture(native.instance, receipt)?;
        for index in 0..PANES.len() {
            pane(&mut shell, index, &clipboard)?;
            screenshot(&shell, output, &format!("settings-{index}"), receipt)?;
        }
        passed(receipt, "six-native-settings-panes-navigate-and-render");
        for key in ["typo_suppression", "typo_fixes", "screenshot_appearance"] {
            ensure!(
                !unsafe { IsWindowEnabled(shell.h(field_id(key)?)).as_bool() },
                "An unavailable Windows capability exposes an effective-looking control."
            );
        }
        passed(
            receipt,
            "unavailable-spelling-and-appearance-controls-disabled",
        );
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
        edit(&mut shell, "writing_language", "pt-BR", &clipboard)?;
        edit(&mut shell, "spelling_language", "en-GB", &clipboard)?;
        click(&mut shell, SAVE, &clipboard)?;
        until(&mut shell, &clipboard, Duration::from_secs(30), |s| {
            s.operations.is_empty()
        })?;
        ensure!(
            crate::config::load_config()?.custom_instructions
                == "Write concise English continuations.",
            "Native Save did not persist the private profile."
        );
        ensure!(
            crate::config::load_config()?.writing_language == "pt-BR" && !shell.draft.is_dirty(),
            "The writing language or matching Saved receipt did not commit."
        );
        ensure!(
            crate::config::load_config()?.spelling_language == "en-GB",
            "Regional model spelling preference did not persist."
        );
        passed(receipt, "native-save-persists-private-profile");
        // Block only this already-established disposable profile's destination.
        // Equal live configuration is deliberately not treated as Save success.
        let config_path = crate::config::config_path()?;
        let backup = config_path.with_extension("native-smoke-backup");
        ensure!(
            !backup.exists() && config_path.is_file(),
            "Private persistence fixture cannot reserve its backup."
        );
        std::fs::rename(&config_path, &backup)?;
        if let Err(error) = std::fs::create_dir(&config_path) {
            std::fs::rename(&backup, &config_path)?;
            return Err(error.into());
        }
        let failure_check = (|| -> Result<()> {
            let active = shell.controller.config();
            for instructions in [None, Some("Keep this unsaved exact draft: 😀 e\u{301}\r\n")] {
                if let Some(value) = instructions {
                    edit(&mut shell, "custom_instructions", value, &clipboard)?;
                }
                modal(&mut shell, &clipboard, "BroTypist", IDOK.0, None, |s, c| {
                    click(s, SAVE, c)?;
                    until(s, c, Duration::from_secs(30), |s| s.operations.is_empty())
                })?;
                ensure!(
                    shell.controller.config() == active,
                    "Failed persistence installed the candidate."
                );
                ensure!(
                    text(shell.h(RESULTS)).contains("failed"),
                    "A failed unchanged-value save lacked its operation failure."
                );
                if let Some(value) = instructions {
                    ensure!(
                        shell.draft.value("custom_instructions") == Some(&json!(value))
                            && shell.draft.is_dirty(),
                        "A failed persistence receipt discarded the instruction draft."
                    );
                }
            }
            Ok(())
        })();
        std::fs::remove_dir(&config_path)?;
        std::fs::rename(&backup, &config_path)?;
        failure_check?;
        click(&mut shell, REVERT, &clipboard)?;
        passed(
            receipt,
            "unchanged-and-edited-save-failures-preserve-live-config-and-exact-draft",
        );
        // Exercise the operation seam without pumping away the outstanding receipt.
        edit(
            &mut shell,
            "custom_instructions",
            "Submitted before cancellation",
            &clipboard,
        )?;
        shell.save()?;
        shell.cancel_operations()?;
        shell.draft.set(
            "custom_instructions",
            json!("Newer draft after cancel: 😀 e\u{301}"),
        )?;
        shell.refresh_controls()?;
        until(&mut shell, &clipboard, Duration::from_secs(30), |s| {
            s.operations.is_empty()
        })?;
        ensure!(
            shell.draft.is_dirty()
                && shell.draft.value("custom_instructions")
                    == Some(&json!("Newer draft after cancel: 😀 e\u{301}")),
            "A cancelled or already-terminal older operation reset a newer draft."
        );
        click(&mut shell, REVERT, &clipboard)?;
        passed(
            receipt,
            "operation-cancel-and-late-receipt-preserve-newer-draft-generation",
        );
        pane(&mut shell, 0, &clipboard)?;
        click(&mut shell, EXPORT_DIAGNOSTICS, &clipboard)?;
        let exported = text(shell.h(RESULTS));
        let path = PathBuf::from(
            exported
                .strip_prefix("Content-free request diagnostics exported to ")
                .context("The explicit diagnostic export did not report its destination.")?,
        );
        ensure!(
            path == crate::paths::config_dir()?
                .join("diagnostics")
                .join("requests.json"),
            "Diagnostic export left the disposable profile."
        );
        let content = std::fs::read_to_string(&path)?;
        let diagnostic: Value = serde_json::from_str(&content)?;
        ensure!(
            diagnostic.is_object()
                && !content.contains("Submitted before cancellation")
                && !content.contains("Newer draft after cancel"),
            "Diagnostic export included writing or lacked the structured snapshot."
        );
        passed(
            receipt,
            "explicit-native-diagnostic-export-remains-content-free-and-private-profile-local",
        );
        pane(&mut shell, 4, &clipboard)?;
        let consents = [
            "clipboard_context",
            "screen_context",
            "screenshot_context",
            "use_history",
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
        until(&mut shell, &clipboard, Duration::from_secs(30), |s| {
            s.operations.is_empty()
        })?;
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
        until(&mut shell, &clipboard, Duration::from_secs(30), |s| {
            s.operations.is_empty()
        })?;
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
        let saved_instructions = shell.controller.config().custom_instructions;
        // Seed an outstanding SettingsDraft through its public seam; the native
        // model dialog must never treat this independent draft as submitted.
        shell.draft.set(
            "custom_instructions",
            json!("Unsaved model-selection draft: 😀 e\u{301}\n"),
        )?;
        shell.refresh_controls()?;
        modal(
            &mut shell,
            &clipboard,
            "Import GGUF model",
            IDOK.0,
            Some(model.clone()),
            |s, c| click(s, MODEL_IMPORT, c),
        )?;
        until(&mut shell, &clipboard, Duration::from_secs(180), |s| {
            s.operations.is_empty() && s.welcome.as_ref().is_some_and(|w| w.can_continue())
        })?;
        ensure!(
            shell.controller.config().custom_instructions == saved_instructions
                && crate::config::load_config()?.custom_instructions == saved_instructions
                && shell.draft.value("custom_instructions")
                    == Some(&json!("Unsaved model-selection draft: 😀 e\u{301}\n")),
            "Model selection saved or discarded the independent instruction draft."
        );
        passed(
            receipt,
            "verified-model-receipt-preserves-independent-instruction-draft",
        );
        shell.draft.reset(&shell.controller.config())?;
        shell.refresh_controls()?;
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
        assert_preview_end_caret(&shell)?;
        wait_preview(
            &mut shell,
            &clipboard,
            output,
            receipt,
            "preview-english-timeout",
        )?;
        let preview_config = &shell.preview.as_ref().unwrap().config;
        ensure!(
            !preview_config.clipboard_context
                && !preview_config.screen_context
                && !preview_config.screenshot_context
                && !preview_config.screenshot_appearance
                && !preview_config.record_history
                && !preview_config.use_history
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
        assert_preview_end_caret(&shell)?;
        wait_preview(
            &mut shell,
            &clipboard,
            output,
            receipt,
            "preview-portuguese-timeout",
        )?;
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
    owned_dialog_seen: bool,
    matched_dialog: bool,
    foreground: bool,
    button_enabled: bool,
    filename_edit_present: bool,
    filename_combo_present: bool,
    filename_edit_matches: bool,
    filename_combo_matches: bool,
    controls: Vec<(i32, String, bool, bool)>,
}
unsafe extern "system" fn observe_dialog_control(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        let controls = &mut *(param.0 as *mut Vec<(i32, String, bool, bool)>);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() && controls.len() < 256 {
            let mut class = [0u16; 64];
            let n = GetClassNameW(hwnd, &mut class);
            controls.push((
                GetDlgCtrlID(hwnd),
                String::from_utf16_lossy(&class[..n.max(0) as usize]),
                IsWindowVisible(hwnd).as_bool(),
                IsWindowEnabled(hwnd).as_bool(),
            ));
        }
        BOOL(i32::from(controls.len() < 256))
    }
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
        request.owned_dialog_seen = true;
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
        request.matched_dialog = true;
        request.foreground = GetForegroundWindow() == hwnd;
        if request.controls.is_empty() {
            let _ = EnumChildWindows(
                Some(hwnd),
                Some(observe_dialog_control),
                LPARAM((&mut request.controls as *mut Vec<(i32, String, bool, bool)>) as isize),
            );
        }
        if let Some(path) = &request.path {
            let Ok(combo) = GetDlgItem(Some(hwnd), cmb13 as i32) else {
                return BOOL(1);
            };
            request.filename_combo_present = true;
            // ComboBoxEx exposes its real filename Edit through this SDK message;
            // changing only the container caption does not enter a selected path.
            let edit = HWND(SendMessageW(combo, CBEM_GETEDITCONTROL, None, None).0 as *mut _);
            if owned(edit).is_err()
                || !IsWindowVisible(edit).as_bool()
                || !IsWindowEnabled(edit).as_bool()
            {
                return BOOL(1);
            }
            request.filename_edit_present = true;
            SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            SendMessageW(
                edit,
                EM_REPLACESEL,
                Some(WPARAM(1)),
                Some(LPARAM(path.as_ptr() as isize)),
            );
            let expected = String::from_utf16_lossy(&path[..path.len() - 1]);
            request.filename_edit_matches = text(edit) == expected;
            request.filename_combo_matches = text(combo) == expected;
            if !request.filename_edit_matches {
                return BOOL(1);
            }
        }
        if let Ok(control) = GetDlgItem(Some(hwnd), request.button) {
            request.button_enabled = IsWindowEnabled(control).as_bool();
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
            owned_dialog_seen: false,
            matched_dialog: false,
            foreground: false,
            button_enabled: false,
            filename_edit_present: false,
            filename_combo_present: false,
            filename_edit_matches: false,
            filename_combo_matches: false,
            controls: Vec::new(),
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
                return request;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        request
    });
    let result = action(shell, clipboard);
    done.store(true, Ordering::Release);
    let response = responder
        .join()
        .map_err(|_| anyhow::anyhow!("The owned dialog responder failed."));
    // Preserve a real picker/action failure rather than replacing it with a
    // secondary missing-dialog assertion.
    let result = result?;
    let response = response?;
    ensure!(
        response.responded && !response.emergency,
        "The expected owned native dialog did not complete: {}",
        json!({
            "expected_dialog": response.title,
            "owned_dialog_seen": response.owned_dialog_seen,
            "matched_dialog": response.matched_dialog,
            "foreground": response.foreground,
            "button_enabled": response.button_enabled,
            "click_dispatched": response.responded,
            "deadline": response.emergency,
            "filename_edit_present": response.filename_edit_present,
            "filename_combo_present": response.filename_combo_present,
            "filename_edit_matches": response.filename_edit_matches,
            "filename_combo_matches": response.filename_combo_matches,
            "controls_id_class_visible_enabled": response.controls,
        })
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
        // Direct DIB access must wait for the calling thread's GDI batch.
        // https://learn.microsoft.com/windows/win32/api/wingdi/nf-wingdi-createdibsection
        ensure!(
            GdiFlush().as_bool(),
            "Owned screenshot drawing did not complete successfully."
        );
        let bytes =
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), width as usize * height as usize * 4);
        let nonblank = bytes.chunks_exact(4).any(|pixel| pixel[..3] != bytes[..3]);
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
        // Preserve the actual failed bitmap for diagnosis; it is not UI proof.
        ensure!(
            nonblank,
            "The native screenshot is blank; failed pixels were retained, not UI rendering proof."
        );
        Ok(())
    }
}
