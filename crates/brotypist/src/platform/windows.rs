//! Windows UIA/Win32 owner. No clipboard, SetValue, selection-unit guessing,
//! injected-shortcut interception, privilege escalation, or permission prompts.
//! UIA interfaces are created, used and released inside one balanced COM scope.
use super::{ContextRead, DetachedContextRead, EditOutcome, Foreground, NativeBridge, Permission, ShortcutHandler, policy::ObservationPolicy};
use crate::{config::Config, core::{InlineRender, Rect, Snapshot, TextBuffer, Utf16Offset}};
use anyhow::{Result, anyhow, ensure};
use std::{marker::PhantomData, mem::size_of, rc::Rc, sync::{Arc, atomic::Ordering}, time::{Duration, Instant}};
use windows::{core::{BSTR, Interface, PCWSTR, PWSTR, BOOL}, Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM},
    Graphics::Gdi::*,
    Security::*,
    System::{Com::*, Ole::*, Threading::*, Variant::*},
    UI::{Accessibility::*, HiDpi::GetDpiForWindow, Input::{Ime::*, KeyboardAndMouse::*}, WindowsAndMessaging::*},
}};
#[path = "windows_decisions.rs"] mod decisions;
#[path = "windows_hook.rs"] mod hook;
use decisions::{MAX_TEXT, Readback, classify_readback, provider_selection, replacement_units};
use hook::{HookOwner, HookShared};

const EM_GETSEL_MESSAGE: u32 = 0x00b0;
const EM_SETSEL_MESSAGE: u32 = 0x00b1;
const EM_REPLACESEL_MESSAGE: u32 = 0x00c2;
const EM_POSFROMCHAR_MESSAGE: u32 = 0x00d6;
const EM_LINEFROMCHAR_MESSAGE: u32 = 0x00c9;
const READY: &str = "Windows UI Automation ready. Native Edit and writable UIA TextPattern/TextEdit providers only. Password, read-only, disabled, composing and higher-integrity targets are excluded. Inline requires verified UIA glyph/font and GDI metrics. Browser domains are not observable; domain-denied browsers fail closed. Windows publication and native compatibility verification remain held.";

struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> { unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?; } Ok(Self) }
}
impl Drop for Apartment { fn drop(&mut self) { unsafe { CoUninitialize(); } } }
fn automation<T>(operation: impl FnOnce(&IUIAutomation) -> Result<T>) -> Result<T> {
    let _apartment = Apartment::enter()?;
    let client: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    operation(&client)
}
struct Handle(HANDLE);
impl Drop for Handle { fn drop(&mut self) { unsafe { let _ = CloseHandle(self.0); } } }
struct Array(*mut SAFEARRAY);
impl Drop for Array { fn drop(&mut self) { if !self.0.is_null() { unsafe { let _ = SafeArrayDestroy(self.0); } } } }
impl Array {
    fn bounds(&self, expected: VARENUM, maximum: usize) -> Result<std::ops::RangeInclusive<i32>> {
        unsafe {
            ensure!(!self.0.is_null() && SafeArrayGetDim(self.0) == 1, "Invalid provider array");
            ensure!(SafeArrayGetVartype(self.0)? == expected, "Unexpected provider array type");
            let first = SafeArrayGetLBound(self.0, 1)?; let last = SafeArrayGetUBound(self.0, 1)?;
            ensure!(i64::from(last) - i64::from(first) + 1 >= 0 && i64::from(last) - i64::from(first) < maximum as i64, "Oversized provider array");
            Ok(first..=last)
        }
    }
    fn ints(&self, maximum: usize) -> Result<Vec<i32>> {
        let bounds = self.bounds(VT_I4, maximum)?;
        let mut result = Vec::new();
        for index in bounds { let mut value = 0i32; unsafe { SafeArrayGetElement(self.0, &index, (&mut value as *mut i32).cast())?; } result.push(value); }
        Ok(result)
    }
    fn rect(&self) -> Result<Option<Rect>> {
        let bounds = self.bounds(VT_R8, 512)?;
        if bounds.clone().count() != 4 { return Ok(None); }
        let mut rect = [0.0; 4];
        for (slot, index) in rect.iter_mut().zip(bounds) { unsafe { SafeArrayGetElement(self.0, &index, (slot as *mut f64).cast())?; } }
        Ok((rect.iter().all(|value| value.is_finite()) && rect[2] >= 0.0 && rect[3] > 0.0).then_some(rect))
    }
}
fn strict_string(value: &BSTR) -> Result<String> { Ok(String::from_utf16(value.as_wide())?) }
fn integer(value: &VARIANT) -> Option<i32> {
    matches!(value.vt(), VT_I4 | VT_I2 | VT_UI1 | VT_UI2 | VT_INT).then(|| i32::try_from(value).ok()).flatten()
}
fn boolean(value: &VARIANT) -> Option<bool> {
    if value.vt() == VT_BOOL { bool::try_from(value).ok() } else { integer(value).filter(|value| matches!(*value, 0 | 1)).map(|value| value != 0) }
}
fn number(value: &VARIANT) -> Option<f64> {
    matches!(value.vt(), VT_R8 | VT_R4 | VT_I4 | VT_I2).then(|| f64::try_from(value).ok()).flatten().filter(|value| value.is_finite())
}
fn variant_string(value: &VARIANT) -> Option<String> {
    if value.vt() != VT_BSTR { return None; }
    unsafe { strict_string(&value.Anonymous.Anonymous.Anonymous.bstrVal).ok() }
}
fn rectangle(rect: RECT) -> Option<Rect> {
    (rect.right > rect.left && rect.bottom > rect.top).then_some([rect.left as f64, rect.top as f64, (rect.right - rect.left) as f64, (rect.bottom - rect.top) as f64])
}
fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let left = a[0].max(b[0]); let top = a[1].max(b[1]);
    let right = (a[0] + a[2]).min(b[0] + b[2]); let bottom = (a[1] + a[3]).min(b[1] + b[3]);
    (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
}
fn native_message(hwnd: HWND, message: u32, wparam: usize, lparam: isize) -> Result<usize> {
    let mut output = 0;
    let status = unsafe { SendMessageTimeoutW(hwnd, message, WPARAM(wparam), LPARAM(lparam), SMTO_BLOCK | SMTO_ABORTIFHUNG, 200, Some(&mut output)) };
    ensure!(status.0 != 0, "Target edit timed out or Windows integrity isolation refused its message");
    Ok(output)
}
fn process_name(pid: u32) -> Result<String> {
    unsafe {
        let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?);
        let mut buffer = vec![0u16; 32768]; let mut length = buffer.len() as u32;
        QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length)?;
        Ok(String::from_utf16(&buffer[..length as usize])?)
    }
}
fn integrity(process: HANDLE) -> Result<u32> {
    unsafe {
        let mut token = HANDLE::default(); OpenProcessToken(process, TOKEN_QUERY, &mut token)?;
        let token = Handle(token); let mut bytes = 0;
        let _ = GetTokenInformation(token.0, TokenIntegrityLevel, None, 0, &mut bytes);
        ensure!((size_of::<TOKEN_MANDATORY_LABEL>() as u32..=65536).contains(&bytes), "Integrity unavailable");
        // usize allocation provides mandatory-label pointer alignment.
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(token.0, TokenIntegrityLevel, Some(buffer.as_mut_ptr().cast()), bytes, &mut bytes)?;
        let label = &*(buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>());
        ensure!(IsValidSid(label.Label.Sid).as_bool(), "Invalid integrity SID");
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        ensure!(count > 0, "Empty integrity SID");
        Ok(*GetSidSubAuthority(label.Label.Sid, u32::from(count - 1)))
    }
}
fn integrity_allowed(pid: u32) -> Result<bool> {
    unsafe { let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?); Ok(integrity(process.0)? <= integrity(GetCurrentProcess())?) }
}
struct Focus { window: HWND, info: GUITHREADINFO, pid: u32, app: String }
fn focus() -> Result<Option<Focus>> {
    unsafe {
        let window = GetForegroundWindow(); let mut pid = 0;
        let thread = GetWindowThreadProcessId(window, Some(&mut pid));
        if thread == 0 || pid == GetCurrentProcessId() { return Ok(None); }
        let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        GetGUIThreadInfo(thread, &mut info)?;
        if info.hwndFocus.0.is_null() { return Ok(None); }
        Ok(Some(Focus { window, info, pid, app: process_name(pid)? }))
    }
}
fn provider_id(element: &IUIAutomationElement, pid: u32) -> Result<String> {
    let values = Array(unsafe { element.GetRuntimeId()? }).ints(128)?;
    ensure!(!values.is_empty(), "Provider has no stable runtime identity");
    Ok(format!("{pid}:uia:{}", values.iter().map(i32::to_string).collect::<Vec<_>>().join(":")))
}
fn no_provider_composition(edit: &IUIAutomationTextEditPattern) -> Result<bool> {
    unsafe {
        let mut range = std::ptr::null_mut();
        // GetActiveComposition returning S_OK + NULL is the documented inactive
        // state. The generated nonnullable return wrapper conflates it with failure.
        (Interface::vtable(edit).GetActiveComposition)(Interface::as_raw(edit), &mut range).ok()?;
        if range.is_null() { return Ok(true); }
        drop(IUIAutomationTextRange::from_raw(range));
        Ok(false)
    }
}
fn native_composing(hwnd: HWND) -> Result<bool> {
    unsafe {
        let context = ImmGetContext(hwnd);
        if context.0.is_null() { return Ok(false); }
        let length = ImmGetCompositionStringW(context, GCS_COMPSTR, None, 0);
        let released = ImmReleaseContext(hwnd, context);
        ensure!(released.as_bool() && length != -2, "IME composition state unavailable");
        Ok(length > 0)
    }
}
struct Captured { snapshot: Snapshot, hwnd: Option<HWND>, selected: Option<IUIAutomationTextRange> }

