//! Feature-gated verification of the real resident shell and its owned AppKit views.
//! No external Accessibility query, global input injection, permission request, or download.
use super::*;
use objc2_app_kit::{NSAccessibilityPressAction, NSAccessibilityUnignoredDescendant};
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
fn press(kit: &Kit, control: &AnyObject) -> Result<()> {
    ensure!(
        is_enabled(control),
        "Native control is disabled: {}",
        native_id(control)
    );
    unsafe {
        let bounds: NSRect = msg_send![control, bounds];
        let _: bool = msg_send![control,scrollRectToVisible:bounds];
        // A view is not necessarily the element exposed to assistive clients:
        // single-cell AppKit controls may expose their cell instead. In addition,
        // NSObject responds to accessibilityPerformPress even when that method
        // does not implement this control's AXPress action. Resolve the exposed
        // element and use AppKit's advertised action dispatcher, not selector
        // presence or a performClick fallback.
        let element = NSAccessibilityUnignoredDescendant(control).with_context(|| {
            format!(
                "Native control has no unique exposed accessibility element: {}",
                native_id(control)
            )
        })?;
        let actions: *mut AnyObject = msg_send![&*element, accessibilityActionNames];
        ensure!(
            !actions.is_null(),
            "Native accessibility element advertises no actions: {}",
            native_id(control)
        );
        let supported: bool = msg_send![actions,containsObject:NSAccessibilityPressAction];
        ensure!(
            supported,
            "Native accessibility element does not advertise AXPress: {}",
            native_id(control)
        );
        let queued = kit.queue.borrow().pending.len();
        let _: () = msg_send![&*element,accessibilityPerformAction:NSAccessibilityPressAction];
        // The public legacy dispatcher returns void. Success therefore requires
        // the real native target/action callback, not an invented acknowledgment.
        // Canonical actions must enqueue once; shell state is asserted after tick.
        ensure!(
            kit.queue.borrow().pending.len() == queued + 1,
            "Owned native AXPress did not dispatch exactly one captured intent: {}",
            native_id(control)
        );
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
    press(&shell.kit, &control)?;
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

fn preview_word_expectation(
    before: &crate::core::Snapshot,
    generated: &str,
    visible: &str,
    config: &Config,
) -> Option<(String, crate::core::Snapshot)> {
    let mut suggestion = crate::core::SuggestionState {
        snapshot: Some(before.clone()),
        text: generated.into(),
        ..Default::default()
    };
    // Model wording is not an oracle. Derive the exact native insertion from the
    // configured consumer contract, including its terminal-word trailing space.
    let insertion = suggestion.accept(
        before,
        false,
        config.trailing_space,
        crate::features::include_punctuation(&config.punctuation_mode, false),
        Some(visible),
    )?;
    Some((insertion, suggestion.snapshot?))
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
fn screenshot(
    app: &AnyObject,
    window_id: &str,
    output: &Path,
    name: &str,
    theme_frame: bool,
) -> Result<Value> {
    let window = window(app, window_id)?;
    let content = content(&window)?;
    let view = if theme_frame {
        let parent: *mut AnyObject = unsafe { msg_send![&*content, superview] };
        unsafe { Retained::retain(parent).context("Owned window has no native theme frame")? }
    } else {
        content
    };
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
        let bytes: *const std::ffi::c_void = msg_send![data, bytes];
        ensure!(!bytes.is_null(), "PNG encoding has no data");
        let bytes = std::slice::from_raw_parts(bytes.cast::<u8>(), length);
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
fn layout_rect(rect: NSRect) -> Value {
    json!({
        "x":rect.origin.x,"y":rect.origin.y,
        "width":rect.size.width,"height":rect.size.height
    })
}
fn layout_geometry(
    settings: &Settings,
    app: &AnyObject,
    pane: &str,
    size_case: &str,
    phase: &str,
) -> Result<Value> {
    let owned = window(app, "inklow.settings")?;
    let root = content(&owned)?;
    let sidebar = settings.sidebar_layout_report()?;
    ensure!(
        sidebar["single_styled_highlight"] == true,
        "Sidebar must have one styled selection without native button fills"
    );
    for row in sidebar["rows"]
        .as_array()
        .context("Missing native sidebar rows")?
    {
        let number = |key: &str| -> Result<f64> {
            row[key]
                .as_f64()
                .with_context(|| format!("Missing sidebar geometry: {key}"))
        };
        ensure!(
            row["tile_rect"]["width"].as_f64().unwrap_or(0.) >= 23.
                && row["tile_rect"]["height"].as_f64().unwrap_or(0.) >= 23.
                && number("tile_title_separation")? >= 7.,
            "Sidebar icon is undersized or crowded against its title: {}",
            row["key"]
        );
        if row["selected"] == true {
            ensure!(
                number("selection_leading_padding")? >= 7.
                    && number("selection_top_padding")? >= 5.
                    && number("selection_bottom_padding")? >= 5.
                    && row["selection_matches_row"] == true
                    && row["fully_visible_in_viewport"] == true,
                "Selected sidebar item lost its inset padding or visibility"
            );
        }
    }
    unsafe {
        let _: () = msg_send![&*root, layoutSubtreeIfNeeded];
        let _: () = msg_send![app, updateWindows];
        let active: bool = msg_send![app, isActive];
        ensure!(
            !active,
            "Layout capture unexpectedly activated the application"
        );
        let frame: NSRect = msg_send![&*owned, frame];
        let bounds: NSRect = msg_send![&*root, bounds];
        let visible: NSRect = msg_send![&*root, visibleRect];
        Ok(json!({
            "pane":pane,"size_case":size_case,"phase":phase,
            "window_frame":layout_rect(frame),
            "content_bounds":layout_rect(bounds),
            "content_visible_rect":layout_rect(visible),
            "alignment":settings.alignment_report()?,
            "sidebar":sidebar
        }))
    }
}
fn layout_draft_resize(
    settings: &mut Settings,
    app: &AnyObject,
    output: &Path,
    captures: &mut Vec<Value>,
    geometry: &mut Vec<Value>,
    receipt: &mut Value,
) -> Result<()> {
    let owned = window(app, "inklow.settings")?;
    let root = content(&owned)?;
    let id = "inklow.personalization.globalInstructions";
    let editor = find(&root, id)?;
    let draft = "Layout draft — amanhã 🦀\n日本語 • مرحبًا • café";
    let selection = NSRange::new(
        "Layout draft — ".encode_utf16().count(),
        "amanhã 🦀".encode_utf16().count(),
    );
    insert_owned(&editor, draft)?;
    unsafe {
        let _: () = msg_send![&*editor,setSelectedRange:selection];
    }
    *receipt = json!({
        "editor_id":id,"draft":draft,
        "selection_utf16":{"location":selection.location,"length":selection.length},
        "mutation":"Owned NSTextView insertText:replacementRange:; no save action or configuration reconstruction",
        "observations":[]
    });
    // Keep the actual NSTextView alive across every reflow. In particular, never
    // call prepare here: rebuilding from remembered text would hide lost state.
    for (size_case, size) in [
        ("before", None),
        ("minimum", Some(NSSize::new(760., 560.))),
        ("baseline", Some(NSSize::new(900., 640.))),
        ("expanded", Some(NSSize::new(1000., 740.))),
    ] {
        if let Some(size) = size {
            unsafe {
                let _: () = msg_send![&*owned,setContentSize:size];
            }
            settings.reflow();
        }
        geometry.push(layout_geometry(
            settings,
            app,
            "personalization",
            size_case,
            "retained-draft",
        )?);
        let resized = find(&root, id)?;
        let same_editor = Retained::as_ptr(&resized) == Retained::as_ptr(&editor);
        unsafe {
            let scroll: *mut AnyObject = msg_send![&*resized, enclosingScrollView];
            ensure!(
                !scroll.is_null(),
                "Instruction editor has no native scroll view"
            );
            let scroll_bounds: NSRect = msg_send![scroll, bounds];
            let _: bool = msg_send![scroll,scrollRectToVisible:scroll_bounds];
            let _: () = msg_send![&*root, layoutSubtreeIfNeeded];
            let actual_selection: NSRange = msg_send![&*resized, selectedRange];
            let actual_text = string(&resized);
            let bounds: NSRect = msg_send![&*resized, bounds];
            let visible: NSRect = msg_send![&*resized, visibleRect];
            let scroll_frame: NSRect = msg_send![scroll, frame];
            let scroll_visible: NSRect = msg_send![scroll, visibleRect];
            let retained_selection = actual_selection.location == selection.location
                && actual_selection.length == selection.length;
            let readable = visible.size.width >= 300. && visible.size.height >= 80.;
            receipt["observations"].as_array_mut().unwrap().push(json!({
                "size_case":size_case,"same_native_editor":same_editor,
                "native_editor_identity":format!("{:p}",Retained::as_ptr(&resized)),
                "readback":actual_text,
                "selection_utf16":{"location":actual_selection.location,"length":actual_selection.length},
                "bounds":layout_rect(bounds),"visible_rect":layout_rect(visible),
                "scroll_frame":layout_rect(scroll_frame),"scroll_visible_rect":layout_rect(scroll_visible),
                "passed":same_editor && actual_text == draft && retained_selection && readable
            }));
            ensure!(
                same_editor && actual_text == draft && retained_selection && readable,
                "Native instruction editor lost identity, Unicode draft, selection or readable geometry during {size_case} reflow"
            );
        }
        captures.push(screenshot(
            app,
            "inklow.settings",
            output,
            &format!("personalization-draft-{size_case}"),
            true,
        )?);
    }
    Ok(())
}
/// Bounded rendering only: no Shell, Controller, profile, input hooks, services or activation.
pub(super) fn capture_layout(output: &Path) -> Result<i32> {
    ensure!(
        output.is_absolute() && !output.exists(),
        "Layout output must be a new absolute directory"
    );
    crate::paths::private_directory(output)?;
    let mtm = MainThreadMarker::new().context("Layout capture requires the main AppKit thread")?;
    let app: Obj = unsafe {
        let raw: *mut AnyObject =
            msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
        Retained::retain(raw).context("AppKit is unavailable")?
    };
    unsafe {
        let _: bool = msg_send![&*app,setActivationPolicy:1isize];
    }
    install_source_icon(&app);
    let kit = Kit::new(mtm, true);
    let config = Config {
        global_enabled: false,
        enabled: false,
        check_updates_automatically: false,
        record_history: false,
        record_unaccepted: false,
        statistics_enabled: false,
        screen_context: false,
        clipboard_context: false,
        screenshot_context: false,
        screenshot_appearance: false,
        ..Config::default()
    };
    let status = json!({
        "state":"paused", "model_name":"Gemma 4 E2B", "current_app":"",
        "message":"This field does not expose verified same-line rendering; ordinary completion is silent.",
        "paused_remaining":0, "permissions":{}
    });
    let menus = Menus::new(&kit);
    menus.refresh(&kit, &config, &status, &json!({}), None);
    let mut menu_items = Vec::new();
    unsafe {
        let count: isize = msg_send![&*menus.resident, numberOfItems];
        ensure!(count == 8, "Resident menu regained dashboard rows");
        for index in 0..count {
            let item: *mut AnyObject = msg_send![&*menus.resident,itemAtIndex:index];
            let custom: *mut AnyObject = msg_send![item, view];
            ensure!(
                custom.is_null(),
                "Resident menu contains a custom dashboard view"
            );
            let title: *mut NSString = msg_send![item, title];
            let title = (&*title).to_string();
            ensure!(
                !title.to_lowercase().contains("battery") && !title.contains('%'),
                "Battery presentation returned to the resident menu"
            );
            menu_items.push(json!({"id":native_id(&*item),"title":title}));
        }
        let hidden: bool = msg_send![&*menus.resume, isHidden];
        ensure!(
            hidden && !is_enabled(&menus.pause_hour),
            "Inactive pause controls changed"
        );
    }
    let mut power = PowerUi::new()?;
    let card = power.make_card("pane");
    let card = unsafe {
        Retained::retain(&*card as *const objc2_app_kit::NSView as *mut AnyObject)
            .context("Battery pane card disappeared")?
    };
    let mut settings = Settings::new(kit);
    settings.set_power_card(card);
    let mut captures = Vec::new();
    let mut geometry = Vec::new();
    let mut draft_retention = Value::Null;
    let result = (|| -> Result<()> {
        // Keep all twelve baseline panes, then exercise the same representative
        // forms at both size extremes. Geometry comes from alignment_report's
        // current text/control contract rather than copied alignment thresholds.
        for (appearance, panes, width, height, suffix) in [
            (
                "NSAppearanceNameDarkAqua",
                &[
                    "general",
                    "setup",
                    "models",
                    "context",
                    "personalization",
                    "emoji",
                    "shortcuts",
                    "battery",
                    "appSettings",
                    "labs",
                    "statistics",
                    "about",
                ][..],
                900.,
                640.,
                "baseline",
            ),
            (
                "NSAppearanceNameDarkAqua",
                &[
                    "general",
                    "setup",
                    "models",
                    "context",
                    "personalization",
                    "about",
                ][..],
                760.,
                560.,
                "minimum",
            ),
            (
                "NSAppearanceNameAqua",
                &[
                    "general",
                    "setup",
                    "models",
                    "context",
                    "personalization",
                    "about",
                ][..],
                1000.,
                740.,
                "resized",
            ),
        ] {
            unsafe {
                let appearance: *mut AnyObject = msg_send![AnyClass::get(c"NSAppearance").unwrap(),appearanceNamed:&*NSString::from_str(appearance)];
                ensure!(!appearance.is_null(), "Native appearance is unavailable");
                let _: () = msg_send![&*app,setAppearance:appearance];
            }
            for pane in panes {
                settings.prepare(pane, &config, &status, &json!([]), &json!({}))?;
                geometry.push(layout_geometry(&settings, &app, pane, suffix, "prepared")?);
                let owned = window(&app, "inklow.settings")?;
                unsafe {
                    let _: () = msg_send![&*owned,setContentSize:NSSize::new(width,height)];
                    let _: () = msg_send![&*owned,orderBack:std::ptr::null::<AnyObject>()];
                }
                settings.reflow();
                geometry.push(layout_geometry(&settings, &app, pane, suffix, "resized")?);
                captures.push(screenshot(
                    &app,
                    "inklow.settings",
                    output,
                    &format!("{pane}-{suffix}"),
                    true,
                )?);
                if matches!(
                    *pane,
                    "general" | "setup" | "models" | "context" | "personalization"
                ) {
                    // Verify the document's final content as well as its final
                    // identified control, including trailing explanatory text.
                    let root = content(&owned)?;
                    let prefix = format!("inklow.{pane}.");
                    let last = descendants(&root)?
                        .into_iter()
                        .filter(|view| native_id(view).starts_with(&prefix))
                        .last()
                        .with_context(|| format!("{pane} has no native setting controls"))?;
                    unsafe {
                        let bounds: NSRect = msg_send![&*last, bounds];
                        let scroll: *mut AnyObject = msg_send![&*last, enclosingScrollView];
                        ensure!(
                            !scroll.is_null(),
                            "{pane} is not inside its native scroll view"
                        );
                        let document: *mut AnyObject = msg_send![scroll, documentView];
                        ensure!(!document.is_null(), "{pane} has no scroll document");
                        let document_bounds: NSRect = msg_send![document, bounds];
                        let bottom = rect(
                            0.,
                            document_bounds.size.height - 1.,
                            document_bounds.size.width,
                            1.,
                        );
                        let _: bool = msg_send![document,scrollRectToVisible:bottom];
                        let document_visible: NSRect = msg_send![document, visibleRect];
                        ensure!(
                            document_visible.origin.y + document_visible.size.height
                                >= document_bounds.origin.y + document_bounds.size.height - 1.,
                            "The final {pane} content is not reachable by scrolling"
                        );
                        let visible: NSRect = msg_send![&*last, visibleRect];
                        ensure!(
                            visible.size.width >= bounds.size.width - 1.
                                && visible.size.height >= bounds.size.height - 1.,
                            "The last {pane} control is not fully reachable by scrolling"
                        );
                    }
                    geometry.push(layout_geometry(
                        &settings,
                        &app,
                        pane,
                        suffix,
                        "scrolled-bottom",
                    )?);
                    captures.push(screenshot(
                        &app,
                        "inklow.settings",
                        output,
                        &format!("{pane}-{suffix}-bottom"),
                        true,
                    )?);
                }
                if *pane == "personalization" && suffix == "resized" {
                    layout_draft_resize(
                        &mut settings,
                        &app,
                        output,
                        &mut captures,
                        &mut geometry,
                        &mut draft_retention,
                    )?;
                }
            }
        }
        Ok(())
    })();
    settings.close();
    power.close()?;
    let receipt = json!({
        "schema":"brotypist-macos-layout-capture/3", "passed":result.is_ok(),
        "scope":"Canonical owned native Settings controls and theme frame; synthetic preferences/status, no runtime services. Native menu inspected without opening it.",
        "safety":{"controller_started":false,"profile_loaded":false,"input_hooks_started":false,"foreground_activation_allowed":false,"external_screen_capture":false},
        "limits":"Native geometry assertions and NSView rasterization do not prove WindowServer vibrancy/compositing or external-host typing. Visual review of images is required.",
        "menu":menu_items,"screenshots":captures,"geometry":geometry,
        "draft_retention":draft_retention,
        "error":result.as_ref().err().map(|error|format!("{error:#}"))
    });
    write_new(
        &output.join("receipt.json"),
        &serde_json::to_vec_pretty(&receipt)?,
    )?;
    result?;
    Ok(0)
}
fn snapshot(shell: &Shell, receipt: &mut Value, output: &Path, id: &str, name: &str) -> Result<()> {
    let evidence = objc2::rc::autoreleasepool(|_| screenshot(&shell.app, id, output, name, false))?;
    receipt["screenshots"]
        .as_array_mut()
        .unwrap()
        .push(evidence);
    Ok(())
}
fn navigate(shell: &mut Shell, pane: &str) -> Result<()> {
    action(shell, &format!("inklow.sidebar.{pane}"))
}
fn sidebar_press_contract(shell: &mut Shell, receipt: &mut Value) -> Result<()> {
    let id = "inklow.sidebar.setup";
    let setup = control(shell, id)?;
    let element = unsafe { NSAccessibilityUnignoredDescendant(&setup) }
        .context("Canonical Setup has no unique exposed accessibility element")?;
    receipt["accessibility_press"] = json!({
        "action": "AXPress",
        "dispatch": "accessibilityPerformAction: on NSAccessibilityUnignoredDescendant",
        "setup_view_is_exposed_element": std::ptr::eq(&*setup, &*element),
        "success_requires": "exactly one real native target/action intent and scenario state readback",
        "perform_click_fallback": false
    });
    ensure!(
        is_enabled(&setup),
        "Canonical Setup control must be enabled"
    );
    let queued = shell.kit.queue.borrow().pending.len();
    unsafe {
        let _: () = msg_send![&*setup,setEnabled:false];
    }
    let disabled = press(&shell.kit, &setup);
    // Restore the real control before recording a failing assertion as well.
    unsafe {
        let _: () = msg_send![&*setup,setEnabled:true];
    }
    let unchanged = shell.kit.queue.borrow().pending.len() == queued;
    assert_record(
        receipt,
        "accessibility.disabled_press_rejected",
        disabled.is_err() && unchanged,
        json!({"refused":disabled.is_err(),"captured_intents_unchanged":unchanged}),
    )?;
    for name in [
        "accessibility.setup_press",
        "accessibility.setup_repeat_press",
    ] {
        // Exercise the canonical control through its exposed AXPress action.
        // Repeat selection must dispatch again and leave the sidebar selected,
        // not acknowledge an inherited no-op or toggle the current pane off.
        action(shell, id)?;
        let setup = control(shell, id)?;
        let title = string(&*control(shell, "inklow.settings.paneTitle")?);
        assert_record(
            receipt,
            name,
            shell.settings.pane() == "setup" && title == "Setup" && state(&setup) == 1,
            json!({"pane":shell.settings.pane(),"native_title":title,"selected_state":state(&setup),"dispatch":"advertised AXPress on unignored native element; exactly one captured intent"}),
        )?;
    }
    Ok(())
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
        let config = shell.config();
        let mut offered = None;
        while Instant::now() < deadline {
            pump(shell, deadline)?;
            if let Some((before, generated, visible)) = shell.welcome.preview_offer() {
                if let Some((insertion, expected)) =
                    preview_word_expectation(&before, &generated, &visible, &config)
                {
                    offered = Some((before, generated, visible, insertion, expected));
                    break;
                }
            }
        }
        let (before, generated, visible, insertion, expected) = offered.with_context(|| {
            format!("Actual {language} preview did not expose a drawn, word-acceptable suggestion before its deadline")
        })?;
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
        let actual = macos_welcome::read_editor(&editor)?;
        assert_record(
            receipt,
            &format!("welcome.preview_acceptance.{language}"),
            before.text.as_ref() == prefix
                && before.caret == prefix.chars().count()
                && expected.same_key(&actual),
            json!({"before":prefix,"generated_offer":generated,"rendered_offer":visible,"expected_word_insertion":insertion,"expected_widget_readback":expected.text.as_ref(),"expected_caret":expected.caret,"actual_widget_readback":actual.text.as_ref(),"actual_caret":actual.caret,"actual_selection_length":actual.selection_length,"event":"NSEvent sent only to the owned canonical preview NSTextView"}),
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
        sidebar_press_contract(&mut shell, &mut receipt)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Snapshot;

    #[test]
    fn preview_word_expectation_includes_configured_terminal_space() {
        let before = Snapshot {
            text: "I was thinking we could".into(),
            caret: 23,
            ..Default::default()
        };
        for (trailing_space, insertion) in [(true, " make "), (false, " make")] {
            let config = Config {
                trailing_space,
                ..Config::default()
            };
            let (actual, expected) =
                preview_word_expectation(&before, " make", " make", &config).unwrap();
            assert_eq!(actual, insertion);
            assert_eq!(
                expected.text.as_ref(),
                format!("{}{insertion}", before.text.as_ref())
            );
            assert_eq!(expected.caret, before.caret + insertion.chars().count());
            assert_eq!(expected.selection_length, 0);
        }
    }

    #[test]
    fn preview_word_expectation_preserves_punctuation_and_clipping_policy() {
        let before = Snapshot {
            text: "Hello".into(),
            caret: 5,
            ..Default::default()
        };
        let config = Config::default();
        for (generated, visible, insertion) in [
            (" world tomorrow", " world", Some(" world ")),
            (" world.", " world.", Some(" world")),
            (" world tomorrow", " wo", None),
            (" world", "", None),
        ] {
            assert_eq!(
                preview_word_expectation(&before, generated, visible, &config)
                    .map(|(text, _)| text),
                insertion.map(str::to_owned)
            );
        }
    }

    #[test]
    fn preview_word_expectation_checks_unicode_caret_and_existing_suffix() {
        let before = Snapshot {
            text: "Olá 😀 fim".into(),
            caret: 6,
            ..Default::default()
        };
        let (insertion, expected) =
            preview_word_expectation(&before, "ação ", "ação ", &Config::default()).unwrap();
        assert_eq!(insertion, "ação ");
        assert_eq!(expected.text.as_ref(), "Olá 😀 ação fim");
        assert_eq!(expected.caret, 11);
        let mut wrong_caret = expected.clone();
        wrong_caret.caret -= 1;
        assert!(!expected.same_key(&wrong_caret));
        let mut wrong_selection = expected.clone();
        wrong_selection.selection_length = 1;
        assert!(!expected.same_key(&wrong_selection));
    }
}
