//! Feature-gated verification of the real resident shell and its owned AppKit views.
//! No external Accessibility query, global input injection, permission request, or download.
use super::*;
use objc2_foundation::NSRange;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

fn ci() -> bool {
    std::env::var("CI").is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE"))
}
fn native_id(view: &AnyObject) -> String {
    unsafe {
        let supports: bool = msg_send![view,respondsToSelector:sel!(identifier)];
        if !supports {
            return String::new();
        }
        let id: *mut NSString = msg_send![view, identifier];
        if id.is_null() {
            String::new()
        } else {
            (&*id).to_string()
        }
    }
}
fn descendants(root: &AnyObject) -> Result<Vec<Obj>> {
    let mut stack = vec![(
        unsafe {
            Retained::retain(root as *const _ as *mut AnyObject)
                .context("Owned native root disappeared")?
        },
        0usize,
    )];
    let mut result = Vec::new();
    while let Some((view, depth)) = stack.pop() {
        ensure!(
            result.len() < 4096 && depth <= 32,
            "Owned view hierarchy exceeds verification bounds"
        );
        unsafe {
            let children: bool = msg_send![&*view,respondsToSelector:sel!(subviews)];
            if children {
                let children: *mut AnyObject = msg_send![&*view, subviews];
                let count: usize = msg_send![children, count];
                ensure!(
                    count <= 4096,
                    "Owned view children exceed verification bounds"
                );
                for index in (0..count).rev() {
                    let child: *mut AnyObject = msg_send![children,objectAtIndex:index];
                    if let Some(child) = Retained::retain(child) {
                        stack.push((child, depth + 1));
                    }
                }
            }
        }
        result.push(view);
    }
    Ok(result)
}
fn find(root: &AnyObject, id: &str) -> Result<Obj> {
    descendants(root)?
        .into_iter()
        .find(|view| native_id(view) == id)
        .with_context(|| format!("Canonical native control is missing: {id}"))
}
fn window(app: &AnyObject, id: &str) -> Result<Obj> {
    unsafe {
        let windows: *mut AnyObject = msg_send![app, windows];
        let count: usize = msg_send![windows, count];
        ensure!(
            count <= 128,
            "Unexpected number of application-owned windows"
        );
        for index in 0..count {
            let view: *mut AnyObject = msg_send![windows,objectAtIndex:index];
            if !view.is_null() && native_id(&*view) == id {
                return Retained::retain(view).context("Owned window disappeared");
            }
        }
    }
    anyhow::bail!("Canonical window is missing: {id}")
}
fn content(window: &AnyObject) -> Result<Obj> {
    unsafe {
        let view: *mut AnyObject = msg_send![window, contentView];
        Retained::retain(view).context("Canonical window has no content view")
    }
}
fn control(shell: &Shell, id: &str) -> Result<Obj> {
    let window = window(
        &shell.app,
        if id.starts_with("inklow.welcome.") {
            "inklow.welcome"
        } else {
            "inklow.settings"
        },
    )?;
    let root = content(&window)?;
    find(&root, id)
}
fn state(control: &AnyObject) -> isize {
    unsafe { msg_send![control, state] }
}
fn is_enabled(control: &AnyObject) -> bool {
    unsafe { msg_send![control, isEnabled] }
}
fn press(control: &AnyObject) -> Result<()> {
    ensure!(
        is_enabled(control),
        "Native control is disabled: {}",
        native_id(control)
    );
    unsafe {
        let bounds: NSRect = msg_send![control, bounds];
        let _: bool = msg_send![control,scrollRectToVisible:bounds];
        let supported: bool = msg_send![control,respondsToSelector:sel!(accessibilityPerformPress)];
        if supported {
            let accepted: bool = msg_send![control, accessibilityPerformPress];
            ensure!(
                accepted,
                "Owned native accessibility press was refused: {}",
                native_id(control)
            );
        } else {
            let supported: bool = msg_send![control,respondsToSelector:sel!(performClick:)];
            ensure!(supported, "Owned control has no native press action");
            let _: () = msg_send![control,performClick:std::ptr::null::<AnyObject>()];
        }
    }
    Ok(())
}
fn pump(shell: &mut Shell, deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "Owned native verification deadline expired"
    );
    let guard = ModalGuard::arm("", "", false)?;
    let result = objc2::rc::autoreleasepool(|_| -> Result<()> {
        unsafe {
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25))
                .as_secs_f64();
            let date: *mut AnyObject =
                msg_send![AnyClass::get(c"NSDate").unwrap(),dateWithTimeIntervalSinceNow:wait];
            let event: *mut AnyObject = msg_send![&*shell.app,nextEventMatchingMask:usize::MAX,untilDate:date,inMode:&*NSString::from_str("kCFRunLoopDefaultMode"),dequeue:true];
            if !event.is_null() {
                let _: () = msg_send![&*shell.app,sendEvent:event];
            }
            let _: () = msg_send![&*shell.app, updateWindows];
        }
        shell.tick()
    });
    result?;
    guard.finish()
}
fn settle(shell: &mut Shell) -> Result<()> {
    shell.tick()?;
    shell.refresh_state()?;
    unsafe {
        let _: () = msg_send![&*shell.app, updateWindows];
    }
    Ok(())
}
fn action_unchecked(shell: &mut Shell, id: &str) -> Result<()> {
    let control = control(shell, id)?;
    press(&control)?;
    settle(shell)
}
fn action(shell: &mut Shell, id: &str) -> Result<()> {
    let guard = ModalGuard::arm("", "", false)?;
    action_unchecked(shell, id)?;
    guard.finish()
}
fn assert_record(receipt: &mut Value, name: &str, actual: bool, observed: Value) -> Result<()> {
    receipt["assertions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":name,"passed":actual,"observed":observed}));
    ensure!(actual, "Owned native assertion failed: {name}");
    Ok(())
}
fn skip(receipt: &mut Value, name: &str, reason: &str) {
    receipt["not_exercised"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":name,"reason":reason}));
}
fn native_text(view: &AnyObject) -> String {
    unsafe {
        let readable: bool = msg_send![view,respondsToSelector:sel!(stringValue)];
        let editable: bool = msg_send![view,respondsToSelector:sel!(string)];
        if readable || editable {
            string(view)
        } else {
            String::new()
        }
    }
}
fn insert_owned(editor: &AnyObject, text: &str) -> Result<()> {
    let before = string(editor);
    unsafe {
        let value = NSString::from_str(text);
        let _: () = msg_send![editor,insertText:&*value,replacementRange:NSRange::new(0,before.encode_utf16().count())];
        let _: () = msg_send![editor,setSelectedRange:NSRange::new(text.encode_utf16().count(),0)];
    }
    ensure!(
        string(editor) == text,
        "Owned NSTextView insertText readback did not match"
    );
    Ok(())
}

struct ModalState {
    expected: String,
    button: String,
    deadline: Instant,
    outcome: RefCell<Option<std::result::Result<(), String>>>,
}
define_class!(
 #[unsafe(super=NSObject)] #[thread_kind=MainThreadOnly] #[ivars=ModalState]
 struct SmokeModalResponder;
 unsafe impl NSObjectProtocol for SmokeModalResponder{}
 impl SmokeModalResponder{
  #[unsafe(method(drive:))]
  fn drive(&self,timer:&AnyObject){
   if self.ivars().outcome.borrow().is_some(){return}
   let expired=Instant::now()>=self.ivars().deadline;
   unsafe{
    let app:*mut AnyObject=msg_send![AnyClass::get(c"NSApplication").unwrap(),sharedApplication];let modal:*mut AnyObject=msg_send![app,modalWindow];
    if modal.is_null(){if expired{self.ivars().outcome.replace(Some(Err("Expected native consent did not appear before deadline".into())));let _:()=msg_send![timer,invalidate];}return}
    let root:*mut AnyObject=msg_send![modal,contentView];if root.is_null(){return}
    let views=match descendants(&*root){Ok(views)=>views,Err(error)=>{self.ivars().outcome.replace(Some(Err(error.to_string())));let _:()=msg_send![app,abortModal];let _:()=msg_send![timer,invalidate];return}};
    let matches=views.iter().any(|view|native_text(view)==self.ivars().expected);
    if matches&&!expired{
     for view in views{
      let titled:bool=msg_send![&*view,respondsToSelector:sel!(title)];if !titled{continue}let title:*mut NSString=msg_send![&*view,title];if title.is_null()||(&*title).to_string()!=self.ivars().button{continue}
      let clickable:bool=msg_send![&*view,respondsToSelector:sel!(performClick:)];if !clickable{continue}
      self.ivars().outcome.replace(Some(Ok(())));let _:()=msg_send![timer,invalidate];let _:()=msg_send![&*view,performClick:std::ptr::null::<AnyObject>()];return
     }
    }
    if expired||!matches{self.ivars().outcome.replace(Some(Err(if expired{"Native modal action deadline expired"}else{"An unexpected native modal appeared"}.into())));let _:()=msg_send![timer,invalidate];let _:()=msg_send![app,abortModal];}
   }
  }
 }
);
struct ModalGuard {
    target: Retained<SmokeModalResponder>,
    timer: Obj,
    required: bool,
}
impl ModalGuard {
    fn arm(expected: &str, button: &str, required: bool) -> Result<Self> {
        let mtm =
            MainThreadMarker::new().context("Native modal responder requires the main thread")?;
        let target = SmokeModalResponder::alloc(mtm).set_ivars(ModalState {
            expected: expected.into(),
            button: button.into(),
            deadline: Instant::now() + Duration::from_secs(3),
            outcome: RefCell::new(None),
        });
        let target: Retained<SmokeModalResponder> = unsafe { msg_send![super(target), init] };
        let timer = unsafe {
            let timer: *mut AnyObject = msg_send![AnyClass::get(c"NSTimer").unwrap(),timerWithTimeInterval:0.01f64,target:&*target,selector:sel!(drive:),userInfo:std::ptr::null::<AnyObject>(),repeats:true];
            let timer = Retained::retain(timer)
                .context("Could not retain native modal verification timer")?;
            let runloop: *mut AnyObject =
                msg_send![AnyClass::get(c"NSRunLoop").unwrap(), mainRunLoop];
            for mode in ["kCFRunLoopCommonModes", "NSModalPanelRunLoopMode"] {
                let _: () = msg_send![runloop,addTimer:&*timer,forMode:&*NSString::from_str(mode)];
            }
            timer
        };
        Ok(Self {
            target,
            timer,
            required,
        })
    }
    fn finish(&self) -> Result<()> {
        unsafe {
            let _: () = msg_send![&*self.timer, invalidate];
        }
        match self.target.ivars().outcome.borrow().as_ref() {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => Err(anyhow::anyhow!("{error}")),
            None if !self.required => Ok(()),
            None => Err(anyhow::anyhow!(
                "Canonical consent was not actually observed"
            )),
        }
    }
}
impl Drop for ModalGuard {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![&*self.timer, invalidate];
        }
    }
}
fn consent(shell: &mut Shell, id: &str, title: &str, button: &str) -> Result<()> {
    let guard = ModalGuard::arm(title, button, true)?;
    action_unchecked(shell, id)?;
    guard.finish()
}

fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}
fn screenshot(app: &AnyObject, window_id: &str, output: &Path, name: &str) -> Result<Value> {
    let window = window(app, window_id)?;
    let view = content(&window)?;
    unsafe {
        let _: () = msg_send![&*view, layoutSubtreeIfNeeded];
        let _: () = msg_send![&*window, displayIfNeeded];
        let bounds: NSRect = msg_send![&*view, bounds];
        ensure!(
            bounds.size.width >= 300. && bounds.size.height >= 200.,
            "Canonical owned view has invalid dimensions"
        );
        let rep: *mut AnyObject = msg_send![&*view,bitmapImageRepForCachingDisplayInRect:bounds];
        ensure!(!rep.is_null(), "AppKit refused owned view rasterization");
        let _: () = msg_send![&*view,cacheDisplayInRect:bounds,toBitmapImageRep:rep];
        let properties = obj("NSDictionary");
        let data: *mut AnyObject =
            msg_send![rep,representationUsingType:4usize,properties:&*properties];
        ensure!(!data.is_null(), "AppKit refused PNG encoding");
        let length: usize = msg_send![data, length];
        ensure!(
            (33..=50 * 1024 * 1024).contains(&length),
            "PNG encoding has invalid size"
        );
        let bytes: *const u8 = msg_send![data, bytes];
        ensure!(!bytes.is_null(), "PNG encoding has no data");
        let bytes = std::slice::from_raw_parts(bytes, length);
        ensure!(
            &bytes[..8] == b"\x89PNG\r\n\x1a\n" && &bytes[12..16] == b"IHDR",
            "AppKit did not produce a PNG"
        );
        let width = u32::from_be_bytes(bytes[16..20].try_into()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into()?);
        ensure!(width > 0 && height > 0, "Owned PNG is empty");
        let filename = format!("{name}.png");
        write_new(&output.join(&filename), bytes)?;
        Ok(
            json!({"file":filename,"sha256":format!("{:x}",Sha256::digest(bytes)),"width":width,"height":height,"capture":"in-process owned NSView bitmap; not compositor or external-screen capture"}),
        )
    }
}
fn snapshot(shell: &Shell, receipt: &mut Value, output: &Path, id: &str, name: &str) -> Result<()> {
    let evidence = objc2::rc::autoreleasepool(|_| screenshot(&shell.app, id, output, name))?;
    receipt["screenshots"]
        .as_array_mut()
        .unwrap()
        .push(evidence);
    Ok(())
}
fn navigate(shell: &mut Shell, pane: &str) -> Result<()> {
    action(shell, &format!("inklow.sidebar.{pane}"))
}
fn navigate_panes(shell: &mut Shell, receipt: &mut Value, output: &Path) -> Result<()> {
    for (index, (pane, title)) in [
        ("setup", "Setup"),
        ("general", "General"),
        ("models", "AI Models"),
        ("context", "Context"),
        ("personalization", "Personalization"),
        ("emoji", "Emoji"),
        ("shortcuts", "Shortcuts"),
        ("battery", "Battery"),
        ("appSettings", "App Settings"),
        ("labs", "Labs"),
        ("statistics", "Statistics"),
        ("about", "About"),
    ]
    .into_iter()
    .enumerate()
    {
        navigate(shell, pane)?;
        let title_view = control(shell, "inklow.settings.paneTitle")?;
        let selected = control(shell, &format!("inklow.sidebar.{pane}"))?;
        let observed = string(&title_view);
        assert_record(
            receipt,
            &format!("navigation.{pane}"),
            shell.settings.pane() == pane && observed == title && state(&selected) == 1,
            json!({"pane":shell.settings.pane(),"native_title":observed,"selected_state":state(&selected)}),
        )?;
        snapshot(
            shell,
            receipt,
            output,
            "inklow.settings",
            &format!("pane-{:02}-{pane}", index + 1),
        )?;
    }
    Ok(())
}
fn drafts(shell: &mut Shell, receipt: &mut Value, output: &Path) -> Result<()> {
    navigate(shell, "personalization")?;
    let id = "inklow.personalization.globalInstructions";
    let original = shell.config().custom_instructions;
    let editor = control(shell, id)?;
    insert_owned(&editor, "Ephemeral owned draft — amanhã 🦀")?;
    settle(shell)?;
    assert_record(
        receipt,
        "draft.native_edit_unsaved",
        string(&editor) == "Ephemeral owned draft — amanhã 🦀"
            && shell.config().custom_instructions == original,
        json!({"widget":string(&editor),"saved_unchanged":shell.config().custom_instructions==original}),
    )?;
    let before = Retained::as_ptr(&editor);
    let window = window(&shell.app, "inklow.settings")?;
    unsafe {
        let _: () = msg_send![&*window,setContentSize:NSSize::new(980.,700.)];
    }
    shell.settings.reflow();
    let resized = control(shell, id)?;
    let same_editor = Retained::as_ptr(&resized) == before;
    assert_record(
        receipt,
        "draft.resize_retains_editor",
        same_editor && string(&resized) == "Ephemeral owned draft — amanhã 🦀",
        json!({"same_native_editor":same_editor,"widget":string(&resized)}),
    )?;
    navigate(shell, "emoji")?;
    navigate(shell, "personalization")?;
    let restored = control(shell, id)?;
    assert_record(
        receipt,
        "draft.pane_retention",
        string(&restored) == "Ephemeral owned draft — amanhã 🦀",
        json!({"widget":string(&restored)}),
    )?;
    action(shell, "inklow.personalization.global.revertInstructions")?;
    let reverted = string(&*control(shell, id)?);
    assert_record(
        receipt,
        "draft.revert",
        reverted == original && shell.config().custom_instructions == original,
        json!({"native_matches_saved":reverted==original}),
    )?;
    let editor = control(shell, id)?;
    insert_owned(
        &editor,
        "Synthetic preference for the isolated native smoke.",
    )?;
    settle(shell)?;
    action(shell, "inklow.personalization.global.saveInstructions")?;
    let persisted = crate::config::load_config()?;
    let readback = string(&*control(shell, id)?);
    assert_record(
        receipt,
        "draft.save_persisted",
        persisted.custom_instructions == "Synthetic preference for the isolated native smoke."
            && readback == persisted.custom_instructions,
        json!({"native_matches_private_settings":readback==persisted.custom_instructions}),
    )?;
    snapshot(shell, receipt, output, "inklow.settings", "draft-saved")?;
    Ok(())
}
fn privacy(shell: &mut Shell, receipt: &mut Value, output: &Path) -> Result<()> {
    navigate(shell, "context")?;
    for field in [
        "screen_context",
        "clipboard_context",
        "screenshot_context",
        "screenshot_appearance",
    ] {
        let view = control(shell, &format!("inklow.context.{field}"))?;
        assert_record(
            receipt,
            &format!("privacy.initial.{field}"),
            state(&view) == 0,
            json!({"native_state":state(&view)}),
        )?;
    }
    for (field, title) in [
        ("screen_context", "Use focused-window text for context?"),
        ("clipboard_context", "Use the clipboard for context?"),
        ("screenshot_context", "Use screenshots for context?"),
        (
            "screenshot_appearance",
            "Use screenshots to improve suggestion appearance?",
        ),
    ] {
        consent(shell, &format!("inklow.context.{field}"), title, "Cancel")?;
        let config = serde_json::to_value(shell.config())?;
        let native_state = state(&*control(shell, &format!("inklow.context.{field}"))?);
        assert_record(
            receipt,
            &format!("privacy.cancel.{field}"),
            config[field] == false && native_state == 0,
            json!({"saved":config[field],"native_state":native_state}),
        )?;
    }
    consent(
        shell,
        "inklow.context.screen_context",
        "Use focused-window text for context?",
        "Continue",
    )?;
    assert_record(
        receipt,
        "privacy.focused_window_independent",
        shell.config().screen_context
            && !shell.config().clipboard_context
            && !shell.config().record_history,
        json!({"screen_context":shell.config().screen_context,"clipboard_context":shell.config().clipboard_context,"record_history":shell.config().record_history}),
    )?;
    consent(
        shell,
        "inklow.context.clipboard_context",
        "Use the clipboard for context?",
        "Continue",
    )?;
    action(shell, "inklow.context.screen_context")?;
    assert_record(
        receipt,
        "privacy.clipboard_independent",
        !shell.config().screen_context
            && shell.config().clipboard_context
            && !shell.config().screenshot_context
            && !shell.config().screenshot_appearance
            && !shell.config().record_history,
        json!({"config":{ "screen_context":shell.config().screen_context,"clipboard_context":shell.config().clipboard_context,"screenshot_context":shell.config().screenshot_context,"screenshot_appearance":shell.config().screenshot_appearance,"record_history":shell.config().record_history}}),
    )?;
    action(shell, "inklow.context.clipboard_context")?;
    snapshot(shell, receipt, output, "inklow.settings", "privacy-context")?;
    navigate(shell, "personalization")?;
    consent(
        shell,
        "inklow.personalization.record_history",
        "Enable encrypted local learning?",
        "Cancel",
    )?;
    assert_record(
        receipt,
        "privacy.history_consent_cancelled",
        !shell.config().record_history
            && !shell.config().screen_context
            && !shell.config().clipboard_context,
        json!({"record_history":shell.config().record_history,"context_still_off":!shell.config().screen_context&&!shell.config().clipboard_context}),
    )?;
    action(shell, "inklow.personalization.record_unaccepted")?;
    assert_record(
        receipt,
        "privacy.unaccepted_is_not_history_consent",
        shell.config().record_unaccepted && !shell.config().record_history,
        json!({"record_unaccepted":shell.config().record_unaccepted,"record_history":shell.config().record_history}),
    )?;
    action(shell, "inklow.personalization.record_unaccepted")?;
    Ok(())
}
fn choose(shell: &mut Shell, id: &str, index: isize) -> Result<()> {
    let view = control(shell, id)?;
    unsafe {
        let _: () = msg_send![&*view,selectItemAtIndex:index];
        let selector: objc2::runtime::Sel = msg_send![&*view, action];
        let target: *mut AnyObject = msg_send![&*view, target];
        let sent: bool = msg_send![&*shell.app,sendAction:selector,to:target,from:&*view];
        ensure!(sent, "Native popup did not dispatch its action");
    }
    settle(shell)
}
fn welcome(shell: &mut Shell, receipt: &mut Value, output: &Path) -> Result<()> {
    navigate(shell, "setup")?;
    action(shell, "inklow.setup.welcome")?;
    snapshot(
        shell,
        receipt,
        output,
        "inklow.welcome",
        "welcome-introduction",
    )?;
    action(shell, "inklow.welcome.next")?;
    for field in [
        "screen_context",
        "clipboard_context",
        "screenshot_context",
        "screenshot_appearance",
    ] {
        let native_state = state(&*control(shell, &format!("inklow.welcome.{field}"))?);
        assert_record(
            receipt,
            &format!("welcome.independent.{field}"),
            native_state == 0,
            json!({"native_state":native_state}),
        )?;
    }
    action(shell, "inklow.welcome.back")?;
    action(shell, "inklow.welcome.next")?;
    action(shell, "inklow.welcome.next")?;
    let next = control(shell, "inklow.welcome.next")?;
    let permissions = shell.status["permissions"]["accessibility"] == true
        && shell.status["permissions"]["input_monitoring"] == true;
    assert_record(
        receipt,
        "welcome.permission_navigation_gate",
        is_enabled(&next) == permissions,
        json!({"permissions":shell.status["permissions"],"next_enabled":is_enabled(&next)}),
    )?;
    snapshot(
        shell,
        receipt,
        output,
        "inklow.welcome",
        "welcome-permissions",
    )?;
    if !permissions {
        skip(
            receipt,
            "welcome.model_and_preview_navigation",
            "Actual Accessibility or Input Monitoring permission is absent; the canonical Next control correctly remains disabled. No grant or readiness was fabricated",
        );
        return Ok(());
    }
    action(shell, "inklow.welcome.next")?;
    let next = control(shell, "inklow.welcome.next")?;
    snapshot(shell, receipt, output, "inklow.welcome", "welcome-models")?;
    if !is_enabled(&next) {
        assert_record(
            receipt,
            "welcome.selected_model_gate",
            !is_enabled(&next),
            json!({"model_ready":shell.status["model_ready"],"next_enabled":false}),
        )?;
        skip(
            receipt,
            "welcome.preview",
            "No selected local model is ready; BROTYPIST_SMOKE_MODEL may supply an existing GGUF. Nothing was downloaded",
        );
        return Ok(());
    }
    action(shell, "inklow.welcome.next")?;
    let editor = control(shell, "inklow.welcome.preview")?;
    assert_record(
        receipt,
        "welcome.preview_owned_widget",
        native_id(&editor) == "inklow.welcome.preview",
        json!({"native_id":native_id(&editor)}),
    )?;
    for (index, language, prefix) in [
        (0isize, "en", "I was thinking we could"),
        (1isize, "pt", "Amanhã vamos nos encontrar para"),
    ] {
        choose(shell, "inklow.welcome.previewLanguage", index)?;
        let window = window(&shell.app, "inklow.welcome")?;
        let editor = control(shell, "inklow.welcome.preview")?;
        unsafe {
            let accepted: bool = msg_send![&*window,makeFirstResponder:&*editor];
            ensure!(accepted, "Owned preview refused first responder");
        }
        insert_owned(&editor, prefix)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut offered = String::new();
        while Instant::now() < deadline {
            pump(shell, deadline)?;
            unsafe {
                let help: *mut NSString = msg_send![&*editor, accessibilityHelp];
                if !help.is_null() {
                    let text = (&*help).to_string();
                    if let Some(value) = text.strip_prefix("Suggestion: ").filter(|s| !s.is_empty())
                    {
                        offered = value.into();
                        break;
                    }
                }
            }
        }
        ensure!(
            !offered.is_empty(),
            "Actual {language} preview did not expose a visible suggestion before its deadline"
        );
        snapshot(
            shell,
            receipt,
            output,
            "inklow.welcome",
            &format!("preview-{language}-offered"),
        )?;
        unsafe {
            let key: *mut AnyObject = msg_send![AnyClass::get(c"NSEvent").unwrap(),keyEventWithType:10usize,location:NSPoint::new(0.,0.),modifierFlags:0usize,timestamp:0.0f64,windowNumber:{let n:isize=msg_send![&*window,windowNumber];n},context:std::ptr::null::<AnyObject>(),characters:&*NSString::from_str("\t"),charactersIgnoringModifiers:&*NSString::from_str("\t"),isARepeat:false,keyCode:48u16];
            ensure!(!key.is_null(), "AppKit refused an owned Tab event");
            let _: () = msg_send![&*editor,keyDown:key];
        }
        settle(shell)?;
        let actual = string(&editor);
        let inserted = actual.strip_prefix(prefix).unwrap_or("");
        assert_record(
            receipt,
            &format!("welcome.preview_acceptance.{language}"),
            !inserted.is_empty() && offered.starts_with(inserted),
            json!({"before":prefix,"accessible_offer":offered,"actual_widget_readback":actual,"event":"NSEvent sent only to the owned canonical preview NSTextView"}),
        )?;
    }
    action(shell, "inklow.welcome.back")?;
    action(shell, "inklow.welcome.next")?;
    let preview = control(shell, "inklow.welcome.preview")?;
    assert_record(
        receipt,
        "welcome.preview_back_and_return",
        native_id(&preview) == "inklow.welcome.preview",
        json!({"native_preview_recreated":true}),
    )?;
    action(shell, "inklow.welcome.next")?;
    snapshot(shell, receipt, output, "inklow.welcome", "welcome-finish")?;
    action(shell, "inklow.welcome.next")?;
    Ok(())
}
fn quit(shell: &mut Shell) -> Result<()> {
    let guard = ModalGuard::arm("Unsaved custom instructions", "Discard Changes", false)?;
    unsafe {
        let menu = &shell.menus.resident;
        let count: isize = msg_send![&**menu, numberOfItems];
        let mut found = None;
        for index in 0..count {
            let item: *mut AnyObject = msg_send![&**menu,itemAtIndex:index];
            if !item.is_null() && native_id(&*item) == "inklow.menu.quit" {
                found = Some(index);
                break;
            }
        }
        let index = found.context("Canonical Quit menu action is missing")?;
        let _: () = msg_send![&**menu,performActionForItemAtIndex:index];
    }
    shell.tick()?;
    guard.finish()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !shell.complete {
        pump(shell, deadline)?;
        ensure!(
            shell.quitting,
            "Canonical Quit was cancelled: {}",
            shell.shutdown_error
        );
    }
    Ok(())
}
pub(super) fn run(output: &Path) -> Result<i32> {
    ensure!(
        ci(),
        "The canonical macOS shell smoke is CI-only and must run in a disposable native desktop account; a local explicit flag is insufficient"
    );
    let home = std::env::var_os("INKLOW_HOME")
        .map(PathBuf::from)
        .context("Owned native smoke requires an isolated INKLOW_HOME")?;
    ensure!(home.is_absolute(), "INKLOW_HOME must be absolute");
    let metadata = fs::symlink_metadata(&home)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "INKLOW_HOME must be a real private directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
            "INKLOW_HOME must be private and owned by the current user"
        );
    }
    ensure!(
        !home.join("settings.json").exists() && !home.join("local.sqlite3").exists(),
        "Smoke refuses an existing profile; the common entry may create only its instance lock before this point"
    );
    ensure!(
        output.is_absolute(),
        "Smoke output must be an absolute directory"
    );
    crate::paths::private_directory(output)?;
    ensure!(
        !output.join("receipt.json").exists(),
        "Smoke refuses to overwrite an existing receipt"
    );
    let mut receipt = json!({"schema":"brotypist-canonical-macos-ui-smoke/1","passed":false,"scope":"canonical owned native shell/settings/welcome and clean Quit on a disposable CI desktop; not signed external-host or permission proof","private_profile":true,"safety_policy":{"ci_only":true,"automatic_updates":false,"model_downloads_requested":false,"permission_prompts_requested":false,"external_capture_enabled":false},"foreground_activation_allowed":true,"assertions":[],"not_exercised":[],"screenshots":[]});
    let model = match super::super::smoke_model_path() {
        Ok(model) => model,
        Err(error) => {
            receipt["prerequisite_error"] = json!(format!("{error:#}"));
            write_new(
                &output.join("receipt.json"),
                &serde_json::to_vec_pretty(&receipt)?,
            )?;
            return Err(error);
        }
    };
    let config = Config {
        global_enabled: false,
        enabled: false,
        setup_completed: true,
        check_updates_automatically: false,
        record_history: false,
        record_unaccepted: false,
        statistics_enabled: false,
        screen_context: false,
        clipboard_context: false,
        screenshot_context: false,
        screenshot_appearance: false,
        typo_suppression: false,
        typo_fixes: false,
        model_path: model.to_string_lossy().into_owned(),
        model_id: String::new(),
        ..Config::default()
    };
    let options = LaunchOptions {
        background: true,
        no_tray: true,
        pane: Some("general".into()),
        login_startup: false,
    };
    let startup_guard = ModalGuard::arm("", "", false)?;
    let mut shell = match objc2::rc::autoreleasepool(|_| Shell::new(config, &options)) {
        Ok(shell) => shell,
        Err(error) => {
            receipt["startup_error"] = json!(format!("{error:#}"));
            write_new(
                &output.join("receipt.json"),
                &serde_json::to_vec_pretty(&receipt)?,
            )?;
            return Err(error);
        }
    };
    let startup_result = startup_guard.finish();
    shell.controller.borrow().suspend_shortcuts(true)?;
    let result = startup_result.and_then(|_| {
        navigate_panes(&mut shell, &mut receipt, output)?;
        drafts(&mut shell, &mut receipt, output)?;
        privacy(&mut shell, &mut receipt, output)?;
        welcome(&mut shell, &mut receipt, output)?;
        Ok(())
    });
    if let Err(error) = &result {
        receipt["error"] = json!(format!("{error:#}"));
    }
    let cleanup = quit(&mut shell);
    let cleaned = cleanup.is_ok()
        && shell.complete
        && shell.controller.borrow().status()["shutdown_complete"] == true
        && shell.clipboard.is_none()
        && shell.sampler.is_none();
    let cleanup_assert = assert_record(
        &mut receipt,
        "quit.canonical_cleanup",
        cleaned,
        json!({"shell_complete":shell.complete,"controller_shutdown_complete":shell.controller.borrow().status()["shutdown_complete"],"clipboard_owner_released":shell.clipboard.is_none(),"power_sampler_released":shell.sampler.is_none()}),
    );
    if let Err(error) = &cleanup {
        receipt["cleanup_error"] = json!(format!("{error:#}"));
    }
    unsafe {
        let _: () = msg_send![&*shell.app,setDelegate:std::ptr::null::<AnyObject>()];
    }
    receipt["passed"] = json!(result.is_ok() && cleanup.is_ok() && cleanup_assert.is_ok());
    write_new(
        &output.join("receipt.json"),
        &serde_json::to_vec_pretty(&receipt)?,
    )?;
    result?;
    cleanup?;
    cleanup_assert?;
    Ok(0)
}