pub struct WindowsBridge {
    policy: Arc<ObservationPolicy>,
    shared: Arc<HookShared>,
    hook: Option<HookOwner>,
    stopped: bool,
    diagnostic: String,
    last: Option<Snapshot>,
    _apartment_owner: PhantomData<Rc<()>>,
}
impl WindowsBridge {
    pub fn new() -> Result<Self> {
        // Construction does not install hooks, register integration or ask for access.
        automation(|_| Ok(()))?;
        let policy = Arc::new(ObservationPolicy::new(&Config::default())?);
        Ok(Self::with_policy(policy))
    }
    fn with_policy(policy: Arc<ObservationPolicy>) -> Self {
        Self { shared: Arc::new(HookShared::new(policy.clone())), policy, hook: None, stopped: false, diagnostic: READY.into(), last: None, _apartment_owner: PhantomData }
    }
    fn publish(&self, app: &str, window: HWND) {
        let changed = if let Ok(mut published) = self.shared.published.lock() {
            let changed = published.app != app || published.foreground != window.0 as usize;
            if changed { published.app = app.into(); published.foreground = window.0 as usize; }
            changed
        } else { false };
        if changed { self.shared.note(true); }
    }
    fn capture_raw(&self, client: &IUIAutomation) -> Result<Option<Captured>> {
        self.capture_with_render(client, true)
    }
    fn capture_with_render(&self, client: &IUIAutomation, include_render: bool) -> Result<Option<Captured>> {
        unsafe {
            if self.stopped { return Ok(None); }
            let Some(focus) = focus()? else { return Ok(None) };
            self.publish(&focus.app, focus.window);
            if !self.policy.allowed(&focus.app, "", &[], false) || !integrity_allowed(focus.pid)? { return Ok(None); }
            let element = client.GetFocusedElement()?;
            // Privacy and writability precede ALL document/value/range reads.
            if !decisions::readable_element(element.CurrentIsPassword()?.as_bool(), element.CurrentIsEnabled()?.as_bool(),
                element.CurrentHasKeyboardFocus()?.as_bool(), element.CurrentIsOffscreen()?.as_bool(),
                element.CurrentProcessId()?, focus.pid as i32, element.CurrentControlType()?.0) { return Ok(None); }
            let hwnd = focus.info.hwndFocus;
            let mut class = [0u16; 256]; let class_length = GetClassNameW(hwnd, &mut class);
            let class = String::from_utf16(&class[..class_length.max(0) as usize])?.to_ascii_lowercase();
            let native = element.CurrentNativeWindowHandle()? == hwnd && (class == "edit" || class.starts_with("windowsforms10.edit."));
            let mut captured = if native {
                let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                if style & (0x0020 | 0x0800) != 0 || !IsWindowEnabled(hwnd).as_bool() { return Ok(None); }
                let value: IUIAutomationValuePattern = element.GetCurrentPatternAs(UIA_ValuePatternId)?;
                if value.CurrentIsReadOnly()?.as_bool() || native_composing(hwnd)? { return Ok(None); }
                let length = native_message(hwnd, WM_GETTEXTLENGTH, 0, 0)?;
                if length > MAX_TEXT { return Ok(None); }
                let mut buffer = vec![0u16; length + 1];
                let copied = native_message(hwnd, WM_GETTEXT, buffer.len(), buffer.as_mut_ptr() as isize)?;
                if copied != length || buffer[length] != 0 { return Ok(None); }
                let observed: TextBuffer = String::from_utf16(&buffer[..length])?.into();
                let mut start = 0u32; let mut end = 0u32;
                native_message(hwnd, EM_GETSEL_MESSAGE, (&mut start as *mut u32) as usize, (&mut end as *mut u32) as isize)?;
                let Some(caret) = observed.index().utf16_to_scalar(Utf16Offset(start as usize)) else { return Ok(None) };
                let Some(last) = observed.index().utf16_to_scalar(Utf16Offset(end as usize)) else { return Ok(None) };
                let Some(selection_length) = last.0.checked_sub(caret.0) else { return Ok(None) };
                let caret_rect = if focus.info.hwndCaret == hwnd && focus.info.rcCaret.bottom > focus.info.rcCaret.top {
                    let rect = focus.info.rcCaret; let mut point = POINT { x: rect.left, y: rect.top };
                    ClientToScreen(hwnd, &mut point).as_bool().then_some([point.x as f64, point.y as f64, (rect.right - rect.left).max(1) as f64, (rect.bottom - rect.top) as f64])
                } else { None };
                let selected = element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).and_then(|pattern| pattern.GetSelection()).ok().and_then(|selection| (selection.Length().ok()? == 1).then(|| selection.GetElement(0).ok()).flatten());
                Captured { snapshot: Snapshot { app_id: focus.app.clone(), field_id: format!("{}:{}", focus.pid, hwnd.0 as usize), text: observed, caret: caret.0, selection_length, caret_rect, multiline: style & 4 != 0, ..Default::default() }, hwnd: Some(hwnd), selected }
            } else {
                let edit: IUIAutomationTextEditPattern = element.GetCurrentPatternAs(UIA_TextEditPatternId)?;
                if !no_provider_composition(&edit)? { return Ok(None); }
                let pattern: IUIAutomationTextPattern = element.GetCurrentPatternAs(UIA_TextPatternId)?;
                let document = pattern.DocumentRange()?;
                let read_only = match element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                    Ok(value) => Some(value.CurrentIsReadOnly()?.as_bool()),
                    Err(_) => boolean(&document.GetAttributeValue(UIA_IsReadOnlyAttributeId)?),
                };
                if read_only != Some(false) { return Ok(None); }
                let selections = pattern.GetSelection()?;
                if selections.Length()? != 1 { return Ok(None); }
                let selected = selections.GetElement(0)?;
                let prefix = document.Clone()?;
                prefix.MoveEndpointByRange(TextPatternRangeEndpoint_End, &selected, TextPatternRangeEndpoint_Start)?;
                let through = document.Clone()?;
                through.MoveEndpointByRange(TextPatternRangeEndpoint_End, &selected, TextPatternRangeEndpoint_End)?;
                // Two UTF-16 units per scalar; never truncate a valid maximum-size document.
                let prefix = strict_string(&prefix.GetText((MAX_TEXT * 2 + 1) as i32)?)?;
                let through = strict_string(&through.GetText((MAX_TEXT * 2 + 1) as i32)?)?;
                let text = strict_string(&document.GetText((MAX_TEXT * 2 + 1) as i32)?)?;
                let Some((caret, selection_length)) = provider_selection(&text, &prefix, &through) else { return Ok(None) };
                let mut caret_rect = Array(selected.GetBoundingRectangles()?).rect()?;
                if caret_rect.is_none() {
                    if let Ok(second) = element.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id) {
                        let mut active = BOOL(0);
                        if let Ok(range) = second.GetCaretRange(&mut active) {
                            if !active.as_bool() || range.CompareEndpoints(TextPatternRangeEndpoint_Start, &selected, TextPatternRangeEndpoint_Start)? != 0 { return Ok(None); }
                            caret_rect = Array(range.GetBoundingRectangles()?).rect()?;
                        }
                    }
                }
                if let Some(rect) = caret_rect.as_mut() { rect[2] = rect[2].max(1.0); }
                Captured { snapshot: Snapshot { app_id: focus.app.clone(), field_id: provider_id(&element, focus.pid)?, text: text.into(), caret, selection_length, caret_rect, ..Default::default() }, hwnd: None, selected: Some(selected) }
            };
            captured.snapshot.window_id = format!("{}:{}", focus.pid, focus.window.0 as usize);
            captured.snapshot.window_revision = format!("{}", focus.window.0 as usize);
            captured.snapshot.window_pid = focus.pid as i32;
            captured.snapshot.field_rect = rectangle(element.CurrentBoundingRectangle()?);
            if include_render && captured.snapshot.selection_length == 0 {
                captured.snapshot.inline_render = captured.selected.as_ref().and_then(|selected| inline_metrics(client, &element, selected, &captured.snapshot, focus.window).ok().flatten());
            }
            // Recheck focus and provider authority after potentially slow calls.
            if GetForegroundWindow() != focus.window || !identity_matches(client, &captured.snapshot)? { return Ok(None); }
            Ok(Some(captured))
        }
    }
    fn mutation(&mut self, before: &Snapshot, start: usize, length: usize, text: &str) -> Result<EditOutcome> {
        let Some((unit_start, unit_end)) = replacement_units(before, start, length, text) else { return Ok(EditOutcome::Rejected) };
        if self.stopped { return Ok(EditOutcome::Rejected); }
        let mut attempted = false;
        let result = automation(|client| {
            let Some(current) = self.capture_raw(client)? else { return Ok(EditOutcome::Rejected) };
            if !current.snapshot.same_key(before) || !current.snapshot.same_authority(before) { return Ok(EditOutcome::Rejected); }
            let synchronous = current.hwnd.is_some();
            if let Some(hwnd) = current.hwnd {
                if (start, length) != (before.caret, before.selection_length) {
                    // Selection itself is an effect; a timed-out setter is not retried.
                    if !self.shared.begin_effect() { return Ok(EditOutcome::Rejected); }
                    attempted = true;
                    native_message(hwnd, EM_SETSEL_MESSAGE, unit_start, unit_end as isize)?;
                    let Some(selected) = self.capture_raw(client)? else { return Ok(EditOutcome::Indeterminate) };
                    let mut expected = before.clone(); expected.caret = start; expected.selection_length = length;
                    if !selected.snapshot.same_key(&expected) || !selected.snapshot.same_authority(before) { return Ok(EditOutcome::Indeterminate); }
                }
                // All messages preserve native Edit undo history (TRUE).
                let buffer: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                if !self.shared.begin_effect() { return Ok(if attempted { EditOutcome::Indeterminate } else { EditOutcome::Rejected }); }
                attempted = true;
                native_message(hwnd, EM_REPLACESEL_MESSAGE, 1, buffer.as_ptr() as isize)?;
            } else {
                if (start, length) != (before.caret, before.selection_length) {
                    return Ok(EditOutcome::Rejected); // provider Character units are not scalar/UTF-16 promises
                }
                if !identity_matches(client, before)? { return Ok(EditOutcome::Rejected); }
                if [0x11, 0x12, 0x5b, 0x5c].into_iter().any(|key| unsafe { GetAsyncKeyState(key) } < 0) { return Ok(EditOutcome::Rejected); }
                let events = unicode_events(text);
                if !self.shared.begin_effect() { return Ok(EditOutcome::Rejected); }
                attempted = true;
                let delivered = unsafe { SendInput(&events, size_of::<INPUT>() as i32) } as usize;
                if delivered == 0 { return Ok(EditOutcome::Rejected); }
                if delivered != events.len() { return Ok(EditOutcome::Indeterminate); }
            }
            let Some(after) = self.capture_raw(client)? else { return Ok(EditOutcome::Indeterminate) };
            Ok(match classify_readback(before, &after.snapshot, start, length, text) {
                Readback::Applied => EditOutcome::Applied(after.snapshot),
                Readback::Unchanged if synchronous => EditOutcome::Rejected,
                Readback::Unchanged => EditOutcome::Dispatched,
                Readback::Uncertain => EditOutcome::Indeterminate,
            })
        });
        match result {
            Ok(outcome) => { if attempted { self.shared.note(false); } Ok(outcome) }
            Err(_) => { self.diagnostic = "Windows edit refused or uncertain; no automatic retry is permitted.".into(); if attempted { self.shared.note(false); } Ok(if attempted { EditOutcome::Indeterminate } else { EditOutcome::Rejected }) }
        }
    }
}

fn identity_matches(client: &IUIAutomation, snapshot: &Snapshot) -> Result<bool> {
    unsafe {
        let Some(focus) = focus()? else { return Ok(false) };
        if focus.app != snapshot.app_id || format!("{}:{}", focus.pid, focus.window.0 as usize) != snapshot.window_id
            || focus.pid as i32 != snapshot.window_pid { return Ok(false); }
        let element = client.GetFocusedElement()?;
        if element.CurrentIsPassword()?.as_bool() || element.CurrentIsOffscreen()?.as_bool() || !element.CurrentIsEnabled()?.as_bool()
            || !element.CurrentHasKeyboardFocus()?.as_bool() || element.CurrentProcessId()? != snapshot.window_pid { return Ok(false); }
        if snapshot.field_id.contains(":uia:") { Ok(provider_id(&element, focus.pid)? == snapshot.field_id) }
        else { Ok(element.CurrentNativeWindowHandle()? == focus.info.hwndFocus && format!("{}:{}", focus.pid, focus.info.hwndFocus.0 as usize) == snapshot.field_id) }
    }
}
fn unicode_events(text: &str) -> Vec<INPUT> {
    let mut events = Vec::with_capacity(text.encode_utf16().count() * 2);
    for unit in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            events.push(INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 } } });
        }
    }
    events
}

impl NativeBridge for WindowsBridge {
    fn configure(&mut self, config: &Config) -> Result<()> {
        self.policy = Arc::new(ObservationPolicy::new(config)?);
        self.shared.published.lock().map_err(|_| anyhow!("Windows policy publication poisoned"))?.policy = self.policy.clone();
        Ok(())
    }
    fn capture(&mut self) -> Result<Option<Snapshot>> {
        let captured = match automation(|client| Ok(self.capture_raw(client)?.map(|captured| captured.snapshot))) {
            Ok(value) => value,
            Err(_) => { self.diagnostic = "Windows capture unavailable: provider, composition, privacy or integrity evidence was incomplete.".into(); None }
        };
        if self.last.as_ref().map(|last| captured.as_ref().is_none_or(|value| !last.same_key(value) || !last.same_authority(value))).unwrap_or(captured.is_some()) { self.shared.note(false); }
        self.last = captured.clone();
        Ok(captured)
    }
    fn foreground(&mut self) -> Result<Option<Foreground>> {
        unsafe {
            let window = GetForegroundWindow(); let mut pid = 0;
            GetWindowThreadProcessId(window, Some(&mut pid));
            if pid == 0 || pid == GetCurrentProcessId() { self.publish("", HWND::default()); return Ok(None); }
            let executable = process_name(pid)?;
            self.publish(&executable, window);
            Ok(Some(Foreground { app_id: executable.clone(), executable, domain: String::new(), window_id: format!("{pid}:{}", window.0 as usize), window_pid: pid as i32, ..Default::default() }))
        }
    }
    fn insert(&mut self, snapshot: &Snapshot, text: &str) -> Result<EditOutcome> {
        if snapshot.selection_length != 0 { return Ok(EditOutcome::Rejected); }
        self.mutation(snapshot, snapshot.caret, 0, text)
    }
    fn replace_range(&mut self, snapshot: &Snapshot, start: usize, length: usize, text: &str) -> Result<EditOutcome> { self.mutation(snapshot, start, length, text) }
    fn range_rect(&mut self, snapshot: &Snapshot, start: usize, length: usize) -> Result<Option<Rect>> {
        let Some((first, last)) = replacement_units(snapshot, start, length, "x") else { return Ok(None) };
        automation(|client| {
            let Some(current) = self.capture_raw(client)? else { return Ok(None) };
            if !current.snapshot.same_key(snapshot) || !current.snapshot.same_authority(snapshot) { return Ok(None); }
            if let Some(hwnd) = current.hwnd {
                if native_message(hwnd, EM_LINEFROMCHAR_MESSAGE, first, 0)? != native_message(hwnd, EM_LINEFROMCHAR_MESSAGE, last, 0)? { return Ok(None); }
                let p0 = native_message(hwnd, EM_POSFROMCHAR_MESSAGE, first, 0)?;
                let p1 = native_message(hwnd, EM_POSFROMCHAR_MESSAGE, last, 0)?;
                if p0 as i32 == -1 || p1 as i32 == -1 { return Ok(None); }
                let mut left = POINT { x: (p0 as u16 as i16) as i32, y: ((p0 >> 16) as u16 as i16) as i32 };
                let mut right = POINT { x: (p1 as u16 as i16) as i32, y: ((p1 >> 16) as u16 as i16) as i32 };
                if left.y != right.y || right.x < left.x { return Ok(None); }
                let Some(caret) = current.snapshot.caret_rect else { return Ok(None) };
                unsafe { if !ClientToScreen(hwnd, &mut left).as_bool() || !ClientToScreen(hwnd, &mut right).as_bool() { return Ok(None); } }
                let rect = [left.x as f64, left.y as f64, (right.x - left.x).max(1) as f64, caret[3]];
                if current.snapshot.field_rect.and_then(|clip| intersection(rect, clip)) != Some(rect) { return Ok(None); }
                Ok(Some(rect))
            } else if (start, length) == (snapshot.caret, snapshot.selection_length) {
                current.selected.map(|selected| Array(unsafe { selected.GetBoundingRectangles()? }).rect()).transpose().map(Option::flatten)
            } else { Ok(None) }
        }).or(Ok(None))
    }
    fn focused_context(&mut self, snapshot: &Snapshot, authorized: &dyn Fn() -> bool, max_chars: usize) -> Result<ContextRead> {
        if self.stopped { return Ok(ContextRead::blocked("accessibility")); }
        read_context(snapshot, &self.policy, authorized, max_chars)
    }
    fn context_ticket(&mut self, snapshot: &Snapshot) -> Result<Option<Box<dyn DetachedContextRead>>> {
        if !self.context_matches(snapshot)? { return Ok(None); }
        Ok(Some(Box::new(WindowsContextRead { snapshot: snapshot.clone(), policy: self.policy.clone() })))
    }
    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> {
        if self.stopped || !self.policy.allowed(&snapshot.app_id, "", &[], false) { return Ok(false); }
        Ok(automation(|client| identity_matches(client, snapshot)).unwrap_or(false))
    }
    fn start_shortcuts(&mut self, callback: ShortcutHandler) -> Result<()> {
        ensure!(!self.stopped, "Windows bridge has stopped");
        self.shared.published.lock().map_err(|_| anyhow!("Windows callback publication poisoned"))?.callback = Some(callback);
        if self.hook.is_none() { self.hook = Some(HookOwner::start(self.shared.clone())?); }
        Ok(())
    }
    fn set_shortcuts_enabled(&mut self, enabled: bool) { self.shared.enabled.store(enabled && !self.stopped, Ordering::Release); }
    fn set_input_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) { if let Ok(mut published) = self.shared.published.lock() { published.input_callback = callback; } }
    fn finish_input(&mut self, accepted: bool) -> Result<()> { self.shared.finish_claim(accepted); Ok(()) }
    fn input_claim_valid(&self) -> bool { !self.stopped && self.shared.activate_claim() }
    fn input_revision(&self) -> u64 { self.shared.revision.load(Ordering::Acquire) }
    fn user_input_revision(&self) -> u64 { self.shared.user_revision.load(Ordering::Acquire) }
    fn input_timestamp(&self) -> Option<Instant> { self.shared.timestamp.lock().ok().and_then(|value| *value) }
    fn secure_input_enabled(&self) -> Option<bool> {
        // Query current focus on the owner thread; never trust a stale last-capture flag.
        automation(|client| unsafe {
            let Some(focus) = focus()? else { return Err(anyhow!("No observable foreground focus")); };
            let element = client.GetFocusedElement()?;
            ensure!(element.CurrentProcessId()? == focus.pid as i32 && element.CurrentHasKeyboardFocus()?.as_bool(), "Focus is not observable");
            Ok(element.CurrentIsPassword()?.as_bool())
        }).ok()
    }
    fn request_permission(&mut self, kind: &str) -> Result<Permission> {
        // UIA/UIPI have no legitimate in-app privilege bypass or consent popup.
        Ok(match kind {
            "accessibility" | "automation" => if automation(|_| Ok(())).is_ok() { Permission::Allowed } else { Permission::Denied },
            "input" | "keyboard" | "input_monitoring" if self.hook.is_some() && !self.stopped => Permission::Allowed,
            _ => Permission::Unknown,
        })
    }
    fn status(&self) -> serde_json::Value {
        let hook_diagnostic = self.shared.diagnostic.lock().ok().map(|value| value.clone()).unwrap_or_default();
        serde_json::json!({"platform":"windows", "message":self.diagnostic, "hook_diagnostic":hook_diagnostic, "hook_running":self.hook.is_some(), "stopped":self.stopped, "domain_observable":false, "native_edit":true, "uia_existing_selection_only":true, "shortcut_claim_timeout_ms":500, "publication":"held"})
    }
    fn stop(&mut self) -> Result<()> {
        self.stopped = true; self.shared.enabled.store(false, Ordering::Release);
        self.shared.cancel_claim();
        self.shared.finish_claim(false); // closed owner cannot execute a queued claim after stop
        if let Some(hook) = self.hook.as_mut() { hook.stop(&self.shared)?; }
        self.hook = None;
        if let Ok(mut published) = self.shared.published.lock() { published.callback = None; published.input_callback = None; }
        self.last = None;
        Ok(())
    }
}
impl Drop for WindowsBridge {
    fn drop(&mut self) {
        // Failed ACK retains callback ownership in the message thread's Arc/TLS,
        // never frees native callback storage under an installed system hook.
        let _ = self.stop();
    }
}

fn inline_metrics(client: &IUIAutomation, element: &IUIAutomationElement, selected: &IUIAutomationTextRange, snapshot: &Snapshot, window: HWND) -> Result<Option<InlineRender>> {
    unsafe {
        let Some(caret) = snapshot.caret_rect else { return Ok(None) };
        if snapshot.caret == 0 || snapshot.selection_length != 0 || (!snapshot.suffix().is_empty() && !snapshot.suffix().starts_with(['\r', '\n'])) { return Ok(None); }
        let run = selected.Clone()?;
        run.MoveEndpointByRange(TextPatternRangeEndpoint_End, selected, TextPatternRangeEndpoint_Start)?;
        if run.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1)? != -1 { return Ok(None); }
        let Some(previous) = snapshot.text.slice(snapshot.caret - 1, 1) else { return Ok(None) };
        if strict_string(&run.GetText(8)?)? != previous { return Ok(None); }
        let Some(name) = variant_string(&run.GetAttributeValue(UIA_FontNameAttributeId)?) else { return Ok(None) };
        let Some(size) = number(&run.GetAttributeValue(UIA_FontSizeAttributeId)?) else { return Ok(None) };
        let Some(weight) = integer(&run.GetAttributeValue(UIA_FontWeightAttributeId)?) else { return Ok(None) };
        let Some(italic) = boolean(&run.GetAttributeValue(UIA_IsItalicAttributeId)?) else { return Ok(None) };
        if integer(&run.GetAttributeValue(UIA_TextFlowDirectionsAttributeId)?) != Some(0)
            || boolean(&run.GetAttributeValue(UIA_IsHiddenAttributeId)?) != Some(false) || element.CurrentIsOffscreen()?.as_bool() { return Ok(None); }
        let Some(glyph) = Array(run.GetBoundingRectangles()?).rect()? else { return Ok(None) };
        let Some(mut clip) = rectangle(element.CurrentBoundingRectangle()?) else { return Ok(None) };
        let pattern: IUIAutomationTextPattern = element.GetCurrentPatternAs(UIA_TextPatternId)?;
        let visible = pattern.GetVisibleRanges()?; let mut observed = false;
        for index in 0..visible.Length()?.clamp(0, 128) {
            let range = visible.GetElement(index)?;
            if range.CompareEndpoints(TextPatternRangeEndpoint_Start, &run, TextPatternRangeEndpoint_Start)? <= 0
                && range.CompareEndpoints(TextPatternRangeEndpoint_End, &run, TextPatternRangeEndpoint_End)? >= 0 { observed = true; break; }
        }
        if !observed { return Ok(None); }
        let walker = client.ControlViewWalker()?; let mut parent = parent_element(&walker, element)?;
        let mut complete_clip = false;
        for _ in 0..16 {
            let Some(element) = parent else { complete_clip = true; break };
            if element.CurrentIsOffscreen()?.as_bool() { return Ok(None); }
            if let Some(bounds) = rectangle(element.CurrentBoundingRectangle()?) {
                let Some(intersected) = intersection(clip, bounds) else { return Ok(None) }; clip = intersected;
            }
            if element.CurrentControlType()? == UIA_WindowControlTypeId { complete_clip = true; break; }
            parent = parent_element(&walker, &element)?;
        }
        if !complete_clip { return Ok(None); }
        let dpi = GetDpiForWindow(window);
        if dpi == 0 || !(1.0..=300.0).contains(&size) || name.contains('\0') { return Ok(None); }
        let face: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let font = CreateFontW(-(size * dpi as f64 / 72.0).round_ties_even() as i32, 0, 0, 0, weight, u32::from(italic), 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY, 0, PCWSTR(face.as_ptr()));
        let dc = CreateCompatibleDC(None);
        if font.0.is_null() || dc.0.is_null() {
            if !font.0.is_null() { let _ = DeleteObject(font.into()); }
            if !dc.0.is_null() { let _ = DeleteDC(dc); }
            return Ok(None);
        }
        let previous_object = SelectObject(dc, font.into());
        if previous_object.0.is_null() || previous_object.0 as isize == -1 {
            let _ = DeleteObject(font.into()); let _ = DeleteDC(dc); return Ok(None);
        }
        let mut metrics = TEXTMETRICW::default(); let mut observed_face = [0u16; 256];
        let metrics_ok = GetTextMetricsW(dc, &mut metrics).as_bool();
        let face_length = GetTextFaceW(dc, Some(&mut observed_face));
        let _ = SelectObject(dc, previous_object); let _ = DeleteObject(font.into()); let _ = DeleteDC(dc);
        if !metrics_ok || face_length <= 0 { return Ok(None); }
        let face_end = observed_face.iter().position(|unit| *unit == 0).unwrap_or(observed_face.len());
        if crate::core::casefold(&String::from_utf16(&observed_face[..face_end])?) != crate::core::casefold(&name)
            || (glyph[3] - metrics.tmHeight as f64).abs() > 1.0 || (glyph[1] - caret[1]).abs() > 1.0
            || (glyph[0] + glyph[2] - caret[0]).abs() > 1.5 { return Ok(None); }
        let render = InlineRender { font_name: name, font_size: size, baseline: glyph[1] + metrics.tmAscent as f64, clip_rect: clip,
            ascent: metrics.tmAscent as f64, descent: metrics.tmDescent as f64, source: "windows-uia-font-run".into(), font_weight: weight, italic, ..Default::default() };
        Ok(render.valid(Some(caret)).then_some(render))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_sendinput_pairs_every_utf16_unit_without_virtual_shortcuts() {
        let events = unicode_events("a😀");
        assert_eq!(events.len(), 6);
        for (pair, unit) in events.chunks_exact(2).zip("a😀".encode_utf16()) {
            unsafe { assert_eq!(pair[0].Anonymous.ki.wVk, VIRTUAL_KEY(0)); assert_eq!(pair[0].Anonymous.ki.wScan, unit); assert_eq!(pair[1].Anonymous.ki.wScan, unit); assert_eq!(pair[0].Anonymous.ki.dwFlags, KEYEVENTF_UNICODE); assert_eq!(pair[1].Anonymous.ki.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP); }
        }
    }
    #[test]
    fn mixed_unsupported_and_wrong_typed_attributes_never_become_writable_or_font_evidence() {
        assert_eq!(boolean(&VARIANT::default()), None);
        assert_eq!(boolean(&VARIANT::from("false")), None);
        assert_eq!(integer(&VARIANT::from("400")), None);
        assert_eq!(number(&VARIANT::from(f64::NAN)), None);
        assert_eq!(boolean(&VARIANT::from(false)), Some(false));
        assert_eq!(variant_string(&VARIANT::from("Host Font")), Some("Host Font".into()));
    }
    #[test]
    fn clipping_intersection_refuses_empty_and_preserves_exact_row() {
        assert_eq!(intersection([0.,0.,100.,20.], [10.,5.,50.,30.]), Some([10.,5.,50.,15.]));
        assert_eq!(intersection([0.,0.,10.,10.], [10.,0.,10.,10.]), None);
    }
}

struct WindowsContextRead { snapshot: Snapshot, policy: Arc<ObservationPolicy> }
impl DetachedContextRead for WindowsContextRead {
    fn read(&self, authorized: &dyn Fn() -> bool, max_chars: usize) -> Result<ContextRead> {
        read_context(&self.snapshot, &self.policy, authorized, max_chars)
    }
    fn matches(&self) -> Result<bool> {
        if !self.policy.allowed(&self.snapshot.app_id, "", &[], false) { return Ok(false); }
        let reader = WindowsBridge::with_policy(self.policy.clone());
        Ok(automation(|client| {
            Ok(reader.capture_with_render(client, false)?.is_some_and(|current| current.snapshot.same_key(&self.snapshot) && current.snapshot.same_authority(&self.snapshot)))
        }).unwrap_or(false))
    }
    fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
        self.snapshot.same_key(snapshot) && self.snapshot.same_authority(snapshot)
    }
}
fn read_context(snapshot: &Snapshot, policy: &ObservationPolicy, authorized: &dyn Fn() -> bool, max_chars: usize) -> Result<ContextRead> {
    let source = "accessibility";
    if !authorized() || !policy.allowed(&snapshot.app_id, "", &[], false) { return Ok(ContextRead::blocked(source)); }
    let result = automation(|client| unsafe {
        if !authorized() || !identity_matches(client, snapshot)? { return Ok(ContextRead::blocked(source)); }
        let hwnd = GetForegroundWindow();
        let window = client.ElementFromHandle(hwnd)?; let walker = client.ControlViewWalker()?;
        let mut stack = vec![(window, 0)]; let mut chunks = Vec::new(); let mut remaining = max_chars.min(12000);
        let deadline = Instant::now() + Duration::from_millis(150);
        for _ in 0..128 {
            if remaining == 0 || Instant::now() >= deadline { break; }
            let Some((element, depth)) = stack.pop() else { break };
            if !authorized() || GetForegroundWindow() != hwnd { return Ok(ContextRead::blocked(source)); }
            if element.CurrentIsPassword()?.as_bool() || element.CurrentIsOffscreen()?.as_bool() { continue; }
            let role = element.CurrentControlType()?;
            if role == UIA_TextControlTypeId {
                if !authorized() { return Ok(ContextRead::blocked(source)); }
                let value: String = strict_string(&element.CurrentName()?)?.chars().take(remaining).collect();
                if !value.is_empty() && value != snapshot.text.as_ref() { remaining = remaining.saturating_sub(value.chars().count() + 1); chunks.push(value); }
            }
            if depth < 8 && !matches!(role, UIA_EditControlTypeId | UIA_DocumentControlTypeId) {
                let mut child = walker.GetFirstChildElement(&element).ok();
                for _ in 0..64 {
                    if !authorized() || Instant::now() >= deadline { break; }
                    let Some(element) = child else { break };
                    child = walker.GetNextSiblingElement(&element).ok();
                    if stack.len() < 128 { stack.push((element, depth + 1)); }
                }
            }
        }
        if !authorized() || !identity_matches(client, snapshot)? { return Ok(ContextRead::blocked(source)); }
        let text: String = chunks.join("\n").chars().take(max_chars.min(12000)).collect();
        Ok(ContextRead { status: if text.is_empty() { "empty" } else { "ready" }.into(), text, source: source.into() })
    });
    Ok(result.unwrap_or_else(|_| ContextRead { text: String::new(), status: "error".into(), source: source.into() }))
}

fn parent_element(walker: &IUIAutomationTreeWalker, element: &IUIAutomationElement) -> Result<Option<IUIAutomationElement>> {
    unsafe {
        let mut output = std::ptr::null_mut();
        (Interface::vtable(walker).GetParentElement)(Interface::as_raw(walker), Interface::as_raw(element), &mut output).ok()?;
        Ok((!output.is_null()).then(|| IUIAutomationElement::from_raw(output)))
    }
}
