//! Feature-gated verification of the real resident shell and its owned AppKit views.
//! Full-shell acceptance remains CI-only; layout-only remains rendering-only.
//! Separately authorized owned acceptance uses WorkerClient with either controlled
//! loopback output or an explicitly supplied local GGUF, never a controller bypass.
//! No external Accessibility query, global input injection, permission request, or download.
//! Layout evidence includes real catalogue selections with unspecified and positive
//! memory thresholds; neither render fixture asserts installation or hardware fit.
use super::*;
use objc2_app_kit::{NSAccessibilityPressAction, NSAccessibilityUnignoredDescendant};
use objc2_foundation::NSRange;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
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
        let mut hidden = None;
        for index in 0..count {
            let view: *mut AnyObject = msg_send![windows,objectAtIndex:index];
            if !view.is_null() && native_id(&*view) == id {
                let retained = Retained::retain(view).context("Owned window disappeared")?;
                let visible: bool = msg_send![view, isVisible];
                if visible {
                    return Ok(retained);
                }
                hidden = Some(retained);
            }
        }
        if let Some(window) = hidden {
            return Ok(window);
        }
    }
    anyhow::bail!("Canonical window is missing: {id}")
}
pub(super) fn content(window: &AnyObject) -> Result<Obj> {
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
        let mut ready = false;
        unsafe {
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25))
                .as_secs_f64();
            let date: *mut AnyObject =
                msg_send![AnyClass::get(c"NSDate").unwrap(),dateWithTimeIntervalSinceNow:wait];
            let event: *mut AnyObject = msg_send![&*shell.app,nextEventMatchingMask:usize::MAX,untilDate:date,inMode:&*NSString::from_str("kCFRunLoopDefaultMode"),dequeue:true];
            if !event.is_null() {
                ready = shell.wakeup.consume(&*event);
                if !ready {
                    let _: () = msg_send![&*shell.app,sendEvent:event];
                }
            }
            let _: () = msg_send![&*shell.app, updateWindows];
        }
        if ready { shell.ready() } else { shell.tick() }
    });
    result?;
    guard.finish()
}
fn native_wake_cycles(shell: &mut Shell, receipt: &mut Value) -> Result<()> {
    let mut deliveries = [0; 2];
    for delivery in &mut deliveries {
        let before = shell.wakeup.delivery_count();
        shell.wakeup.callback()();
        let deadline = Instant::now() + Duration::from_secs(2);
        while shell.wakeup.delivery_count() == before {
            pump(shell, deadline)?;
        }
        *delivery = shell.wakeup.delivery_count() - before;
    }
    assert_record(
        receipt,
        "native_wake.rearms_after_native_event_consumption",
        deliveries == [1, 1],
        json!({"successive_delivery_cycles": deliveries}),
    )
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

pub(super) fn write_new(path: &Path, data: &[u8]) -> Result<()> {
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
            row["tile_rect"]["width"].as_f64().unwrap_or(0.) > 0.
                && row["tile_rect"]["height"].as_f64().unwrap_or(0.) > 0.
                && number("tile_title_separation")? > 0.,
            "Sidebar icon is missing or overlaps its title: {}",
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
fn capture_menu_glyphs(output: &Path) -> Result<Vec<Value>> {
    let mut glyphs = Vec::new();
    for state in [
        "active",
        "paused",
        "off",
        "attention",
        "loading-1",
        "loading-2",
        "loading-3",
    ] {
        let image = super::capture_status_image(state);
        unsafe {
            let template: bool = msg_send![&*image, isTemplate];
            let size: NSSize = msg_send![&*image, size];
            ensure!(
                template && size == NSSize::new(18., 16.),
                "Invalid menu template"
            );
            let reps: *mut AnyObject = msg_send![&*image, representations];
            let count: usize = msg_send![reps, count];
            ensure!(
                count == 2,
                "Menu artwork must have 1x and 2x representations"
            );
            let mut representations = Vec::new();
            for index in 0..count {
                let rep: *mut AnyObject = msg_send![reps,objectAtIndex:index];
                let width: isize = msg_send![rep, pixelsWide];
                let height: isize = msg_send![rep, pixelsHigh];
                let scale = index + 1;
                ensure!(
                    width == 18 * scale as isize && height == 16 * scale as isize,
                    "Invalid menu representation dimensions"
                );
                let properties = obj("NSDictionary");
                let data: *mut AnyObject =
                    msg_send![rep,representationUsingType:4usize,properties:&*properties];
                ensure!(!data.is_null(), "AppKit refused menu PNG encoding");
                let length: usize = msg_send![data, length];
                ensure!(
                    (33..=1024 * 1024).contains(&length),
                    "Invalid menu PNG size"
                );
                let bytes: *const std::ffi::c_void = msg_send![data, bytes];
                ensure!(!bytes.is_null(), "Menu PNG has no data");
                let bytes = std::slice::from_raw_parts(bytes.cast::<u8>(), length);
                ensure!(
                    &bytes[..8] == b"\x89PNG\r\n\x1a\n" && &bytes[12..16] == b"IHDR",
                    "Menu encoding is not PNG"
                );
                let png_width = u32::from_be_bytes(bytes[16..20].try_into()?);
                let png_height = u32::from_be_bytes(bytes[20..24].try_into()?);
                ensure!(
                    png_width == width as u32 && png_height == height as u32,
                    "Menu PNG dimensions differ from native representation"
                );
                let file = format!("menubar-{state}@{scale}x.png");
                write_new(&output.join(&file), bytes)?;
                representations.push(json!({
                    "file":file, "scale":scale,
                    "pixel_width":width, "pixel_height":height,
                    "sha256":format!("{:x}", Sha256::digest(bytes))
                }));
            }
            glyphs.push(json!({
                "state":state, "isTemplate":template,
                "logical_size":{"width":size.width,"height":size.height},
                "source_svg":format!("crates/brotypist/assets/menubar/menubar-{state}.svg"),
                "representations":representations,
                "capture":"Actual in-process NSImage bitmap representations; no screen capture or activation"
            }));
        }
    }
    Ok(glyphs)
}

fn capture_choice_surfaces(app: &AnyObject, output: &Path) -> Result<Vec<Value>> {
    use crate::{core::Snapshot, platform::Action};
    let mut overlay = OverlayOwner::new()?;
    overlay.configure(false, "Tab");
    let config = Config::default();
    let key_before: *mut AnyObject = unsafe { msg_send![app, keyWindow] };
    let mut observations = Vec::new();
    let mut cases = Vec::new();
    for (case, text, wanted) in [
        ("alias", "Synthetic :sorriso", Some(0usize)),
        ("emoticon", "Synthetic <3", Some(0)),
        ("last-row", "Synthetic :", None),
    ] {
        let (_, rows) = crate::features::emoji_choices(text, &config)
            .context("Synthetic emoji trigger produced no choices")?;
        let wanted = wanted.unwrap_or_else(|| rows.len().saturating_sub(1));
        ensure!(rows.len() > wanted, "Synthetic emoji row unavailable");
        cases.push((case, text, rows.clone(), rows, wanted));
    }
    cases.push((
        "compact-middle",
        "Synthetic choice",
        vec!["tomorrow".into(), "W".repeat(200), "later".into()],
        vec!["tomorrow".into(), "later".into()],
        1,
    ));
    cases.push((
        "compact-exact-whitespace",
        "Synthetic choice",
        vec![
            "  amanhã  cedo ".into(),
            "W".repeat(200),
            " later 😀".into(),
        ],
        vec!["  amanhã  cedo ".into(), " later 😀".into()],
        1,
    ));
    for (case, text, rows, expected_rows, wanted) in cases {
        let snapshot = Snapshot {
            text: text.into(),
            caret: text.chars().count(),
            app_id: "owned.synthetic.emoji".into(),
            field_id: "owned-editor".into(),
            ..Snapshot::default()
        };
        let acknowledged = overlay.handle(&json!({
            "type":"suggestion","mode":"emoji","text":rows[0],"choices":rows,
            "rect":[120.,240.,1.,18.],"field_rect":[100.,200.,700.,100.],
            "key":snapshot.key(),"authority":snapshot.authority(),"revision":1
        }))?;
        ensure!(
            acknowledged.iter().any(|command| matches!(
                command,
                Command::PresentationResult { mode, visible_text, .. }
                    if mode == "emoji" && visible_text == &expected_rows.join("\n")
            )),
            "Native chooser did not acknowledge the exact compact rows"
        );
        let targets = overlay
            .choice_targets()
            .context("Visible chooser has no target geometry")?;
        ensure!(
            targets.button_count == expected_rows.len(),
            "Native hit targets do not match the compact rows"
        );
        let chooser = window(app, "inklow.chooser")?;
        let view = content(&chooser)?;
        for (index, expected) in expected_rows.iter().enumerate() {
            let row_button = find(&view, &format!("inklow.suggestion.choice.{}", index + 1))?;
            let title: *mut NSString = unsafe { msg_send![&*row_button, title] };
            let tag: isize = unsafe { msg_send![&*row_button, tag] };
            ensure!(
                !title.is_null()
                    && unsafe { (&*title).to_string() } == format!("{}   {}", index + 1, expected)
                    && tag == (index + 1) as isize
                    && is_enabled(&row_button),
                "Native chooser changed the raw row or displayed ordinal"
            );
        }
        ensure!(
            find(
                &view,
                &format!("inklow.suggestion.choice.{}", expected_rows.len() + 1)
            )
            .is_err(),
            "Native chooser retained an ineligible row"
        );
        let button = find(&view, &format!("inklow.suggestion.choice.{}", wanted + 1))?;
        let filename = format!("emoji-{case}.png");
        unsafe {
            let _: () = msg_send![&*view, layoutSubtreeIfNeeded];
            let bounds: NSRect = msg_send![&*view, bounds];
            let rep: *mut AnyObject =
                msg_send![&*view,bitmapImageRepForCachingDisplayInRect:bounds];
            ensure!(!rep.is_null(), "Emoji view could not be rasterized");
            let _: () = msg_send![&*view,cacheDisplayInRect:bounds,toBitmapImageRep:rep];
            let properties = obj("NSDictionary");
            let data: *mut AnyObject =
                msg_send![rep,representationUsingType:4usize,properties:&*properties];
            ensure!(!data.is_null(), "Emoji PNG could not be encoded");
            let length: usize = msg_send![data, length];
            let bytes: *const std::ffi::c_void = msg_send![data, bytes];
            ensure!(
                !bytes.is_null() && (33..=1024 * 1024).contains(&length),
                "Emoji PNG has invalid bytes"
            );
            write_new(
                &output.join(&filename),
                std::slice::from_raw_parts(bytes.cast::<u8>(), length),
            )?;
            let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
        }
        let actions = overlay.tick()?;
        ensure!(
            matches!(actions.as_slice(), [Command::Action(Action::Choice(index))] if *index == wanted),
            "Native button delivered the wrong displayed choice ordinal"
        );
        let key_after: *mut AnyObject = unsafe { msg_send![app, keyWindow] };
        ensure!(key_before == key_after, "Chooser stole the key window");
        overlay.handle(&json!({"type":"clear","hide_accessory":true}))?;
        ensure!(
            overlay.choice_targets().is_none(),
            "Hidden chooser retained hit targets"
        );
        unsafe {
            let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
        }
        ensure!(
            overlay.tick()?.is_empty(),
            "Hidden choice remained actionable"
        );
        observations.push(json!({
            "case":case,"file":filename,"choice_index":wanted,
            "acknowledged_rows":expected_rows,
            "key_window_preserved":true,"hidden_choice_inactive":true,"passed":true,
            "scope":"native chooser pixels, exact titles and acknowledgment, hit regions, NSButton target/action and hide invalidation only; no runtime acceptance, insertion or host readback"
        }));
    }
    overlay.close()?;
    Ok(observations)
}

/// A real local HTTP provider endpoint, not an injected Worker implementation.
/// Only synthetic responses are queued; every unexpected route/request fails closed.
struct CompletionFixture {
    endpoint: String,
    samples: std::sync::Arc<std::sync::Mutex<VecDeque<String>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    hold_terminal: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: RefCell<Option<std::thread::JoinHandle<Result<usize>>>>,
}
impl CompletionFixture {
    fn start() -> Result<Self> {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let samples = Arc::new(Mutex::new(VecDeque::<String>::new()));
        let responses = samples.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let hold_terminal = Arc::new(AtomicBool::new(false));
        let held = hold_terminal.clone();
        let thread = std::thread::Builder::new().name("owned-completion-fixture".into()).spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(120);
            let mut completions = 0;
            let mut requests = 0;
            while !stopped.load(Ordering::Acquire) {
                ensure!(Instant::now() < deadline, "Completion fixture exceeded its lifetime");
                let (mut socket, peer) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                requests += 1;
                ensure!(peer.ip().is_loopback() && requests <= 128, "Invalid fixture client/request count");
                // Accepted sockets may inherit the listener's nonblocking mode.
                // This bounded HTTP parser requires blocking reads with deadlines.
                socket.set_nonblocking(false)?;
                socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                socket.set_write_timeout(Some(Duration::from_secs(2)))?;
                let (request, body) = Self::request(&mut socket)?;
                let response = if request == "GET /v1/models HTTP/1.1" {
                    ensure!(body.is_empty(), "Unexpected fixture prepare body");
                    json!({"data":[{"id":"owned-choice-fixture"}]}).to_string()
                } else {
                    ensure!(request == "POST /v1/completions HTTP/1.1", "Unexpected fixture endpoint");
                    let body: Value = serde_json::from_slice(&body)?;
                    ensure!(
                        body["model"] == "owned-choice-fixture"
                            && body["stream"] == true
                            && body["prompt"].is_string(),
                        "Invalid real provider fixture request"
                    );
                    let sample = responses.lock().map_err(|_| anyhow::anyhow!("Fixture queue poisoned"))?
                        .pop_front().context("Unscripted provider generation")?;
                    completions += 1;
                    format!("data: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"text":sample,"finish_reason":if held.load(Ordering::Acquire) { Value::Null } else { json!("stop") }}]}))
                };
                if held.load(Ordering::Acquire) && request.starts_with("POST ") {
                    let (partial, terminal) = response.split_once("data: [DONE]")
                        .context("Held fixture has no terminal marker")?;
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
                        response.len(), partial)?;
                    socket.flush()?;
                    let until = Instant::now() + Duration::from_secs(15);
                    while held.load(Ordering::Acquire) {
                        ensure!(!stopped.load(Ordering::Acquire) && Instant::now() < until,
                            "Owned terminal publication barrier expired");
                        std::thread::yield_now();
                    }
                    write!(socket, "data: [DONE]{terminal}")?;
                } else {
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
                        response.len(), response)?;
                }
            }
            Ok(completions)
        })?;
        Ok(Self {
            endpoint,
            samples,
            stop,
            hold_terminal,
            thread: RefCell::new(Some(thread)),
        })
    }
    fn request(socket: &mut std::net::TcpStream) -> Result<(String, Vec<u8>)> {
        use std::io::Read;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut header = Vec::new();
        // Byte-at-a-time parsing is deliberately confined to this bounded fixture.
        while !header.ends_with(b"\r\n\r\n") {
            ensure!(
                header.len() < 8192 && Instant::now() < deadline,
                "Fixture HTTP header exceeded bounds"
            );
            let mut byte = [0];
            socket.read_exact(&mut byte)?;
            header.push(byte[0]);
        }
        let header = std::str::from_utf8(&header)?;
        let mut lines = header.split("\r\n");
        let request = lines.next().context("Missing fixture request")?.to_owned();
        let mut length = None;
        for line in lines.filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').context("Malformed fixture header")?;
            ensure!(
                !name.eq_ignore_ascii_case("transfer-encoding"),
                "Fixture refuses chunked requests"
            );
            if name.eq_ignore_ascii_case("content-length") {
                ensure!(length.is_none(), "Duplicate fixture content length");
                length = Some(value.trim().parse::<usize>()?);
            }
        }
        let length = length.unwrap_or(0);
        ensure!(length <= 128 * 1024, "Fixture request body exceeded bounds");
        let mut body = vec![0; length];
        let mut read = 0;
        while read < body.len() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("Fixture request exceeded deadline")?;
            socket.set_read_timeout(Some(remaining.min(Duration::from_secs(2))))?;
            let count = socket.read(&mut body[read..])?;
            ensure!(count != 0, "Fixture request body ended early");
            read += count;
        }
        ensure!(
            Instant::now() < deadline,
            "Fixture request exceeded deadline"
        );
        Ok((request, body))
    }
    fn arm(&self, samples: Vec<String>) -> Result<()> {
        self.check()?;
        ensure!(
            !samples.is_empty() && samples.len() <= 5,
            "Invalid fixture sample count"
        );
        let mut queue = self
            .samples
            .lock()
            .map_err(|_| anyhow::anyhow!("Fixture queue poisoned"))?;
        ensure!(
            queue.is_empty(),
            "Previous fixture samples were not consumed"
        );
        queue.extend(samples);
        Ok(())
    }
    fn check(&self) -> Result<()> {
        let mut thread = self.thread.borrow_mut();
        if thread.as_ref().is_some_and(|thread| thread.is_finished()) {
            let result = thread
                .take()
                .context("Owned completion fixture already stopped")?
                .join()
                .map_err(|_| anyhow::anyhow!("Owned completion fixture panicked"))?;
            return match result {
                Ok(_) => anyhow::bail!("Owned completion fixture ended before shutdown"),
                Err(error) => Err(error.context("Owned completion fixture failed")),
            };
        }
        ensure!(thread.is_some(), "Owned completion fixture already stopped");
        Ok(())
    }
    fn finish(&mut self) -> Result<usize> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let count = self
            .thread
            .get_mut()
            .take()
            .context("Fixture already stopped")?
            .join()
            .map_err(|_| anyhow::anyhow!("Completion fixture panicked"))??;
        ensure!(
            self.samples
                .lock()
                .map_err(|_| anyhow::anyhow!("Fixture queue poisoned"))?
                .is_empty(),
            "Fixture samples remained unused"
        );
        Ok(count)
    }
}
impl Drop for CompletionFixture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.get_mut().take() {
            let _ = thread.join();
        }
    }
}

/// Optional I/O is refused even if a future configuration change asks for it.
struct ChoiceReaders;
impl crate::runtime::Readers for ChoiceReaders {
    fn power(&mut self) -> crate::system::PowerStatus {
        crate::system::PowerStatus::default()
    }
    fn persist(&mut self, _: &Config) -> Result<()> {
        anyhow::bail!("Owned choice fixture refuses preference persistence")
    }
    fn context_ticket(
        &mut self,
        _: &crate::core::Snapshot,
        _: &mut dyn crate::platform::NativeBridge,
    ) -> Result<Option<Box<dyn crate::platform::DetachedContextRead>>> {
        anyhow::bail!("Owned choice fixture refuses external context")
    }
}
impl crate::runtime::BackgroundReaders for ChoiceReaders {
    fn clipboard(
        &self,
        _: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
        _: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<String> {
        anyhow::bail!("Owned choice fixture refuses clipboard access")
    }
    fn screen(
        &self,
        _: &crate::core::Snapshot,
        _: &dyn crate::platform::DetachedContextRead,
        _: &dyn Fn() -> bool,
        _: usize,
        _: bool,
        _: bool,
    ) -> Result<crate::platform::ContextRead> {
        anyhow::bail!("Owned choice fixture refuses screen access")
    }
}

// Transparent observation of WorkerClient-delivered events (poll may already
// filter stale wire events). No synthesis, replay, filtering or acknowledgment.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize)]
struct ChoiceWorkerCounts {
    results: u64,
    finals: u64,
    done: u64,
    events: Vec<Value>,
}
struct ChoiceWorker {
    client: crate::process_client::WorkerClient,
    counts: Rc<RefCell<BTreeMap<u64, ChoiceWorkerCounts>>>,
    submissions: Rc<Cell<u64>>,
}
impl crate::runtime::Worker for ChoiceWorker {
    fn prepare(&mut self, c: &Config) -> Result<u64> {
        self.client.prepare(c)
    }
    fn submit(
        &mut self,
        c: &Config,
        prefix: &str,
        suffix: &str,
        options: &crate::providers::GenerationOptions,
    ) -> Result<u64> {
        self.submissions.set(self.submissions.get() + 1);
        self.client.submit(c, prefix, suffix, options)
    }
    fn proceed(&mut self, id: u64) -> Result<()> {
        self.client.proceed(id)
    }
    fn poll(&mut self) -> Vec<crate::process_client::WorkerEvent> {
        let events = self.client.poll();
        for event in &events {
            if event.kind == "result" || event.kind == "done" {
                let mut counts = self.counts.borrow_mut();
                let counts = counts.entry(event.id).or_default();
                counts.results += u64::from(event.kind == "result");
                counts.finals += u64::from(event.kind == "result" && event.final_);
                counts.done += u64::from(event.kind == "done");
                counts.events.push(json!({
                    "kind":event.kind,"text":event.text,"final":event.final_
                }));
            }
        }
        events
    }
    fn cancel(&mut self) -> Result<()> {
        self.client.cancel()
    }
    fn release(&mut self) -> Result<crate::process_client::ReleaseTicket> {
        self.client.release()
    }
    fn release_complete(&self, ticket: crate::process_client::ReleaseTicket) -> bool {
        self.client.release_complete(ticket)
    }
    fn prepare_for_validation(
        &mut self,
        c: &Config,
        ticket: crate::process_client::ReleaseTicket,
    ) -> Result<std::num::NonZeroU64> {
        self.client.prepare_for_validation(c, ticket)
    }
    fn set_wakeup(&mut self, callback: Option<std::sync::Arc<dyn Fn() + Send + Sync>>) {
        self.client.set_wakeup(callback);
    }
    fn take_terminal_report(&mut self) -> Option<crate::diagnostics::TerminalReport> {
        self.client.take_terminal_report()
    }
    fn lost_terminal_reports(&self) -> u64 {
        self.client.lost_terminal_reports()
    }
    fn set_idle_timeout(&mut self, seconds: u32) {
        self.client.set_idle_timeout(seconds);
    }
    fn status(&self) -> Value {
        self.client.status()
    }
    fn request_stop(&mut self) -> Result<()> {
        self.client.request_stop()
    }
    fn is_stopped(&mut self) -> bool {
        self.client.is_stopped()
    }
}

/// The bridge can address only these two retained NSTextViews, never AX or an
/// arbitrary process. Native insertion lives here, behind Engine::accept.
struct ChoiceHost {
    window: Obj,
    editors: [Obj; 2],
    revision: u64,
    edits: usize,
    captures: std::cell::Cell<u64>,
    enabled: bool,
    started: bool,
    shortcut: Option<crate::platform::ShortcutHandler>,
    changed: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    model_clock: Option<Instant>,
    model_input_at: Option<Instant>,
    model_notifications: Vec<Value>,
}
impl ChoiceHost {
    fn focused(&self) -> Option<usize> {
        unsafe {
            let visible: bool = msg_send![&*self.window, isVisible];
            let key: bool = msg_send![&*self.window, isKeyWindow];
            let app: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let active: bool = msg_send![app, isActive];
            let responder: *mut AnyObject = msg_send![&*self.window, firstResponder];
            let attached = |editor: &Obj| {
                let owner: *mut AnyObject = msg_send![&**editor, window];
                std::ptr::eq(&*self.window, owner)
            };
            (visible && key && active)
                .then(|| {
                    self.editors
                        .iter()
                        .position(|editor| std::ptr::eq(&**editor, responder) && attached(editor))
                })
                .flatten()
        }
    }
    fn seed(&mut self, field: usize, text: &str, selection: NSRange) -> Result<()> {
        ensure!(
            !self.started || self.focused().is_some(),
            OwnedInterruption::EditorFocus
        );
        // Fixture setup, not acceptance: the only acceptance mutation is in
        // ChoiceBridge::replace_range below.
        set_string(&self.editors[field], text);
        unsafe {
            let _: () = msg_send![&*self.editors[field],setSelectedRange:selection];
            if !self.started {
                let _: () =
                    msg_send![&*self.window,makeKeyAndOrderFront:std::ptr::null::<AnyObject>()];
            }
            let focused: bool = msg_send![&*self.window,makeFirstResponder:&*self.editors[field]];
            ensure!(focused, OwnedInterruption::EditorFocus);
            let _: () = msg_send![&*self.window, displayIfNeeded];
        }
        self.revision += 1;
        let callback_started = Instant::now();
        if self.model_clock.is_some() {
            self.model_input_at = Some(callback_started);
        }
        if let Some(changed) = &self.changed {
            changed();
        }
        if let Some(origin) = self.model_clock {
            self.model_notifications.push(json!({
                "revision":self.revision,"callback_installed":self.changed.is_some(),
                "callback_start_us":callback_started.duration_since(origin).as_micros(),
                "callback_elapsed_us":callback_started.elapsed().as_micros(),
                "scope":"synchronous owned NativeBridge input callback after synthetic NSTextView seed; excludes text mutation, physical input, capture and worker execution"
            }));
        }
        ensure!(
            self.focused() == Some(field),
            "Owned field did not gain focus"
        );
        self.started = true;
        Ok(())
    }
    fn capture(&self) -> Result<Option<crate::core::Snapshot>> {
        use crate::core::{InlineRender, Snapshot, TextIndex};
        self.captures.set(self.captures.get() + 1);
        use objc2_app_kit::{NSFont, NSScreen};
        let Some(field) = self.focused() else {
            return Ok(None);
        };
        let editor = &self.editors[field];
        let text = string(editor);
        unsafe {
            let selection: NSRange = msg_send![&**editor, selectedRange];
            let (caret, selection_length) = TextIndex::new(&text)
                .utf16_range_to_scalar(selection.location, selection.length)
                .context("Owned widget returned a non-scalar selection")?;
            let caret_native: NSRect = msg_send![&**editor,
                firstRectForCharacterRange:NSRange::new(selection.location,0),
                actualRange:std::ptr::null_mut::<NSRange>()];
            let bounds: NSRect = msg_send![&**editor, visibleRect];
            let in_window: NSRect =
                msg_send![&**editor,convertRect:bounds,toView:std::ptr::null::<AnyObject>()];
            let field_native: NSRect = msg_send![&*self.window,convertRectToScreen:in_window];
            let mtm = MainThreadMarker::new().context("Owned bridge requires AppKit")?;
            let screens = NSScreen::screens(mtm);
            let screen = screens
                .firstObject()
                .context("Owned bridge requires a screen")?;
            let top = screen.frame().origin.y + screen.frame().size.height;
            let screen_rect = |r: NSRect| {
                [
                    r.origin.x,
                    top - r.origin.y - r.size.height,
                    r.size.width,
                    r.size.height,
                ]
            };
            let mut caret_rect = screen_rect(caret_native);
            caret_rect[2] = caret_rect[2].max(1.);
            let field_rect = screen_rect(field_native);
            ensure!(
                caret_rect[3] > 0. && field_rect[2] > 0.,
                "Owned widget has no geometry"
            );
            let font: *mut NSFont = msg_send![&**editor, font];
            ensure!(!font.is_null(), "Owned widget has no font");
            let font = &*font;
            Ok(Some(Snapshot {
                app_id: "owned.native.choice-smoke".into(),
                field_id: format!("owned-choice-field-{field}"),
                text: text.into(),
                caret,
                selection_length,
                caret_rect: Some(caret_rect),
                field_rect: Some(field_rect),
                window_id: "owned-choice-host".into(),
                window_revision: "1".into(),
                window_pid: std::process::id() as i32,
                inline_render: Some(InlineRender {
                    font_name: font.fontName().to_string(),
                    font_size: font.pointSize(),
                    baseline: caret_rect[1] + font.ascender(),
                    clip_rect: field_rect,
                    ascent: font.ascender(),
                    descent: -font.descender(),
                    system_font: true,
                    source: "owned-nstextview".into(),
                    ..InlineRender::default()
                }),
                ..Snapshot::default()
            }))
        }
    }
}
impl Drop for ChoiceHost {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![&*self.window,setDelegate:std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.window, close];
        }
    }
}
struct ChoiceBridge(Rc<RefCell<ChoiceHost>>, Option<String>);
impl crate::platform::NativeBridge for ChoiceBridge {
    fn configure(&mut self, c: &Config) -> Result<()> {
        ensure!(
            !c.record_history
                && !c.record_unaccepted
                && !c.use_history
                && c.personalization_strength == 0
                && !c.statistics_enabled
                && !c.screen_context
                && !c.clipboard_context
                && !c.screenshot_context
                && !c.screenshot_appearance
                && !c.launch_at_login
                && !c.check_updates_automatically
                && match &self.1 {
                    Some(path) =>
                        c.backend == "gguf"
                            && c.model_path == *path
                            && c.model_id.is_empty()
                            && c.endpoint.is_empty(),
                    None => c.backend == "openai" && c.model == "owned-choice-fixture",
                }
                && c.app_rules.is_empty(),
            "Owned bridge refuses external services"
        );
        Ok(())
    }
    fn capture(&mut self) -> Result<Option<crate::core::Snapshot>> {
        self.0.borrow().capture()
    }
    fn foreground(&mut self) -> Result<Option<crate::platform::Foreground>> {
        Ok(self
            .0
            .borrow()
            .focused()
            .map(|_| crate::platform::Foreground {
                app_id: "owned.native.choice-smoke".into(),
                window_id: "owned-choice-host".into(),
                window_pid: std::process::id() as i32,
                ..crate::platform::Foreground::default()
            }))
    }
    fn insert(
        &mut self,
        snapshot: &crate::core::Snapshot,
        text: &str,
    ) -> Result<crate::platform::EditOutcome> {
        self.replace_range(snapshot, snapshot.caret, 0, text)
    }
    fn replace_range(
        &mut self,
        snapshot: &crate::core::Snapshot,
        start: usize,
        length: usize,
        text: &str,
    ) -> Result<crate::platform::EditOutcome> {
        use crate::{core::ScalarIndex, platform::EditOutcome};
        let mut host = self.0.borrow_mut();
        let Some(current) = host.capture()? else {
            return Ok(EditOutcome::Rejected);
        };
        if !host.enabled
            || !current.same_key(snapshot)
            || !current.same_authority(snapshot)
            || !current.can_replace
        {
            return Ok(EditOutcome::Rejected);
        }
        let index = current.text_index();
        let begin = index
            .scalar_to_utf16(ScalarIndex(start))
            .context("Invalid edit start")?
            .0;
        let end = index
            .scalar_to_utf16(ScalarIndex(
                start.checked_add(length).context("Invalid edit length")?,
            ))
            .context("Invalid edit end")?
            .0;
        let field = host.focused().context("Owned focus changed before edit")?;
        let value = NSString::from_str(text);
        unsafe {
            let _: () = msg_send![&*host.editors[field],insertText:&*value,replacementRange:NSRange::new(begin,end-begin)];
        }
        host.edits += 1;
        Ok(match host.capture()? {
            Some(readback) => EditOutcome::Applied(readback),
            None => EditOutcome::Indeterminate,
        })
    }
    fn range_rect(
        &mut self,
        snapshot: &crate::core::Snapshot,
        _: usize,
        _: usize,
    ) -> Result<Option<crate::core::Rect>> {
        Ok(self
            .0
            .borrow()
            .capture()?
            .filter(|current| current.same_key(snapshot) && current.same_authority(snapshot))
            .and_then(|current| current.caret_rect))
    }
    fn focused_context(
        &mut self,
        _: &crate::core::Snapshot,
        _: &dyn Fn() -> bool,
        _: usize,
    ) -> Result<crate::platform::ContextRead> {
        anyhow::bail!("Owned choice smoke forbids context collection")
    }
    fn start_shortcuts(&mut self, callback: crate::platform::ShortcutHandler) -> Result<()> {
        // Retain the real runtime callback, but install no OS/global hook.
        self.0.borrow_mut().shortcut = Some(callback);
        Ok(())
    }
    fn set_shortcuts_enabled(&mut self, enabled: bool) {
        self.0.borrow_mut().enabled = enabled;
    }
    fn set_input_callback(&mut self, callback: Option<std::sync::Arc<dyn Fn() + Send + Sync>>) {
        self.0.borrow_mut().changed = callback;
    }
    fn finish_input(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
    fn input_claim_valid(&self) -> bool {
        let host = self.0.borrow();
        host.enabled && host.focused().is_some()
    }
    fn input_revision(&self) -> u64 {
        self.0.borrow().revision
    }
    fn user_input_revision(&self) -> u64 {
        self.0.borrow().revision
    }
    fn input_timestamp(&self) -> Option<Instant> {
        self.0.borrow().model_input_at
    }
    fn secure_input_enabled(&self) -> Option<bool> {
        // These retained NSTextViews are created here as ordinary, non-secure
        // fields; this says nothing about global secure-input/AX permissions.
        self.0.borrow().focused().map(|_| false)
    }
    fn request_permission(&mut self, _: &str) -> Result<crate::platform::Permission> {
        anyhow::bail!("Owned choice smoke must not request permissions")
    }
    fn status(&self) -> Value {
        json!({"owned_choice_smoke":true})
    }
    fn stop(&mut self) -> Result<()> {
        let mut host = self.0.borrow_mut();
        host.enabled = false;
        host.shortcut = None;
        host.changed = None;
        Ok(())
    }
}

fn capture_choice_bitmap(app: &AnyObject, output: &Path, name: &str) -> Result<Value> {
    let chooser = window(app, "inklow.chooser")?;
    let view = content(&chooser)?;
    let mut row_pixels = Vec::new();
    for button in descendants(&view)?
        .into_iter()
        .filter(|button| native_id(button).starts_with("inklow.suggestion.choice."))
    {
        unsafe {
            let bounds: NSRect = msg_send![&*button, bounds];
            let bitmap: *mut AnyObject =
                msg_send![&*button,bitmapImageRepForCachingDisplayInRect:bounds];
            ensure!(!bitmap.is_null(), "Owned row bitmap unavailable");
            let _: () = msg_send![&*button,cacheDisplayInRect:bounds,toBitmapImageRep:bitmap];
            let width: isize = msg_send![bitmap, pixelsWide];
            let height: isize = msg_send![bitmap, pixelsHigh];
            ensure!(
                (1..=4096).contains(&width) && (1..=256).contains(&height),
                "Owned row bitmap dimensions invalid"
            );
            let corner: *mut AnyObject = msg_send![bitmap,colorAtX:0isize,y:0isize];
            ensure!(
                !corner.is_null(),
                "Owned row bitmap has no background pixel"
            );
            let mut painted = 0usize;
            for y in 0..height {
                for x in 0..width {
                    let color: *mut AnyObject = msg_send![bitmap,colorAtX:x,y:y];
                    ensure!(!color.is_null(), "Owned row bitmap has no pixel");
                    let same: bool = msg_send![color,isEqual:corner];
                    painted += usize::from(!same);
                }
            }
            ensure!(
                painted >= 20,
                "Borderless native choice row contains no rasterized glyphs"
            );
            row_pixels.push(json!({"id":native_id(&button),"nonuniform_pixels":painted,
                "width":width,"height":height}));
        }
    }
    ensure!(
        !row_pixels.is_empty(),
        "Owned chooser has no painted native rows"
    );
    unsafe {
        let bounds: NSRect = msg_send![&*view, bounds];
        let bitmap: *mut AnyObject = msg_send![&*view,bitmapImageRepForCachingDisplayInRect:bounds];
        ensure!(!bitmap.is_null(), "Owned chooser bitmap unavailable");
        let _: () = msg_send![&*view,cacheDisplayInRect:bounds,toBitmapImageRep:bitmap];
        let properties = obj("NSDictionary");
        let data: *mut AnyObject =
            msg_send![bitmap,representationUsingType:4usize,properties:&*properties];
        ensure!(!data.is_null(), "Owned chooser PNG unavailable");
        let length: usize = msg_send![data, length];
        let bytes: *const std::ffi::c_void = msg_send![data, bytes];
        ensure!(
            !bytes.is_null() && (33..=1024 * 1024).contains(&length),
            "Owned chooser PNG invalid"
        );
        let bytes = std::slice::from_raw_parts(bytes.cast::<u8>(), length);
        let filename = format!("{name}.png");
        write_new(&output.join(&filename), bytes)?;
        Ok(
            json!({"file":filename,"sha256":format!("{:x}",Sha256::digest(bytes)),
            "row_pixels":row_pixels,
            "capture":"actual owned chooser NSView bitmap, not compositor capture"}),
        )
    }
}

fn choice_commands(controller: &crate::runtime::Controller, commands: Vec<Command>) -> Result<()> {
    for command in commands {
        controller.send(command)?;
    }
    Ok(())
}

fn choice_pump(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
) -> Result<Vec<Value>> {
    choice_pump_receipts(controller, overlay, None)
}

fn choice_pump_receipts(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    mut receipts: Option<&mut Vec<Command>>,
) -> Result<Vec<Value>> {
    choice_owned_events()?;
    let mut observed = Vec::new();
    // Bounded queue drain, no run-loop sleeps or external event synthesis.
    for _ in 0..4 {
        let events = controller.poll();
        if events.is_empty() {
            break;
        }
        for event in &events {
            let commands = overlay.handle(event)?;
            for command in &commands {
                if let Command::PresentationResult {
                    key,
                    authority,
                    revision,
                    visible_text,
                    mode,
                    ..
                } = command
                {
                    // Preserve the actual typed native receipt before sending it.
                    // Rehandling this event cannot recreate a consumed publication.
                    if let Some(receipts) = receipts.as_deref_mut() {
                        receipts.push(command.clone());
                    }
                    observed.push(json!({"type":"owned_presentation_ack","revision":revision,
                        "key":key,"authority":authority,"visible_text":visible_text,"mode":mode}));
                }
            }
            choice_commands(controller, commands)?;
        }
        observed.extend(events);
    }
    Ok(observed)
}

fn choice_readback(
    host: &ChoiceHost,
    field: usize,
    expected: &str,
    caret: usize,
    edits: usize,
) -> Result<Value> {
    // Independent widget read, not the Snapshot returned by NativeBridge.
    let text = string(&host.editors[field]);
    let selection: NSRange = unsafe { msg_send![&*host.editors[field], selectedRange] };
    ensure!(
        text == expected && selection == NSRange::new(caret, 0) && host.edits == edits,
        "Independent owned widget text/caret/edit-count mismatch"
    );
    Ok(
        json!({"text":text,"caret_utf16":selection.location,"selection_utf16":selection.length,
        "native_edits":host.edits,"independent_widget_readback":true}),
    )
}

/// Terminal ownership failures must survive anyhow context and case-row retention.
/// Inference/no-draw errors remain recoverable; a dequeued physical event does not.
#[derive(Clone, Copy, Debug)]
enum OwnedInterruption {
    PhysicalInput,
    ApplicationFocus,
    EditorFocus,
}

impl std::fmt::Display for OwnedInterruption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PhysicalInput => "Physical input interrupted owned fixture",
            Self::ApplicationFocus => "Owned application lost focus; fixture aborted",
            Self::EditorFocus => "Owned editor lost focus; fixture aborted",
        })
    }
}

impl std::error::Error for OwnedInterruption {}

/// Called after retaining the current row, before any recovery or later case.
fn owned_matrix_continue<T>(result: &Result<T>) -> Result<()> {
    if let Some(cause) = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<OwnedInterruption>())
    {
        return Err((*cause).into());
    }
    Ok(())
}

fn choice_owned_events() -> Result<()> {
    ensure!(
        choice_app_events(None)?,
        OwnedInterruption::ApplicationFocus
    );
    Ok(())
}

/// Service only this process's lifecycle events. Physical input interrupts the
/// fixture instead of being consumed as test input or sent to another owner.
pub(super) fn choice_app_events(wake: Option<&AppKitWake>) -> Result<bool> {
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
            if wake.is_some_and(|wake| wake.consume(&*event)) {
                continue;
            }
            let kind: usize = msg_send![event, type];
            ensure!(matches!(kind, 13..=17), OwnedInterruption::PhysicalInput);
            let _: () = msg_send![app,sendEvent:event];
        }
        let _: () = msg_send![app, updateWindows];
        Ok(msg_send![app, isActive])
    }
}

/// Two real provider requests must finish through private NSEvent delivery and
/// Controller::poll_ready, without a routine poll rescuing a missed notification.
fn capture_owned_wake(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    host: &Rc<RefCell<ChoiceHost>>,
    counts: &Rc<RefCell<BTreeMap<u64, ChoiceWorkerCounts>>>,
) -> Result<Value> {
    let mtm = MainThreadMarker::new().context("Owned wake proof requires the main thread")?;
    let wake = AppKitWake::new(mtm)?;
    controller.set_wakeup(Some(wake.callback()));
    let result = (|| -> Result<Value> {
        let mut cycles = Vec::new();
        for text in ["tomorrow", "later"] {
            host.borrow_mut().seed(0, "Draft ", NSRange::new(6, 0))?;
            ensure!(
                choice_app_events(Some(&wake))?,
                "External focus interrupted owned wake proof"
            );
            // Establish the new owned field before starting the request. After
            // submission, only consumed private events may drain publications.
            for event in controller.poll() {
                choice_commands(controller, overlay.handle(&event)?)?;
            }
            let before = wake.delivery_count();
            fixture.arm(vec![text.into()])?;
            controller.send(Command::Action(crate::platform::Action::Force))?;
            for event in controller.flush_commands() {
                choice_commands(controller, overlay.handle(&event)?)?;
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut events = Vec::new();
            let mut ready_turns = 0usize;
            loop {
                fixture.check()?;
                let delivered = wake.delivery_count();
                ensure!(
                    choice_app_events(Some(&wake))?,
                    "External focus interrupted owned wake proof"
                );
                if wake.delivery_count() != delivered {
                    ready_turns += 1;
                    let ready = controller.poll_ready();
                    for event in &ready {
                        choice_commands(controller, overlay.handle(event)?)?;
                    }
                    events.extend(ready);
                    choice_commands(controller, overlay.tick()?)?;
                    for event in controller.flush_commands() {
                        choice_commands(controller, overlay.handle(&event)?)?;
                    }
                }
                // Count actual WorkerClient-delivered final/done events tied to
                // the exact ready-path offer, not transport counters alone.
                let diagnostics = controller.diagnostics();
                let terminal = events.iter().find_map(|offer| {
                    if offer["type"] != "suggestion"
                        || offer["mode"] != "completion"
                        || offer["text"] != text
                    {
                        return None;
                    }
                    diagnostics.traces.iter().find(|trace| {
                        Some(trace.trace_id) == offer["diagnostic"]["trace_id"].as_u64()
                            && trace.terminal.as_ref().is_some_and(|terminal| {
                                terminal.reason == crate::diagnostics::TerminalReason::Completed
                                    && Some(terminal.request_id) == trace.request_id
                                    && counts
                                        .borrow()
                                        .get(&terminal.request_id)
                                        .is_some_and(|count| count.finals == 1 && count.done == 1)
                            })
                    })
                });
                if let Some(trace) = terminal {
                    ensure!(
                        wake.delivery_count() > before && ready_turns > 0,
                        "Owned result bypassed native private-event delivery"
                    );
                    cycles.push(json!({
                        "expected_text":text,"private_event_deliveries":wake.delivery_count() - before,
                        "ready_turns":ready_turns,"ready_events":events,"terminal_trace":trace,
                        "routine_polls_after_submission":0
                    }));
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "Private wake did not rearm and consume the owned completion"
                );
                std::thread::yield_now();
            }
            controller.send(Command::Action(crate::platform::Action::Dismiss))?;
            for event in controller.flush_commands() {
                choice_commands(controller, overlay.handle(&event)?)?;
            }
        }
        Ok(json!({"cycles":cycles,"passed":true,
            "scope":"actual reusable AppKitWake event -> Controller::poll_ready -> real WorkerClient final/done; no routine polling fallback"}))
    })();
    controller.set_wakeup(None);
    // Retained cross-thread callbacks can outlive main-thread source/context
    // invalidation; no callback ever owns or dereferences the freed Event.
    let late = wake.callback();
    drop(wake);
    std::thread::spawn(move || late())
        .join()
        .map_err(|_| anyhow::anyhow!("Late owned wake publisher panicked"))?;
    result
}

fn choice_wait(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    mode: &str,
    mut receipts: Option<&mut Vec<Command>>,
) -> Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut events = Vec::new();
    loop {
        fixture.check()?;
        events.extend(choice_pump_receipts(
            controller,
            overlay,
            receipts.as_deref_mut(),
        )?);
        if events.iter().any(|event| {
            event["type"] == "suggestion"
                && (event["mode"] == mode || event["choices_mode"] == mode)
        }) {
            return Ok(events);
        }
        ensure!(
            Instant::now() < deadline,
            "Real worker did not publish owned {mode} fixture"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn choice_offer(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    values: Vec<String>,
    selected: bool,
    receipts: Option<&mut Vec<Command>>,
) -> Result<Vec<Value>> {
    // Synonyms stream one JSON array; alternatives stream individual completions,
    // but both paths ultimately use parse_choices (including trailing-space
    // trimming). Fixture rows must already be valid exact offered strings.
    // Independently compare the real event below; never inject rendered choices.
    let mode = if selected { "synonyms" } else { "alternatives" };
    let serialized = serde_json::to_string(&values)?;
    ensure!(
        crate::features::parse_choices(&serialized, 5) == values,
        "Owned {mode} fixture rows do not survive final choice parsing unchanged"
    );
    fixture.arm(if selected {
        vec![serialized]
    } else {
        values.clone()
    })?;
    controller.send(Command::Action(if selected {
        crate::platform::Action::Synonyms
    } else {
        crate::platform::Action::Alternatives
    }))?;
    let events = choice_wait(controller, overlay, fixture, mode, receipts)?;
    let offer = events
        .iter()
        .find(|event| {
            event["type"] == "suggestion"
                && (event["mode"] == mode || event["choices_mode"] == mode)
        })
        .context("Missing real provider offer")?;
    ensure!(
        offer["choices"] == json!(values),
        "Owned {mode} provider parsing changed/dropped a fixture row"
    );
    ensure!(
        fixture
            .samples
            .lock()
            .map_err(|_| anyhow::anyhow!("Fixture queue poisoned"))?
            .is_empty(),
        "Provider did not consume the scripted samples"
    );
    Ok(events)
}

fn choice_completion(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
) -> Result<Vec<Value>> {
    fixture.arm(vec!["tomorrow".into()])?;
    controller.send(Command::Action(crate::platform::Action::Force))?;
    let events = choice_wait(controller, overlay, fixture, "completion", None)?;
    ensure!(
        events.iter().any(|event| event["type"] == "suggestion"
            && event["mode"] == "completion"
            && event["text"] == "tomorrow"),
        "Ordinary provider fixture changed"
    );
    Ok(events)
}

/// Event/ownership barriers, never elapsed time used as evidence. Yielding only
/// lets the real worker and AppKit make progress; every exit checks a predicate.
#[track_caller]
fn lifecycle_barrier(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    events: &mut Vec<Value>,
    ready: impl Fn(&[Value], &crate::diagnostics::DiagnosticSnapshot) -> bool,
) -> Result<crate::diagnostics::DiagnosticSnapshot> {
    lifecycle_barrier_receipts(controller, overlay, fixture, events, None, ready)
}

#[track_caller]
fn lifecycle_barrier_receipts(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    events: &mut Vec<Value>,
    mut receipts: Option<&mut Vec<Command>>,
    ready: impl Fn(&[Value], &crate::diagnostics::DiagnosticSnapshot) -> bool,
) -> Result<crate::diagnostics::DiagnosticSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        fixture.check()?;
        events.extend(choice_pump_receipts(
            controller,
            overlay,
            receipts.as_deref_mut(),
        )?);
        let diagnostics = controller.diagnostics();
        if ready(events, &diagnostics) {
            return Ok(diagnostics);
        }
        ensure!(
            Instant::now() < deadline,
            "Owned lifecycle barrier at {} expired: {}",
            std::panic::Location::caller(),
            json!({
                "events": events.len(),
                "suggestions": events.iter().filter(|event| event["type"] == "suggestion").count(),
                "acknowledgments": events.iter().filter(|event| event["type"] == "owned_presentation_ack").count(),
                // This opt-in fixture can expose only its fixed synthetic responses.
                "publications": events.iter().rev()
                    .filter(|event| matches!(event["type"].as_str(), Some("suggestion" | "owned_presentation_ack")))
                    .take(8)
                    .map(|event| json!({
                        "type": event["type"], "mode": event["mode"],
                        "text": event["text"], "visible_text": event["visible_text"],
                        "revision": event["revision"], "diagnostic": event["diagnostic"],
                    })).collect::<Vec<_>>(),
                "last_trace": diagnostics.traces.last(),
            })
        );
        std::thread::yield_now();
    }
}

#[track_caller]
fn lifecycle_presented(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    events: &mut Vec<Value>,
    text: &str,
    receipts: Option<&mut Vec<Command>>,
) -> Result<Value> {
    lifecycle_barrier_receipts(
        controller,
        overlay,
        fixture,
        events,
        receipts,
        |events, diagnostics| {
            events.iter().any(|offer| {
                offer["type"] == "suggestion"
                    && offer["mode"] == "completion"
                    && offer["text"] == text
                    && events.iter().any(|ack| {
                        ack["type"] == "owned_presentation_ack"
                            && ack["mode"] == "completion"
                            && ack["visible_text"] == text
                            && ack["key"] == offer["key"]
                            && ack["authority"] == offer["authority"]
                            && ack["revision"] == offer["revision"]
                    })
                    && diagnostics.traces.iter().any(|trace| {
                        Some(trace.trace_id) == offer["diagnostic"]["trace_id"].as_u64()
                            && Some(trace.view_serial)
                                == offer["diagnostic"]["view_serial"].as_u64()
                            && trace.lane == crate::diagnostics::PresentationLane::Inline
                            && trace.first_draw_us.is_some()
                    })
            })
        },
    )?;
    events
        .iter()
        .rev()
        .find(|event| {
            event["type"] == "suggestion" && event["mode"] == "completion" && event["text"] == text
        })
        .cloned()
        .context("Owned lifecycle lost its presented event")
}

#[track_caller]
fn lifecycle_terminal(
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    counts: &Rc<RefCell<BTreeMap<u64, ChoiceWorkerCounts>>>,
    offer: &Value,
    events: &mut Vec<Value>,
) -> Result<crate::diagnostics::TraceSnapshot> {
    let id = offer["diagnostic"]["trace_id"]
        .as_u64()
        .context("Missing owned trace ID")?;
    let diagnostics = lifecycle_barrier(controller, overlay, fixture, events, |_, diagnostics| {
        diagnostics.traces.iter().any(|trace| {
            trace.trace_id == id
                && trace.terminal.as_ref().is_some_and(|terminal| {
                    Some(terminal.request_id) == trace.request_id
                        && terminal.reason == crate::diagnostics::TerminalReason::Completed
                        && terminal.proceed_us.is_some()
                        && counts
                            .borrow()
                            .get(&terminal.request_id)
                            .is_some_and(|count| count.finals == 1 && count.done == 1)
                })
        })
    })?;
    diagnostics
        .traces
        .into_iter()
        .find(|trace| trace.trace_id == id)
        .context("Owned terminal trace disappeared")
}

fn lifecycle_notify(host: &Rc<RefCell<ChoiceHost>>) {
    let changed = {
        let mut host = host.borrow_mut();
        host.revision += 1;
        host.changed.clone()
    };
    if let Some(changed) = changed {
        changed();
    }
}

fn lifecycle_widgets(host: &ChoiceHost) -> Value {
    let fields: Vec<_> = host.editors.iter().map(|editor| {
        let selection: NSRange = unsafe { msg_send![&**editor, selectedRange] };
        json!({"text":string(editor),"caret_utf16":selection.location,"selection_utf16":selection.length})
    }).collect();
    json!({"fields":fields,"native_edits":host.edits,"focused_field":host.focused()})
}

fn lifecycle_accepted(controller: &crate::runtime::Controller) -> usize {
    controller
        .diagnostics()
        .traces
        .iter()
        .filter(|trace| trace.outcome == Some(crate::diagnostics::Outcome::Accepted))
        .count()
}

#[allow(clippy::too_many_arguments)]
fn capture_owned_lifecycle(
    app: &AnyObject,
    output: &Path,
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    host: &Rc<RefCell<ChoiceHost>>,
    counts: &Rc<RefCell<BTreeMap<u64, ChoiceWorkerCounts>>>,
) -> Result<Value> {
    use crate::platform::Action;
    use std::sync::atomic::Ordering;
    host.borrow_mut().seed(0, "Draft ", NSRange::new(6, 0))?;
    choice_pump(controller, overlay)?;
    let edits = host.borrow().edits;
    let accepted_before = lifecycle_accepted(controller);
    fixture.hold_terminal.store(true, Ordering::Release);
    // Initial streaming withholds the generated trailing delimiter. Terminal
    // results restore that exact payload once, even if the painted prefix is equal.
    let sample = "tomorrow café 😀 ";
    ensure!(
        crate::providers::clean_completion(sample, "Draft ", "", false) == sample,
        "Owned lifecycle fixture is not a canonical ordinary completion"
    );
    fixture.arm(vec![sample.into()])?;
    controller.send(Command::Action(Action::Force))?;
    let mut offered = Vec::new();
    let offer = lifecycle_presented(
        controller,
        overlay,
        fixture,
        &mut offered,
        "tomorrow café 😀",
        None,
    )?;
    let first_draw = capture_owned_ghost(app, output, "lifecycle-original-ghost")?;
    let first_geometry = overlay.smoke_ghost_diagnostic();
    let captures = host.borrow().captures.get();
    let mut unchanged = Vec::new();
    lifecycle_barrier(controller, overlay, fixture, &mut unchanged, |_, _| {
        host.borrow().captures.get() >= captures + 2
    })?;
    ensure!(
        !unchanged.iter().any(|event| event["type"] == "suggestion"),
        "Unchanged native captures republished the ghost"
    );
    let unchanged_captures = host.borrow().captures.get() - captures;
    fixture.hold_terminal.store(false, Ordering::Release);
    let mut final_events = Vec::new();
    let terminal = lifecycle_terminal(
        controller,
        overlay,
        fixture,
        counts,
        &offer,
        &mut final_events,
    )?;
    // fit_text preserves all bytes when this short payload fits the owned row;
    // present_ghost draws and acknowledges that same string, including its space.
    // Require the actual final native receipt and draw, not just worker terminality.
    lifecycle_presented(
        controller,
        overlay,
        fixture,
        &mut final_events,
        sample,
        None,
    )?;
    let final_draw = capture_owned_ghost(app, output, "lifecycle-final-ghost")?;
    let final_geometry = overlay.smoke_ghost_diagnostic();
    let publications: Vec<_> = final_events
        .iter()
        .filter(|event| event["type"] == "suggestion")
        .collect();
    ensure!(
        publications.len() == 1,
        "Expected one final payload progression, not duplicate publications: initial={}, publications={}",
        offer,
        json!(publications)
    );
    let final_offer = publications[0];
    // Revision and diagnostic view identity must refresh for a changed payload;
    // every other rendering, key, authority and choice field must stay exact.
    let mut expected_final = offer.clone();
    expected_final["text"] = json!(sample);
    let mut final_identity = final_offer.clone();
    for identity in [&mut expected_final, &mut final_identity] {
        let fields = identity
            .as_object_mut()
            .context("Non-object completion publication")?;
        fields.remove("revision");
        fields.remove("diagnostic");
    }
    ensure!(
        final_identity == expected_final
            && final_offer["revision"].as_u64() > offer["revision"].as_u64(),
        "Final publication changed more than its generated trailing space: initial={}, final={}",
        offer,
        final_offer
    );
    let request = terminal
        .request_id
        .context("Generated trace has no request")?;
    let delivered = counts
        .borrow()
        .get(&request)
        .context("Missing delivered worker events")?
        .clone();
    ensure!(
        delivered.results >= 2 && delivered.finals == 1 && delivered.done == 1,
        "Fixture did not deliver separate partial/final completion events"
    );
    ensure!(
        delivered.events.iter().any(|event| {
            event["kind"] == "result" && event["final"] == true && event["text"] == sample
        }) && delivered
            .events
            .last()
            .is_some_and(|event| event["kind"] == "done"),
        "Real worker final/done semantics changed: {}",
        json!(delivered)
    );
    let final_readback = choice_readback(&host.borrow(), 0, "Draft ", 6, edits)?;
    let stable_counts = counts.borrow().clone();

    // Real insertText on our NSTextView, followed by an explicitly synthetic
    // owned notification. No keyboard event or input timestamp is fabricated.
    let notified = Instant::now();
    {
        let host = host.borrow();
        ensure!(
            host.focused() == Some(0),
            "Owned typed-through field lost focus"
        );
        unsafe {
            let _: () = msg_send![&*host.editors[0],insertText:&*NSString::from_str("tom"),
                replacementRange:NSRange::new(6,0)];
        }
    }
    lifecycle_notify(host);
    let mut typed_events = Vec::new();
    let mut typed_receipts = Vec::new();
    let typed = lifecycle_presented(
        controller,
        overlay,
        fixture,
        &mut typed_events,
        "orrow café 😀 ",
        Some(&mut typed_receipts),
    )?;
    let synthetic_notification_to_barrier_us = notified.elapsed().as_micros();
    let typed_readback = choice_readback(&host.borrow(), 0, "Draft tom", 9, edits)?;
    let typed_draw = capture_owned_ghost(app, output, "lifecycle-typed-through-ghost")?;
    let typed_geometry = overlay.smoke_ghost_diagnostic();
    let typed_id = typed["diagnostic"]["trace_id"]
        .as_u64()
        .context("Missing retained trace")?;
    let typed_trace = controller
        .diagnostics()
        .traces
        .into_iter()
        .find(|trace| trace.trace_id == typed_id)
        .context("Missing typed-through trace")?;
    ensure!(
        typed_trace.kind == crate::diagnostics::WorkKind::RetainedGhost
            && typed_trace.request_id.is_none()
            && typed_trace.input_wait_us.is_none()
            && *counts.borrow() == stable_counts,
        "Typed-through ghost generated work or invented physical input timing"
    );

    let stale_ack = typed_receipts
        .into_iter()
        .find(|command| {
            matches!(command,
            Command::PresentationResult { key, authority, revision, mode, visible_text }
                if mode == "completion"
                    && visible_text == "orrow café 😀 "
                    && Some(*revision) == typed["revision"].as_u64()
                    && json!(key) == typed["key"]
                    && json!(authority) == typed["authority"])
        })
        .context("Typed-through ghost has no actual native presentation receipt")?;
    // Queue two shortcuts under the same real claim before processing either:
    // the first partial insertion must invalidate the queued stale repeat.
    let shortcut = host
        .borrow()
        .shortcut
        .clone()
        .context("Missing owned shortcut")?;
    ensure!(shortcut(Action::Word), "Typed-through claim was not armed");
    let repeated_claim = shortcut(Action::Word);
    let mut partial_events = Vec::new();
    let remainder = lifecycle_presented(
        controller,
        overlay,
        fixture,
        &mut partial_events,
        " café 😀 ",
        None,
    )?;
    let partial = choice_readback(&host.borrow(), 0, "Draft tomorrow", 14, edits + 1)?;
    ensure!(
        lifecycle_accepted(controller) == accepted_before + 1
            && controller
                .diagnostics()
                .traces
                .iter()
                .any(|trace| trace.trace_id == typed_id
                    && trace.outcome == Some(crate::diagnostics::Outcome::Accepted)),
        "Partial acceptance was not committed exactly once to its trace"
    );
    ensure!(
        shortcut(Action::All),
        "Remainder lost full-accept ownership"
    );
    let repeated_full_claim = shortcut(Action::All);
    let mut full_events = Vec::new();
    lifecycle_barrier(
        controller,
        overlay,
        fixture,
        &mut full_events,
        |_, diagnostics| {
            diagnostics.traces.iter().any(|trace| {
                Some(trace.trace_id) == remainder["diagnostic"]["trace_id"].as_u64()
                    && trace.outcome == Some(crate::diagnostics::Outcome::Accepted)
            })
        },
    )?;
    let expected = "Draft tomorrow café 😀";
    let full = choice_readback(
        &host.borrow(),
        0,
        expected,
        expected.encode_utf16().count(),
        edits + 2,
    )?;
    ensure!(
        lifecycle_accepted(controller) == accepted_before + 2 && *counts.borrow() == stable_counts,
        "Acceptance counted twice or invoked generation"
    );
    controller.send(stale_ack)?;
    controller.send(Command::Action(Action::All))?;
    let stale_full = choice_pump(controller, overlay)?;
    ensure!(
        stale_full
            .iter()
            .any(|event| event["type"] == "action_result" && event["handled"] == false)
            && lifecycle_accepted(controller) == accepted_before + 2,
        "Fully accepted ghost allowed a stale repeat"
    );
    choice_readback(
        &host.borrow(),
        0,
        expected,
        expected.encode_utf16().count(),
        edits + 2,
    )?;

    let invalidations =
        capture_owned_invalidations(app, output, controller, overlay, fixture, host, counts)?;
    Ok(json!({
        "scope":"owned NSTextView/Controller lifecycle, not generic AX or physical input",
        "unchanged_capture_count":unchanged_captures,"unchanged_capture_publications":0,
        "final_payload_publications":publications.len(),"duplicate_final_publications":0,
        "worker_client_delivered_events":delivered,
        "worker_count_scope":"WorkerClient::poll delivered events; not raw wire events",
        "original_offer":offer,"original_events":offered,"unchanged_events":unchanged,
        "final_offer":final_offer,"final_readback":final_readback,
        "final_events":final_events,"terminal_trace":terminal,
        "final_draw":final_draw,"final_geometry":final_geometry,
        "first_draw":first_draw,"first_geometry":first_geometry,
        "typed_through":{"offer":typed,"events":typed_events,"readback":typed_readback,
            "trace":typed_trace,"draw":typed_draw,"geometry":typed_geometry,
            "synthetic_owned_notification_to_barrier_us":synthetic_notification_to_barrier_us,
            "physical_key_latency":false},
        "partial":{"readback":partial,"events":partial_events,"duplicate_hook_consumed":repeated_claim},
        "full":{"readback":full,"events":full_events,"stale_events":stale_full,
            "duplicate_hook_consumed":repeated_full_claim},
        "accepted_trace_delta":2,"native_acceptance_edit_delta":2,
        "history_statistics_provenance_proved":false,"invalidations":invalidations
    }))
}

#[allow(clippy::too_many_arguments)]
fn capture_owned_invalidations(
    app: &AnyObject,
    output: &Path,
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    fixture: &CompletionFixture,
    host: &Rc<RefCell<ChoiceHost>>,
    counts: &Rc<RefCell<BTreeMap<u64, ChoiceWorkerCounts>>>,
) -> Result<Vec<Value>> {
    use crate::platform::Action;
    // A real clip view is necessary: moving synthetic anchor coordinates would
    // not exercise native scroll geometry or clipping.
    let scroll = obj("NSScrollView");
    let clip: Obj;
    {
        let host = host.borrow();
        ensure!(
            host.focused() == Some(0),
            "Owned scroll setup lost its original editor"
        );
        let root = content(&host.window)?;
        unsafe {
            let frame: NSRect = msg_send![&*host.editors[0], frame];
            let _: () = msg_send![&*host.editors[0], removeFromSuperview];
            let _: () = msg_send![&*scroll,setFrame:frame];
            let _: () = msg_send![&*scroll,setHasVerticalScroller:true];
            let _: () = msg_send![&*host.editors[0],setFrame:rect(0.,0.,680.,110.)];
            let _: () = msg_send![&*scroll,setDocumentView:&*host.editors[0]];
            let raw: *mut AnyObject = msg_send![&*scroll, contentView];
            clip = Retained::retain(raw).context("Owned scroll view has no clip view")?;
        }
        add(&root, &scroll);
        // Reparenting can clear first responder. Restore only within the same
        // already-active owned key window; never activate or raise an application.
        unsafe {
            let active: bool = msg_send![app, isActive];
            let key: *mut AnyObject = msg_send![app, keyWindow];
            ensure!(
                active && std::ptr::eq(&*host.window, key),
                "Owned scroll setup lost its key window; refusing focus restoration"
            );
            let restored: bool = msg_send![&*host.window,makeFirstResponder:&*host.editors[0]];
            ensure!(
                restored && host.focused() == Some(0),
                "Reparented owned editor refused first responder"
            );
        }
    }
    let mut cases = Vec::new();
    for case in [
        "same-process-field",
        "selection",
        "suffix",
        "focus",
        "scroll",
        "destruction",
    ] {
        let before = if case == "scroll" {
            format!("Draft \n{}", "owned scroll line\n".repeat(80))
        } else if case == "suffix" {
            "Draft \n— café 👩‍💻".into()
        } else {
            "Draft ".into()
        };
        host.borrow_mut().seed(0, &before, NSRange::new(6, 0))?;
        if case == "scroll" {
            let host = host.borrow();
            unsafe {
                let _: () = msg_send![&*host.editors[0],setFrameSize:NSSize::new(680.,2000.)];
                let _: () = msg_send![&*clip,scrollToPoint:NSPoint::new(0.,0.)];
                let _: () = msg_send![&*scroll,reflectScrolledClipView:&*clip];
            }
        }
        choice_pump(controller, overlay)?;
        fixture.arm(vec!["tomorrow".into()])?;
        controller.send(Command::Action(Action::Force))?;
        let mut events = Vec::new();
        let mut receipts = Vec::new();
        let offer = lifecycle_presented(
            controller,
            overlay,
            fixture,
            &mut events,
            "tomorrow",
            Some(&mut receipts),
        )?;
        let terminal =
            lifecycle_terminal(controller, overlay, fixture, counts, &offer, &mut events)?;
        let draw = capture_owned_ghost(app, output, &format!("lifecycle-{case}-ghost"))?;
        let draw_metadata = overlay.smoke_ghost_diagnostic();
        let old_ack = receipts
            .into_iter()
            .find(|command| {
                matches!(command,
                Command::PresentationResult { key, authority, revision, mode, visible_text }
                    if mode == "completion"
                        && visible_text == "tomorrow"
                        && Some(*revision) == offer["revision"].as_u64()
                        && json!(key) == offer["key"]
                        && json!(authority) == offer["authority"])
            })
            .with_context(|| format!("{case}: ghost has no actual native presentation receipt"))?;
        let edits = host.borrow().edits;
        let accepted = lifecycle_accepted(controller);
        let delivered = counts.borrow().clone();
        let geometry_before: NSRect = unsafe { msg_send![&*clip, bounds] };
        ensure!(
            host.borrow().focused() == Some(0),
            "Owned invalidation lost its source field"
        );
        if case == "same-process-field" {
            host.borrow_mut().seed(1, &before, NSRange::new(6, 0))?;
            ensure!(
                host.borrow().focused() == Some(1),
                "Same-process field switch failed"
            );
        } else {
            let host = host.borrow();
            unsafe {
                match case {
                    "selection" => {
                        let _: () = msg_send![&*host.editors[0],setSelectedRange:NSRange::new(1,2)];
                    }
                    "suffix" => {
                        let _: () = msg_send![&*host.editors[0],insertText:&*NSString::from_str("!"),
                            replacementRange:NSRange::new(before.encode_utf16().count(),0)];
                        let _: () = msg_send![&*host.editors[0],setSelectedRange:NSRange::new(6,0)];
                        ensure!(
                            string(&host.editors[0]) == format!("{before}!"),
                            "Native suffix mutation failed"
                        );
                    }
                    "focus" => {
                        let changed: bool = msg_send![&*host.window,makeFirstResponder:std::ptr::null::<AnyObject>()];
                        ensure!(
                            changed && host.focused().is_none(),
                            "Owned focus did not leave the editor"
                        );
                    }
                    "scroll" => {
                        let _: () = msg_send![&*clip,scrollToPoint:NSPoint::new(0.,600.)];
                        let _: () = msg_send![&*scroll,reflectScrolledClipView:&*clip];
                        let after: NSRect = msg_send![&*clip, bounds];
                        ensure!(
                            after.origin != geometry_before.origin,
                            "Native clip view did not scroll"
                        );
                    }
                    "destruction" => {
                        let _: () =
                            msg_send![&*scroll,setDocumentView:std::ptr::null::<AnyObject>()];
                        let _: () = msg_send![&*host.window, close];
                        let visible: bool = msg_send![&*host.window, isVisible];
                        ensure!(
                            !visible && host.focused().is_none(),
                            "Owned host did not close/detach"
                        );
                    }
                    _ => unreachable!(),
                }
            }
        }
        lifecycle_notify(host);
        let mutation = lifecycle_widgets(&host.borrow());
        if case == "selection" {
            ensure!(
                mutation["fields"][0]["caret_utf16"] == 1
                    && mutation["fields"][0]["selection_utf16"] == 2,
                "Native selection mutation failed"
            );
        }
        let mut invalidated = Vec::new();
        lifecycle_barrier(
            controller,
            overlay,
            fixture,
            &mut invalidated,
            |events, diagnostics| {
                events.iter().any(|event| {
                    event["type"] == "clear"
                        || (event["type"] == "owned_presentation_ack"
                            && event["visible_text"] == "")
                }) || diagnostics.traces.iter().any(|trace| {
                    Some(trace.trace_id) == offer["diagnostic"]["trace_id"].as_u64()
                        && trace.outcome.is_some()
                        && trace.outcome != Some(crate::diagnostics::Outcome::Accepted)
                })
            },
        )?;
        controller.send(old_ack)?;
        controller.send(Command::Action(Action::All))?;
        let mut rejected = Vec::new();
        lifecycle_barrier(controller, overlay, fixture, &mut rejected, |events, _| {
            events
                .iter()
                .any(|event| event["type"] == "action_result" && event["handled"] == false)
        })?;
        ensure!(
            host.borrow().edits == edits
                && lifecycle_widgets(&host.borrow()) == mutation
                && lifecycle_accepted(controller) == accepted
                && *counts.borrow() == delivered,
            "Stale lifecycle acceptance changed a widget, accepted count or worker events"
        );
        let shortcut = host
            .borrow()
            .shortcut
            .clone()
            .context("Missing lifecycle shortcut")?;
        ensure!(
            !shortcut(Action::All),
            "Invalidated presentation rearmed a shortcut"
        );
        rejected.extend(choice_pump(controller, overlay)?);
        ensure!(
            lifecycle_widgets(&host.borrow()) == mutation
                && lifecycle_accepted(controller) == accepted
                && *counts.borrow() == delivered,
            "Rejected owned shortcut changed committed state"
        );
        let geometry_after: NSRect = unsafe { msg_send![&*clip, bounds] };
        cases.push(json!({"case":case,"offer":offer,"offer_events":events,"terminal":terminal,
            "draw":draw,"draw_metadata":draw_metadata,"mutation_readback":mutation,
            "invalidation_events":invalidated,"rejected_events":rejected,
            "clip_bounds_before":layout_rect(geometry_before),"clip_bounds_after":layout_rect(geometry_after),
            "native_edit_delta":0,"accepted_trace_delta":0,"worker_events_unchanged":true,
            "destruction_scope":if case == "destruction" {
                "real NSWindow close and NSTextView detach; retained handles allow readback, not Objective-C deallocation"
            } else { "" }}));
        if case == "focus" {
            let host = host.borrow();
            unsafe {
                let active: bool = msg_send![app, isActive];
                let key: bool = msg_send![&*host.window, isKeyWindow];
                ensure!(
                    active && key,
                    "Unexpected focus loss; refusing owned focus restoration"
                );
                let restored: bool = msg_send![&*host.window,makeFirstResponder:&*host.editors[0]];
                ensure!(restored, "Owned first responder restoration failed");
            }
        } else if case == "scroll" {
            let host = host.borrow();
            unsafe {
                let _: () = msg_send![&*host.editors[0],setFrameSize:NSSize::new(680.,110.)];
                let _: () = msg_send![&*clip,scrollToPoint:NSPoint::new(0.,0.)];
                let _: () = msg_send![&*scroll,reflectScrolledClipView:&*clip];
            }
        }
    }
    Ok(cases)
}

/// Select a real acknowledgment only after its current publication actually draws.
fn model_drawn_offer(
    events: &[Value],
    receipts: &[Command],
    diagnostics: &crate::diagnostics::DiagnosticSnapshot,
    trace_id: u64,
) -> Option<(Value, String, Command)> {
    events.iter().rev().find_map(|offer| {
        if offer["type"] != "suggestion"
            || offer["mode"] != "completion"
            || offer["diagnostic"]["trace_id"].as_u64() != Some(trace_id)
            || !diagnostics.traces.iter().any(|trace| {
                trace.trace_id == trace_id
                    && Some(trace.view_serial) == offer["diagnostic"]["view_serial"].as_u64()
                    && trace.lane == crate::diagnostics::PresentationLane::Inline
                    && matches!(
                        (
                            trace.stage_us[crate::diagnostics::Stage::ProviderPublication as usize],
                            trace.stage_us[crate::diagnostics::Stage::NativeDraw as usize],
                        ),
                        (Some(published), Some(drawn)) if drawn >= published
                    )
            })
        {
            return None;
        }
        receipts.iter().rev().find_map(|receipt| match receipt {
            Command::PresentationResult {
                key,
                authority,
                revision,
                visible_text,
                mode,
            } if mode == "completion"
                && !visible_text.is_empty()
                && json!(key) == offer["key"]
                && json!(authority) == offer["authority"]
                && Some(*revision) == offer["revision"].as_u64()
                && offer["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with(visible_text.as_str())) =>
            {
                Some((offer.clone(), visible_text.clone(), receipt.clone()))
            }
            _ => None,
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn capture_model_timeline(
    app: &AnyObject,
    output: &Path,
    controller: &crate::runtime::Controller,
    overlay: &mut OverlayOwner,
    host: &Rc<RefCell<ChoiceHost>>,
    counts: &Rc<RefCell<BTreeMap<u64, ChoiceWorkerCounts>>>,
    config: &Config,
    evidence: &mut Value,
) -> Result<Value> {
    use crate::{
        diagnostics::{Outcome, Stage, TerminalReason},
        platform::Action,
    };
    const REQUEST_DRAW_LIMIT: Duration = Duration::from_secs(120);
    const ACCEPTANCE_LIMIT: Duration = Duration::from_secs(5);
    const PUMP_INTERVAL: Duration = Duration::from_millis(2);
    const MATRIX_VERSION: &str = "owned-model-en-pt-original-and-preview-v1";
    const INPUTS: [(&str, &str, &str, bool); 4] = [
        (
            "original",
            "en",
            "I reviewed the proposal and I think we should",
            false,
        ),
        (
            "original",
            "pt",
            "Eu revisei a proposta e acho que precisamos",
            true,
        ),
        ("canonical_preview", "en", "I was thinking we could", false),
        (
            "canonical_preview",
            "pt",
            "Eu estava pensando que poderíamos",
            true,
        ),
    ];
    evidence["matrix_version"] = json!(MATRIX_VERSION);
    evidence["fixed_inputs"] = json!(
        INPUTS
            .iter()
            .enumerate()
            .map(|(ordinal, (source, language, input, full))| json!({
                "request_ordinal":ordinal,"source":source,"language":language,"input":input,
                "acceptance":if *full { "full_displayed_prefix" } else { "word" }
            }))
            .collect::<Vec<_>>()
    );
    evidence["cases"] = json!([]);
    evidence["clock_contract"] = json!({
        "input_callback":"parent process Instant; offsets from owned host construction",
        "trace":"existing DiagnosticSnapshot stage offsets from first capture start; input_wait_us precedes that origin",
        "terminal":"existing TerminalSnapshot offsets from WorkerClient submission, not capture",
        "native":"NativeWork decode elapsed_us brackets native calls; not synchronized GPU execution",
        "cold_warm":"fixed request order in one worker is recorded separately from observed cold_evaluations/reuse counters; no warm-cache claim inferred from order",
        "unreported":"missing metrics or terminal reports remain unavailable, never zero; all retained traces and lost/unmatched counters are included",
        "excluded":["physical-key latency","unified cross-process clock","GPU/compositor timing","full64 semantic-quality gate"]
    });
    evidence["work_limits"] = json!({
        "matrix_version":MATRIX_VERSION,
        "fixed_inputs":INPUTS.len(),
        "generation_requests_per_input":1,
        "worker_owners":1,
        "request_and_current_draw_timeout_ms":REQUEST_DRAW_LIMIT.as_millis(),
        "acceptance_timeout_ms":ACCEPTANCE_LIMIT.as_millis(),
        "request_pump_interval_ms":PUMP_INTERVAL.as_millis(),
        "no_offer":"verify correlated terminal/work, no publication/draw/acceptance/edit, unchanged independent readback and stale refusal; retain and continue; never positive acceptance coverage",
        "cancellation_or_failure":"retain case error and continue fixed remaining inputs without retry; final matrix fails; physical input or ownership loss aborts directly to cleanup",
        "positive_coverage":"actual offered/current-view-drawn/accepted cases required for both en and pt; no language substitution",
        "draw_gate":"same trace, inline lane and current view; NativeDraw >= current ProviderPublication, through normal owned AppKit pumping and the existing paint reporter",
        "work_gate":"reported evaluations, input_tokens and sampled_tokens must each be positive; exactly one delivered final and done per request",
        "token_limit":config.max_tokens,
        "completion_length":config.completion_length,
        "semantic_quality_gate":false
    });
    let mut worker_pid = None;
    let mut positive_languages = std::collections::BTreeSet::new();
    let mut stale_offer_receipt: Option<Command> = None;
    // Predeclared inputs run once unless a terminal input/ownership fence aborts.
    for (ordinal, (source, language, input, full)) in INPUTS.into_iter().enumerate() {
        let mut events = Vec::new();
        let mut receipts = Vec::new();
        let baseline = controller
            .diagnostics()
            .traces
            .last()
            .map_or(0, |trace| trace.trace_id);
        let edits_before = host.borrow().edits;
        let accepted_before = lifecycle_accepted(controller);
        let mut case = json!({"language":language,"source":source,"request_ordinal":ordinal,
            "residency_position":if ordinal == 0 { "first_request" } else { "same_worker_subsequent_request" },
            "input":input,"passed":false,"positive_acceptance":false,"verified_no_offer":false,
            "acceptance":if full { "full_displayed_prefix" } else { "word" }});
        let result = (|| -> Result<()> {
            choice_owned_events()?;
            host.borrow_mut()
                .seed(0, input, NSRange::new(input.encode_utf16().count(), 0))?;
            case["seed_readback"] = choice_readback(
                &host.borrow(),
                0,
                input,
                input.encode_utf16().count(),
                edits_before,
            )?;
            let deadline = Instant::now() + REQUEST_DRAW_LIMIT;
            let terminal_trace = loop {
                events.extend(choice_pump_receipts(
                    controller,
                    overlay,
                    Some(&mut receipts),
                )?);
                let diagnostics = controller.diagnostics();
                if let Some(trace) = diagnostics.traces.iter().find(|trace| {
                    trace.trace_id > baseline
                        && trace.request_id.is_some()
                        && trace.terminal.is_some()
                }) {
                    let terminal = trace.terminal.as_ref().unwrap();
                    if terminal.reason != TerminalReason::Completed
                        || trace.outcome == Some(Outcome::NoOffer)
                        || model_drawn_offer(&events, &receipts, &diagnostics, trace.trace_id)
                            .is_some()
                    {
                        break trace.clone();
                    }
                }
                ensure!(
                    Instant::now() < deadline,
                    "Owned model request/draw deadline expired; no retry"
                );
                // The next normal AppKit turn draws any publication installed by
                // this pump. First-draw latency may belong to an earlier view.
                std::thread::sleep(PUMP_INTERVAL);
            };
            case["terminal_trace"] = json!(terminal_trace);
            let terminal = terminal_trace
                .terminal
                .as_ref()
                .context("Missing terminal report")?;
            ensure!(
                terminal.reason == TerminalReason::Completed
                    && !terminal.cancelled
                    && !terminal.failed,
                "Owned model work did not complete"
            );
            let metrics = terminal
                .metrics
                .as_ref()
                .context("Owned model native work is unreported")?;
            let native = metrics.native;
            case["native_work"] = json!(native);
            ensure!(
                native.evaluations > 0 && native.input_tokens > 0 && native.sampled_tokens > 0,
                "Owned model did not report actual inference"
            );
            ensure!(
                terminal_trace.request_id == Some(terminal.request_id)
                    && !terminal.preparing
                    && terminal.terminal_us.is_some()
                    && terminal.ready_us.is_some()
                    && terminal.proceed_us.is_some()
                    && terminal.first_result_us.is_some()
                    && terminal_trace.input_wait_us.is_some()
                    && [
                        Stage::Capture,
                        Stage::Preparation,
                        Stage::Ready,
                        Stage::Proceed
                    ]
                    .iter()
                    .all(|stage| terminal_trace.stage_us[*stage as usize].is_some()),
                "Owned model is missing correlated capture/preparation/ready/proceed/terminal work"
            );
            ensure!(
                counts
                    .borrow()
                    .get(&terminal.request_id)
                    .is_some_and(|count| count.finals == 1 && count.done == 1),
                "Owned model terminal does not match delivered final/done events"
            );
            let pid = controller.status()["worker_pid"]
                .as_u64()
                .context("No real worker PID")?;
            ensure!(
                worker_pid.is_none_or(|previous| previous == pid),
                "Owned inference changed worker owner"
            );
            worker_pid = Some(pid);
            case["worker_pid"] = json!(pid);
            if terminal_trace.outcome == Some(Outcome::NoOffer) || metrics.no_offer {
                case["observed_outcome"] = json!("no_offer");
                case["no_offer_reason"] =
                    json!("Completed owned model request produced no offer; no retry");
                ensure!(
                    terminal_trace.outcome == Some(Outcome::NoOffer) && metrics.no_offer,
                    "Owned no-offer terminal metrics and trace disagree"
                );
                let no_publication_or_draw = |trace: &crate::diagnostics::TraceSnapshot| {
                    trace.stage_us[Stage::ProviderPublication as usize].is_none()
                        && trace.stage_us[Stage::NativeDraw as usize].is_none()
                        && trace.first_publication_us.is_none()
                        && trace.first_draw_us.is_none()
                };
                ensure!(
                    no_publication_or_draw(&terminal_trace)
                        && receipts.is_empty()
                        && !events.iter().any(|event| event["type"] == "suggestion"),
                    "Owned no-offer unexpectedly published or drew an offer"
                );
                case["readback"] = choice_readback(
                    &host.borrow(),
                    0,
                    input,
                    input.encode_utf16().count(),
                    edits_before,
                )?;
                let shortcut = host
                    .borrow()
                    .shortcut
                    .clone()
                    .context("Missing real shortcut callback")?;
                let mut refusals = Vec::new();
                for (name, action) in [("word", Action::Word), ("full", Action::All)] {
                    if let Some(receipt) = &stale_offer_receipt {
                        controller.send(receipt.clone())?;
                        events.extend(choice_pump_receipts(
                            controller,
                            overlay,
                            Some(&mut receipts),
                        )?);
                    }
                    let claimed = shortcut(action.clone());
                    refusals.push(json!({"action":name,"claimed":claimed}));
                    case["no_offer_shortcut_claims"] = json!(refusals);
                    ensure!(!claimed, "Owned no-offer claimed an acceptance shortcut");
                    controller.send(Command::Action(action))?;
                    let refused = choice_pump_receipts(controller, overlay, Some(&mut receipts))?;
                    let refused_action = refused
                        .iter()
                        .any(|event| event["type"] == "action_result" && event["handled"] == false)
                        && !refused.iter().any(|event| {
                            event["type"] == "action_result" && event["handled"] == true
                        });
                    events.extend(refused);
                    ensure!(
                        refused_action,
                        "Owned no-offer did not refuse a stale acceptance action"
                    );
                }
                case["stale_receipt_replayed"] = json!(stale_offer_receipt.is_some());
                case["stale_readback"] = choice_readback(
                    &host.borrow(),
                    0,
                    input,
                    input.encode_utf16().count(),
                    edits_before,
                )?;
                let diagnostics = controller.diagnostics();
                ensure!(
                    lifecycle_accepted(controller) == accepted_before
                        && diagnostics
                            .traces
                            .iter()
                            .filter(|trace| trace.trace_id > baseline)
                            .all(no_publication_or_draw)
                        && receipts.is_empty()
                        && !events.iter().any(|event| event["type"] == "suggestion"),
                    "Owned no-offer gained publication/draw/acceptance after stale actions"
                );
                case["verified_no_offer"] = json!(true);
                return Ok(());
            }
            ensure!(
                terminal_trace.stage_us.iter().all(Option::is_some),
                "Owned offer is missing a publication/draw boundary"
            );
            let (offer, visible, stale_receipt) = model_drawn_offer(
                &events,
                &receipts,
                &controller.diagnostics(),
                terminal_trace.trace_id,
            )
            .context("Completed model request produced no actual displayed offer; no retry")?;
            stale_offer_receipt = Some(stale_receipt.clone());
            case["observed_outcome"] = json!("offer");
            case["offer"] = offer.clone();
            case["displayed_prefix"] = json!(visible);
            choice_owned_events()?;
            ensure!(
                host.borrow().focused() == Some(0),
                OwnedInterruption::EditorFocus
            );
            case["surface"] =
                capture_owned_ghost(app, output, &format!("model-{ordinal}-{language}-ghost"))?;
            case["renderer"] = overlay.smoke_ghost_diagnostic();
            case["host_surface"] = screenshot(
                app,
                "inklow.choice-host",
                output,
                &format!("model-{ordinal}-{language}-before"),
                false,
            )?;
            let before = host
                .borrow()
                .capture()?
                .context("Owned model field disappeared")?;
            let edits = host.borrow().edits;
            let accepted = lifecycle_accepted(controller);
            let mut expected = crate::core::SuggestionState {
                snapshot: Some(before.clone()),
                text: offer["text"]
                    .as_str()
                    .context("Missing generated text")?
                    .into(),
                ..Default::default()
            };
            let insertion = expected
                .accept(
                    &before,
                    full,
                    config.trailing_space,
                    crate::features::include_punctuation(&config.punctuation_mode, full),
                    Some(&visible),
                )
                .context("Displayed model prefix is not acceptably bounded")?;
            let predicted = expected
                .snapshot
                .as_ref()
                .context("Missing expected edit")?;
            case["expected_insertion"] = json!(insertion);
            case["unaccepted_remainder"] = json!(expected.text);
            let shortcut = host
                .borrow()
                .shortcut
                .clone()
                .context("Missing real shortcut callback")?;
            let action = if full { Action::All } else { Action::Word };
            ensure!(
                shortcut(action.clone()),
                "Displayed model prefix has no acceptance claim"
            );
            case["queued_repeat_claimed"] = json!(shortcut(action.clone()));
            let deadline = Instant::now() + ACCEPTANCE_LIMIT;
            loop {
                events.extend(choice_pump_receipts(
                    controller,
                    overlay,
                    Some(&mut receipts),
                )?);
                if host.borrow().edits > edits {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "Owned model acceptance did not edit"
                );
                std::thread::yield_now();
            }
            case["readback"] = choice_readback(
                &host.borrow(),
                0,
                &predicted.text,
                predicted.prefix().encode_utf16().count(),
                edits + 1,
            )?;
            ensure!(
                lifecycle_accepted(controller) == accepted + 1,
                "Repeated acceptance committed more than once"
            );
            // Dismiss any remaining generated suffix, then replay the retained
            // actual old acknowledgment. Neither it nor a repeated action may edit.
            controller.send(Command::Action(Action::Dismiss))?;
            events.extend(choice_pump_receipts(
                controller,
                overlay,
                Some(&mut receipts),
            )?);
            controller.send(stale_receipt)?;
            controller.send(Command::Action(action))?;
            let refused = choice_pump_receipts(controller, overlay, Some(&mut receipts))?;
            case["stale_refusal_events"] = json!(refused);
            ensure!(
                refused
                    .iter()
                    .any(|event| event["type"] == "action_result" && event["handled"] == false),
                "Dismissed model offer accepted a stale action"
            );
            events.extend(refused);
            case["stale_readback"] = choice_readback(
                &host.borrow(),
                0,
                &predicted.text,
                predicted.prefix().encode_utf16().count(),
                edits + 1,
            )?;
            ensure!(
                lifecycle_accepted(controller) == accepted + 1,
                "Stale acceptance counted twice"
            );
            choice_owned_events()?;
            ensure!(
                host.borrow().focused() == Some(0),
                OwnedInterruption::EditorFocus
            );
            case["edited_surface"] = screenshot(
                app,
                "inklow.choice-host",
                output,
                &format!("model-{ordinal}-{language}-accepted"),
                false,
            )?;
            case["positive_acceptance"] = json!(true);
            Ok(())
        })();
        // Preserve the assertion-time state before error capture can service a
        // later AppKit draw. A later surface never certifies the failed boundary.
        case["diagnostics_at_result"] = json!(controller.diagnostics());
        let mut error_capture = Ok(());
        if result.is_err() && owned_matrix_continue(&result).is_ok() {
            let capture = (|| -> Result<Value> {
                choice_owned_events()?;
                ensure!(
                    host.borrow().focused() == Some(0),
                    OwnedInterruption::EditorFocus
                );
                screenshot(
                    app,
                    "inklow.choice-host",
                    output,
                    &format!("model-{ordinal}-{language}-error"),
                    false,
                )
            })();
            case["error_surface"] = json!(capture.as_ref().ok());
            case["error_surface_unavailable"] =
                json!(capture.as_ref().err().map(|error| format!("{error:#}")));
            error_capture = capture.map(|_| ());
        }
        case["events"] = json!(events);
        case["diagnostics"] = json!(controller.diagnostics());
        let case_request_ids: std::collections::BTreeSet<_> = controller
            .diagnostics()
            .traces
            .iter()
            .filter(|trace| trace.trace_id > baseline)
            .filter_map(|trace| trace.request_id)
            .collect();
        case["generation_request_ids"] = json!(case_request_ids);
        case["worker_client_delivered_events"] = json!(*counts.borrow());
        case["native_edits_delta"] = json!(host.borrow().edits.checked_sub(edits_before));
        case["accepted_delta"] = json!(lifecycle_accepted(controller).checked_sub(accepted_before));
        case["widget_readback_at_exit"] = lifecycle_widgets(&host.borrow());
        case["passed"] = json!(result.is_ok() && case_request_ids.len() == 1);
        case["error"] = json!(result.as_ref().err().map(|error| format!("{error:#}")));
        case["request_count_error"] = json!(
            (case_request_ids.len() != 1)
                .then_some("Expected exactly one generation request for this fixed input")
        );
        if case["passed"] == true && case["positive_acceptance"] == true {
            positive_languages.insert(language);
        }
        evidence["cases"].as_array_mut().unwrap().push(case);
        // Do not re-pump a consumed physical event, dismiss, or seed another input.
        owned_matrix_continue(&result)?;
        owned_matrix_continue(&error_capture)?;
        if result.is_err() {
            // Cancel/dismiss failed work before the next authored input, never resample it.
            let reset = controller
                .send(Command::Action(Action::Dismiss))
                .and_then(|_| choice_pump_receipts(controller, overlay, None));
            let last = evidence["cases"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap();
            last["failure_cleanup_events"] = json!(reset.as_ref().ok());
            last["failure_cleanup_error"] =
                json!(reset.as_ref().err().map(|error| format!("{error:#}")));
            owned_matrix_continue(&reset)?;
        }
    }
    let request_ids: std::collections::BTreeSet<_> = controller
        .diagnostics()
        .traces
        .iter()
        .filter_map(|trace| trace.request_id)
        .collect();
    evidence["generation_request_ids"] = json!(request_ids);
    let cases = evidence["cases"].as_array().unwrap();
    let failed_cases: Vec<_> = cases
        .iter()
        .filter(|case| case["passed"] != true || !case["failure_cleanup_error"].is_null())
        .map(|case| case["request_ordinal"].clone())
        .collect();
    let missing_languages: Vec<_> = ["en", "pt"]
        .into_iter()
        .filter(|language| !positive_languages.contains(language))
        .collect();
    let summary = json!({
        "matrix_version":MATRIX_VERSION,"worker_pid":worker_pid,"fixed_inputs":INPUTS.len(),
        "generation_requests":request_ids.len(),"failed_case_ordinals":failed_cases,
        "positive_acceptance_languages":positive_languages,
        "missing_positive_acceptance_languages":missing_languages,
        "verified_no_offer_cases":cases.iter().filter(|case| case["verified_no_offer"] == true).count(),
        "word_and_full_displayed_prefix_acceptance":missing_languages.is_empty(),
        "stale_and_repeated_acceptance_refused":missing_languages.is_empty() && failed_cases.is_empty()
    });
    evidence["matrix_summary"] = summary.clone();
    ensure!(
        request_ids.len() == INPUTS.len() && counts.borrow().len() == INPUTS.len(),
        "Owned model generated other than one request per fixed input; see matrix_summary and case rows"
    );
    ensure!(
        failed_cases.is_empty() && missing_languages.is_empty(),
        "Owned model matrix failed: case errors at {failed_cases:?}; missing actual offered/drawn/accepted language coverage {missing_languages:?}; verified no-offers are not positive coverage"
    );
    Ok(summary)
}

fn capture_owned_acceptance(
    app: &AnyObject,
    kit: &Kit,
    output: &Path,
    model: Option<&Path>,
    evidence: &mut Value,
) -> Result<Value> {
    use crate::{
        platform::Action,
        runtime::{AsyncPreparer, Controller, Engine, MonotonicClock, SharedStorage},
    };
    use std::sync::{Arc, Mutex, atomic::AtomicU64};
    // Only an application-owned key window is retained; no external-app identity
    // or text is queried, and restoration below never reactivates an application.
    let previous_key: Option<Obj> = unsafe {
        let key: *mut AnyObject = msg_send![app, keyWindow];
        Retained::retain(key)
    };
    let owned_window = kit.window(
        "Owned choice acceptance",
        "inklow.choice-host",
        720.,
        320.,
        false,
    );
    let root = content(&owned_window)?;
    let editors = [
        kit.editor(
            "inklow.choice-host.first",
            "First owned field",
            rect(20., 170., 680., 110.),
            "",
        ),
        kit.editor(
            "inklow.choice-host.second",
            "Second owned field",
            rect(20., 30., 680., 110.),
            "",
        ),
    ];
    for editor in &editors {
        // AppKit must not send the synthetic draft to text substitution,
        // spelling or inline-prediction services outside this fixture.
        macro_rules! disable {
            ($setter:ident, $getter:ident) => {
                unsafe {
                    let supported: bool = msg_send![&**editor,respondsToSelector:sel!($setter:)];
                    if supported {
                        let _: () = msg_send![&**editor,$setter:false];
                        let enabled: bool = msg_send![&**editor,$getter];
                        ensure!(!enabled, "Owned text service could not be disabled: {}", stringify!($setter));
                    }
                }
            };
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
        add(&root, editor);
    }
    evidence["native_text_assistance_disabled"] = json!(true);
    let host = Rc::new(RefCell::new(ChoiceHost {
        window: owned_window,
        editors,
        revision: 1,
        edits: 0,
        captures: std::cell::Cell::new(0),
        enabled: false,
        started: false,
        shortcut: None,
        changed: None,
        model_clock: model.map(|_| Instant::now()),
        model_input_at: None,
        model_notifications: Vec::new(),
    }));
    let storage: SharedStorage = Arc::new(Mutex::new(Box::new(
        crate::storage::HistoryStore::with_key_loader(
            output.join("owned-choice-private.sqlite3"),
            Box::new(|| anyhow::bail!("Owned choice fixture must never unlock recording")),
        ),
    )));
    let epoch = Arc::new(AtomicU64::new(1));
    let preparer = AsyncPreparer::new(storage.clone(), epoch.clone(), Arc::new(ChoiceReaders))?;
    let mut fixture = if model.is_none() {
        Some(CompletionFixture::start()?)
    } else {
        None
    };
    let mut config = Config {
        backend: if model.is_some() { "gguf" } else { "openai" }.into(),
        model: if model.is_some() {
            ""
        } else {
            "owned-choice-fixture"
        }
        .into(),
        endpoint: fixture
            .as_ref()
            .map(|fixture| fixture.endpoint.clone())
            .unwrap_or_default(),
        model_id: String::new(),
        model_path: model
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        min_chars: if model.is_some() { 1 } else { 1000 },
        debounce_ms: if model.is_some() { 150 } else { 5000 },
        idle_release_seconds: 60,
        launch_at_login: false,
        use_history: false,
        personalization_strength: 0,
        custom_instructions: String::new(),
        battery_on_demand: false,
        battery_shorter: false,
        battery_model: "same".into(),
        synonyms_enabled: model.is_none(),
        synonyms_mode: "shortcut".into(),
        alternatives_mode: "shortcut".into(),
        emoji_gender: "female".into(),
        emoji_skin_tone: 3,
        emoji_include_neutral: true,
        global_enabled: true,
        enabled: true,
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
        alternatives_enabled: model.is_none(),
        trailing_space: false,
        ..Config::default()
    };
    if model.is_some() {
        // The real-model scenario exercises ordinary writing, not the controlled
        // server fixture's default continuation prompt.
        config.prompt_style = "writing".into();
        evidence["runtime_profile"] = json!({"max_tokens":config.max_tokens,
            "completion_length":config.completion_length,"prompt_style":config.prompt_style,
            "writing_language":config.writing_language,"debounce_ms":config.debounce_ms,
            "acceleration":config.acceleration,"semantic_quality_gate":false,
            "exact_config":config});
    }
    let model_path = model.map(|_| config.model_path.clone());
    let acceptance_config = config.clone();
    let bridge_host = host.clone();
    let worker_counts = Rc::new(RefCell::new(BTreeMap::new()));
    let worker_submissions = Rc::new(Cell::new(0));
    let mut controller = Controller::with_engine(Engine::with_native_factory(
        config,
        Box::new(ChoiceWorker {
            client: crate::process_client::WorkerClient::new()?,
            counts: worker_counts.clone(),
            submissions: worker_submissions.clone(),
        }),
        storage,
        Box::new(preparer),
        None,
        Box::new(ChoiceReaders),
        Box::new(MonotonicClock),
        epoch,
        // RetryNative and configuration operations can only reconstruct this retained host.
        Box::new(move |_| {
            Ok(Box::new(ChoiceBridge(
                bridge_host.clone(),
                model_path.clone(),
            )))
        }),
    )?)?;
    let mut overlay = OverlayOwner::new()?;
    overlay.set_paint_reporter(Some(controller.paint_reporter()));
    overlay.configure(false, "Tab");
    let result = if model.is_some() {
        capture_model_timeline(
            app,
            output,
            &controller,
            &mut overlay,
            &host,
            &worker_counts,
            &acceptance_config,
            evidence,
        )
    } else {
        (|| -> Result<Value> {
            let fixture = fixture
                .as_ref()
                .context("Missing controlled completion fixture")?;
            let mut cases = Vec::new();
            for (name, before, selection, later, expected, caret) in [
                (
                    "compact-middle",
                    "Draft ",
                    NSRange::new(6, 0),
                    "later",
                    "Draft later",
                    11,
                ),
                (
                    "exact-whitespace",
                    "Draft ",
                    NSRange::new(6, 0),
                    "  later  today",
                    "Draft   later  today",
                    20,
                ),
                (
                    "exact-nfd-emoji",
                    "Draft ",
                    NSRange::new(6, 0),
                    "  cafe\u{301}  😀",
                    "Draft   cafe\u{301}  😀",
                    17,
                ),
                (
                    "selected-replacement",
                    "Draft old end",
                    NSRange::new(6, 3),
                    "later",
                    "Draft later end",
                    11,
                ),
            ] {
                host.borrow_mut().seed(0, before, selection)?;
                choice_pump(&controller, &mut overlay)?;
                let baseline_edits = host.borrow().edits;
                let events = choice_offer(
                    &controller,
                    &mut overlay,
                    &fixture,
                    vec!["short".into(), "W".repeat(120), later.into()],
                    selection.length != 0,
                    None,
                )
                .with_context(|| format!("Owned choice scenario {name}"))?;
                let chooser = window(app, "inklow.chooser")?;
                let view = content(&chooser)?;
                for (ordinal, value) in [(1, "short"), (2, later)] {
                    let button = find(&view, &format!("inklow.suggestion.choice.{ordinal}"))?;
                    let title: *mut NSString = unsafe { msg_send![&*button, title] };
                    let tag: isize = unsafe { msg_send![&*button, tag] };
                    ensure!(
                        !title.is_null()
                            && unsafe { (&*title).to_string() } == format!("{ordinal}   {value}")
                            && tag == ordinal
                            && is_enabled(&button),
                        "Owned compact row changed bytes or ordinal"
                    );
                }
                ensure!(
                    find(&view, "inklow.suggestion.choice.3").is_err(),
                    "Oversized middle row survived"
                );
                ensure!(
                    overlay
                        .choice_targets()
                        .is_some_and(|targets| targets.button_count == 2),
                    "Owned compact hit targets mismatch"
                );
                let image = capture_choice_bitmap(app, output, &format!("accept-{name}-chooser"))?;
                let button = find(&view, "inklow.suggestion.choice.2")?;
                unsafe {
                    let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
                }
                let actions = overlay.tick()?;
                ensure!(
                    matches!(actions.as_slice(), [Command::Action(Action::Choice(1))]),
                    "Actual NSButton did not emit displayed ordinal two"
                );
                choice_commands(&controller, actions)?;
                let accepted = choice_pump(&controller, &mut overlay)?;
                ensure!(
                    accepted
                        .iter()
                        .any(|event| event["type"] == "action_result" && event["handled"] == true),
                    "Runtime did not accept the displayed row"
                );
                let readback =
                    choice_readback(&host.borrow(), 0, expected, caret, baseline_edits + 1)?;
                // A repeated target action/old ordinal cannot edit a second time.
                controller.send(Command::Action(Action::Choice(1)))?;
                choice_pump(&controller, &mut overlay)?;
                choice_readback(&host.borrow(), 0, expected, caret, baseline_edits + 1)?;
                let host_image = screenshot(
                    app,
                    "inklow.choice-host",
                    output,
                    &format!("accept-{name}-host"),
                    false,
                )?;
                cases.push(json!({"case":name,"offered_original_indices":[0,1,2],"painted_original_indices":[0,2],
                "clicked_displayed_ordinal":2,"readback":readback,"chooser":image,"host":host_image,
                "offer_events":events,"accepted_events":accepted,"duplicate_edit_refused":true}));
            }
            // Local emoji tools never submit model work. Real chooser draw and
            // typed acknowledgment must precede bitmap capture and native clicks.
            for (name, before, initial_caret, glyph, expected, caret, ordinal, choices) in [
                (
                    "emoji-zwj",
                    "Draft :technologist",
                    19,
                    "👩‍💻",
                    "Draft 👩‍💻",
                    11,
                    2,
                    &["👩🏽‍💻", "👩‍💻"][..],
                ),
                (
                    "emoji-alias",
                    "Olá  :sorriso",
                    13,
                    "😄",
                    "Olá  😄",
                    7,
                    1,
                    &["😄"][..],
                ),
                (
                    "emoji-emoticon",
                    "Olá  <3",
                    7,
                    "❤️",
                    "Olá  ❤️",
                    7,
                    1,
                    &["❤️"][..],
                ),
            ] {
                let submissions = worker_submissions.get();
                let edits = host.borrow().edits;
                host.borrow_mut()
                    .seed(0, before, NSRange::new(initial_caret, 0))?;
                let mut receipts = Vec::new();
                let mut events = choice_wait(
                    &controller,
                    &mut overlay,
                    &fixture,
                    "emoji",
                    Some(&mut receipts),
                )?;
                let offer = events
                    .iter()
                    .rev()
                    .find(|event| event["type"] == "suggestion" && event["mode"] == "emoji")
                    .context("Local emoji tool did not publish")?
                    .clone();
                let offered_choices = offer["choices"]
                    .as_array()
                    .context("Missing local emoji rows")?;
                ensure!(
                    offered_choices.len() >= choices.len()
                        && offered_choices
                            .iter()
                            .zip(choices)
                            .all(|(actual, expected)| actual.as_str() == Some(*expected))
                        && (ordinal != 1 || offered_choices.len() == 1),
                    "{name}: local emoji ordering/singleton count changed"
                );
                let drawn = lifecycle_barrier_receipts(
                    &controller, &mut overlay, &fixture, &mut events, Some(&mut receipts),
                    |events, diagnostics| {
                        events.iter().rev()
                            .find(|event| event["type"] == "suggestion")
                            .is_some_and(|current| current["mode"] == "emoji"
                                && current["key"] == offer["key"]
                                && current["authority"] == offer["authority"]
                                && current["revision"] == offer["revision"]
                                && current["diagnostic"] == offer["diagnostic"])
                            && events.iter().any(|ack| ack["type"] == "owned_presentation_ack"
                                && ack["mode"] == "emoji" && ack["key"] == offer["key"]
                                && ack["authority"] == offer["authority"]
                                && ack["revision"] == offer["revision"])
                            && diagnostics.traces.iter().any(|trace| {
                                Some(trace.trace_id) == offer["diagnostic"]["trace_id"].as_u64()
                                    && Some(trace.view_serial) == offer["diagnostic"]["view_serial"].as_u64()
                                    && trace.lane == crate::diagnostics::PresentationLane::Chooser
                                    && matches!(
                                        (
                                            trace.stage_us[crate::diagnostics::Stage::ProviderPublication as usize],
                                            trace.stage_us[crate::diagnostics::Stage::NativeDraw as usize],
                                        ),
                                        (Some(published), Some(drawn)) if drawn >= published
                                    )
                            })
                    },
                ).with_context(|| format!("{name}: current native chooser draw"))?;
                let receipt = receipts
                    .iter()
                    .rev()
                    .find_map(|receipt| match receipt {
                        Command::PresentationResult {
                            key,
                            authority,
                            revision,
                            visible_text,
                            mode,
                        } if mode == "emoji"
                            && json!(key) == offer["key"]
                            && json!(authority) == offer["authority"]
                            && Some(*revision) == offer["revision"].as_u64() =>
                        {
                            Some(json!({"key":key,"authority":authority,"revision":revision,
                            "visible_text":visible_text,"mode":mode}))
                        }
                        _ => None,
                    })
                    .context("Current emoji chooser lacks actual typed presentation receipt")?;
                let chooser = window(app, "inklow.chooser")?;
                let view = content(&chooser)?;
                let button = find(&view, &format!("inklow.suggestion.choice.{ordinal}"))?;
                let title: *mut NSString = unsafe { msg_send![&*button, title] };
                let tag: isize = unsafe { msg_send![&*button, tag] };
                ensure!(
                    !title.is_null()
                        && unsafe { (&*title).to_string() } == format!("{ordinal}   {glyph}")
                        && tag == ordinal as isize
                        && is_enabled(&button),
                    "{name}: native emoji row changed bytes/ordinal"
                );
                ensure!(
                    overlay
                        .choice_targets()
                        .is_some_and(|targets| targets.button_count == offered_choices.len()),
                    "{name}: native emoji row count differs"
                );
                let image = capture_choice_bitmap(app, output, &format!("accept-{name}-chooser"))?;
                unsafe {
                    let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
                }
                let actions = overlay.tick()?;
                ensure!(
                    matches!(actions.as_slice(), [Command::Action(Action::Choice(index))]
                    if *index == ordinal - 1),
                    "{name}: native button emitted wrong choice action"
                );
                choice_commands(&controller, actions)?;
                let accepted = choice_pump(&controller, &mut overlay)?;
                ensure!(
                    accepted
                        .iter()
                        .any(|event| event["type"] == "action_result" && event["handled"] == true),
                    "{name}: runtime did not accept current native emoji row"
                );
                let readback = choice_readback(&host.borrow(), 0, expected, caret, edits + 1)?;
                controller.send(Command::Action(Action::Choice(ordinal - 1)))?;
                choice_pump(&controller, &mut overlay)?;
                choice_readback(&host.borrow(), 0, expected, caret, edits + 1)?;
                ensure!(
                    worker_submissions.get() == submissions,
                    "{name}: local emoji handling submitted provider work"
                );
                cases.push(json!({"case":name,"source":"local emoji tool, no provider output",
                    "chooser":image,"readback":readback,"offer_events":events,"accepted_events":accepted,
                    "clicked_displayed_ordinal":ordinal,"emitted_choice_index":ordinal-1,
                    "current_typed_presentation_receipt":receipt,"draw_diagnostics":drawn,
                    "draw_gate":"current chooser trace/view; NativeDraw >= ProviderPublication before bitmap/click",
                    "provider_submission_delta":worker_submissions.get()-submissions,
                    "duplicate_edit_refused":true}));
            }
            let mut refusals = Vec::new();
            for case in [
                "clipped",
                "hidden",
                "stale-ack",
                "stale-view",
                "second-field",
            ] {
                host.borrow_mut().seed(0, "Draft ", NSRange::new(6, 0))?;
                choice_pump(&controller, &mut overlay)?;
                let edits = host.borrow().edits;
                let offered = if case == "clipped" {
                    vec!["W".repeat(120), "W".repeat(121), "W".repeat(122)]
                } else {
                    vec!["short".into(), "W".repeat(120), "later".into()]
                };
                if case == "stale-view" {
                    choice_completion(&controller, &mut overlay, &fixture)?;
                }
                let mut receipts = Vec::new();
                let events = choice_offer(
                    &controller,
                    &mut overlay,
                    &fixture,
                    offered,
                    false,
                    Some(&mut receipts),
                )?;
                let event = events
                    .iter()
                    .find(|event| {
                        event["type"] == "suggestion"
                            && (event["mode"] == "alternatives"
                                || event["choices_mode"] == "alternatives")
                    })
                    .context("Owned refusal fixture has no offered view")?
                    .clone();
                let old_ack = receipts
                .into_iter()
                .find(|command| matches!(command,
                    Command::PresentationResult { key, authority, revision, mode, .. }
                        if mode == "alternatives"
                            && Some(*revision) == event["revision"].as_u64()
                            && json!(key) == event["key"]
                            && json!(authority) == event["authority"]))
                .with_context(|| format!("{case}: owned refusal fixture has no actual native presentation receipt"))?;
                if case == "hidden" {
                    let chooser = window(app, "inklow.chooser")?;
                    let view = content(&chooser)?;
                    let button = find(&view, "inklow.suggestion.choice.2")?;
                    unsafe {
                        let _: () = msg_send![&*chooser,orderOut:std::ptr::null::<AnyObject>()];
                    }
                    let revoked = overlay.tick()?;
                    ensure!(
                        revoked.iter().any(|command| matches!(command,
                    Command::PresentationResult { visible_text, mode, .. }
                        if mode == "alternatives" && visible_text.is_empty())),
                        "Hidden native chooser did not revoke its presentation"
                    );
                    choice_commands(&controller, revoked)?;
                    choice_pump(&controller, &mut overlay)?;
                    unsafe {
                        let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
                    }
                    ensure!(
                        overlay.tick()?.is_empty(),
                        "Hidden native button still emitted an action"
                    );
                } else if case == "stale-view" {
                    // The new offer is already acknowledged by choice_offer. An old
                    // acknowledgment must neither replace its exact row mapping nor
                    // revoke it: this is current acceptance, not a refusal case.
                    let chooser = window(app, "inklow.chooser")?;
                    let old_view = content(&chooser)?;
                    let old_button = find(&old_view, "inklow.suggestion.choice.2")?;
                    let old_image = capture_choice_bitmap(app, output, "accept-stale-view-old")?;
                    ensure!(
                        matches!(&old_ack,
                        Command::PresentationResult { visible_text, .. }
                            if visible_text == "short\nlater"),
                        "stale-view: old acknowledgment did not preserve its exact rows"
                    );
                    let revision = host.borrow().revision;
                    let replacement_completion =
                        choice_completion(&controller, &mut overlay, &fixture)?;
                    let newer = choice_offer(
                        &controller,
                        &mut overlay,
                        &fixture,
                        vec!["current".into(), "W".repeat(120), "  newer  today".into()],
                        false,
                        None,
                    )?;
                    let current = newer
                        .iter()
                        .find(|new_event| {
                            new_event["type"] == "suggestion"
                                && (new_event["mode"] == "alternatives"
                                    || new_event["choices_mode"] == "alternatives")
                        })
                        .context("stale-view: newer choice offer missing")?;
                    ensure!(
                        host.borrow().revision == revision
                            && current["revision"] != event["revision"]
                            && current["key"] == event["key"]
                            && current["authority"] == event["authority"],
                        "stale-view: case must change only presentation, not field authority"
                    );
                    ensure!(
                        newer
                            .iter()
                            .any(|ack| ack["type"] == "owned_presentation_ack"
                                && ack["revision"] == current["revision"]
                                && ack["mode"] == "alternatives"
                                && ack["visible_text"] == "current\n  newer  today"),
                        "stale-view: newer exact rows were not acknowledged"
                    );
                    let mut old_negative_ack = old_ack.clone();
                    if let Command::PresentationResult { visible_text, .. } = &mut old_negative_ack
                    {
                        visible_text.clear();
                    }
                    controller.send(old_ack)?;
                    let replayed = choice_pump(&controller, &mut overlay)?;
                    controller.send(old_negative_ack)?;
                    let negative_replayed = choice_pump(&controller, &mut overlay)?;
                    let current_view = content(&chooser)?;
                    for (ordinal, value) in [(1, "current"), (2, "  newer  today")] {
                        let button = find(
                            &current_view,
                            &format!("inklow.suggestion.choice.{ordinal}"),
                        )?;
                        let title: *mut NSString = unsafe { msg_send![&*button, title] };
                        let tag: isize = unsafe { msg_send![&*button, tag] };
                        ensure!(
                            !title.is_null()
                                && unsafe { (&*title).to_string() }
                                    == format!("{ordinal}   {value}")
                                && tag == ordinal
                                && is_enabled(&button),
                            "stale-view: old acknowledgment changed the current displayed row"
                        );
                    }
                    ensure!(
                        find(&current_view, "inklow.suggestion.choice.3").is_err()
                            && overlay
                                .choice_targets()
                                .is_some_and(|targets| targets.button_count == 2),
                        "stale-view: current compact hit mapping was lost"
                    );
                    let button = find(&current_view, "inklow.suggestion.choice.2")?;
                    ensure!(
                        Retained::as_ptr(&old_button) != Retained::as_ptr(&button),
                        "stale-view: native row was not replaced"
                    );
                    let image = capture_choice_bitmap(app, output, "accept-stale-view-current")?;
                    // Probe the retained stale control while the current offer is
                    // still live; probing only after acceptance would miss an old
                    // button borrowing the new view's enabled ordinal.
                    unsafe {
                        let _: () =
                            msg_send![&*old_button,performClick:std::ptr::null::<AnyObject>()];
                    }
                    ensure!(
                        overlay.tick()?.is_empty(),
                        "stale-view: replaced native button emitted an action for the current offer"
                    );
                    let stale_events = choice_pump(&controller, &mut overlay)?;
                    let unchanged = choice_readback(&host.borrow(), 0, "Draft ", 6, edits)?;
                    unsafe {
                        let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
                        // Repeated native dispatch before the queue drains must not
                        // turn this one displayed publication into two actions.
                        let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
                    }
                    let actions = overlay.tick()?;
                    ensure!(
                        matches!(actions.as_slice(), [Command::Action(Action::Choice(1))]),
                        "stale-view: current native row did not emit displayed ordinal two exactly once"
                    );
                    choice_commands(&controller, actions)?;
                    let accepted = choice_pump(&controller, &mut overlay)?;
                    ensure!(
                        accepted
                            .iter()
                            .any(|event| event["type"] == "action_result"
                                && event["handled"] == true),
                        "stale-view: old acknowledgment destroyed current acceptance"
                    );
                    let readback =
                        choice_readback(&host.borrow(), 0, "Draft   newer  today", 20, edits + 1)?;
                    for retained in [&old_button, &button] {
                        unsafe {
                            let _: () =
                                msg_send![&**retained,performClick:std::ptr::null::<AnyObject>()];
                        }
                    }
                    ensure!(
                        overlay.tick()?.is_empty(),
                        "stale-view: consumed native controls emitted another action"
                    );
                    controller.send(Command::Action(Action::Choice(1)))?;
                    let repeated = choice_pump(&controller, &mut overlay)?;
                    ensure!(
                        repeated
                            .iter()
                            .any(|event| event["type"] == "action_result"
                                && event["handled"] == false),
                        "stale-view: consumed current ordinal remained actionable"
                    );
                    choice_readback(&host.borrow(), 0, "Draft   newer  today", 20, edits + 1)?;
                    let host_image = screenshot(
                        app,
                        "inklow.choice-host",
                        output,
                        "accept-stale-view-host",
                        false,
                    )?;
                    cases.push(json!({
                        "case":case,"old_offer_events":events,
                        "replacement_completion_events":replacement_completion,
                        "current_offer_events":newer,"old_ack_replay_events":replayed,
                        "old_negative_ack_replay_events":negative_replayed,
                        "stale_control_events":stale_events,"unchanged_before_acceptance":unchanged,
                        "old_chooser":old_image,"chooser":image,"host":host_image,
                        "painted_original_indices":[0,2],"clicked_displayed_ordinal":2,
                        "readback":readback,"accepted_events":accepted,"repeated_events":repeated,
                        "stale_control_inactive_while_current":true,
                        "old_ack_preserved_current_mapping":true,"native_edit_delta":1,
                        "old_negative_ack_preserved_current_mapping":true,
                        "duplicate_edit_refused":true
                    }));
                    continue;
                } else if case == "stale-ack" || case == "second-field" {
                    // Same process, same draft and caret: revision/field authority,
                    // not text equality or app identity, must reject the old ack.
                    ensure!(
                        matches!(&old_ack,
                        Command::PresentationResult { visible_text, .. }
                            if visible_text == "short\nlater"),
                        "{case}: stale replay did not retain the actual exact native rows"
                    );
                    host.borrow_mut().seed(
                        usize::from(case == "second-field"),
                        "Draft ",
                        NSRange::new(6, 0),
                    )?;
                    choice_pump(&controller, &mut overlay)?;
                    controller.send(old_ack)?;
                    choice_pump(&controller, &mut overlay)?;
                } else {
                    ensure!(
                        overlay.choice_targets().is_none()
                            && events
                                .iter()
                                .any(|event| event["type"] == "owned_presentation_ack"
                                    && event["mode"] == "alternatives"
                                    && event["visible_text"] == ""),
                        "Fully clipped chooser acknowledged an actionable row"
                    );
                }
                let shortcut = host
                    .borrow()
                    .shortcut
                    .clone()
                    .context("Owned shortcut callback missing")?;
                ensure!(
                    !shortcut(Action::Choice(1)),
                    "{case}: unexposed/stale chooser armed a numbered claim"
                );
                controller.send(Command::Action(Action::Choice(1)))?;
                let actions = choice_pump(&controller, &mut overlay)?;
                ensure!(
                    actions
                        .iter()
                        .any(|event| event["type"] == "action_result" && event["handled"] == false),
                    "{case}: runtime accepted an unexposed/stale chooser"
                );
                let first = choice_readback(&host.borrow(), 0, "Draft ", 6, edits)?;
                let second = if case == "second-field" {
                    Some(choice_readback(&host.borrow(), 1, "Draft ", 6, edits)?)
                } else {
                    None
                };
                refusals.push(
                    json!({"case":case,"passed":true,"first_field":first,"second_field":second,
                "offered_and_acknowledged":events,"action_results":actions}),
                );
            }

            // The ordinary ghost and numbered alternatives share a native anchor,
            // not an acceptance owner. Repaint the identical completion, then use
            // the actual runtime hook callback (not global physical keyboard input).
            let mut primary = Vec::new();
            for use_tab in [true, false] {
                host.borrow_mut().seed(0, "Draft ", NSRange::new(6, 0))?;
                choice_pump(&controller, &mut overlay)?;
                let edits = host.borrow().edits;
                let mut events = choice_completion(&controller, &mut overlay, &fixture)?;
                events.extend(choice_offer(
                    &controller,
                    &mut overlay,
                    &fixture,
                    vec!["short".into(), "W".repeat(120), "later".into()],
                    false,
                    None,
                )?);
                let event = events
                    .iter()
                    .find(|event| {
                        event["type"] == "suggestion"
                            && event["mode"] == "completion"
                            && event["choices_mode"] == "alternatives"
                    })
                    .context("Owned ordinary completion was not published")?;
                choice_commands(&controller, overlay.handle(event)?)?;
                choice_pump(&controller, &mut overlay)?;
                let name = if use_tab {
                    "tab-primary"
                } else {
                    "numbered-separate"
                };
                let ghost = capture_owned_ghost(app, output, &format!("accept-{name}-ghost"))?;
                let chooser =
                    capture_choice_bitmap(app, output, &format!("accept-{name}-chooser"))?;
                if use_tab {
                    let shortcut = host
                        .borrow()
                        .shortcut
                        .clone()
                        .context("Owned runtime shortcut callback missing")?;
                    ensure!(
                        shortcut(Action::Word),
                        "Ordinary completion lost Tab ownership"
                    );
                } else {
                    let chooser_window = window(app, "inklow.chooser")?;
                    let view = content(&chooser_window)?;
                    let button = find(&view, "inklow.suggestion.choice.2")?;
                    unsafe {
                        let _: () = msg_send![&*button,performClick:std::ptr::null::<AnyObject>()];
                    }
                    let actions = overlay.tick()?;
                    ensure!(
                        matches!(actions.as_slice(), [Command::Action(Action::Choice(1))]),
                        "Anchored alternative lost displayed ordinal"
                    );
                    choice_commands(&controller, actions)?;
                }
                choice_pump(&controller, &mut overlay)?;
                let (expected, caret) = if use_tab {
                    ("Draft tomorrow", 14)
                } else {
                    ("Draft later", 11)
                };
                let readback = choice_readback(&host.borrow(), 0, expected, caret, edits + 1)?;
                primary.push(
                    json!({"case":name,"readback":readback,"ghost":ghost,"chooser":chooser,
                "offer_events":events,"physical_keyboard_exercised":false}),
                );
            }
            ensure!(
                controller.status()["worker_pid"].as_u64().is_some(),
                "Owned fixture did not use the real WorkerClient subprocess"
            );
            let wake =
                capture_owned_wake(&controller, &mut overlay, &fixture, &host, &worker_counts)?;
            let lifecycle = capture_owned_lifecycle(
                app,
                output,
                &controller,
                &mut overlay,
                &fixture,
                &host,
                &worker_counts,
            )?;
            Ok(
                json!({"cases":cases,"refusals":refusals,"completion_ownership":primary,"lifecycle":lifecycle,"native_wake":wake}),
            )
        })()
    };
    evidence["diagnostics_before_shutdown"] = json!(controller.diagnostics());
    evidence["worker_status_before_shutdown"] = controller.status();
    let close = overlay.close();
    let deadline = Instant::now() + Duration::from_secs(3);
    let stopped = (|| -> Result<()> {
        while !controller.shutdown()? {
            ensure!(
                Instant::now() < deadline,
                "Owned fixture shutdown timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    })();
    let served = fixture.as_mut().map(CompletionFixture::finish).transpose();
    if owned_matrix_continue(&result).is_ok() && host.borrow().focused().is_some() {
        if let Some(previous) = previous_key {
            unsafe {
                let active: bool = msg_send![app, isActive];
                let visible: bool = msg_send![&*previous, isVisible];
                if active && visible {
                    let _: () = msg_send![&*previous, makeKeyWindow];
                }
            }
        }
    }
    evidence["worker_stopped"] = json!(stopped.is_ok());
    evidence["worker_status_after_shutdown"] = controller.status();
    evidence["shutdown_error"] = json!(stopped.as_ref().err().map(|error| format!("{error:#}")));
    evidence["overlay_close_error"] = json!(close.as_ref().err().map(|error| format!("{error:#}")));
    evidence["diagnostics_after_shutdown"] = json!(controller.diagnostics());
    evidence["worker_client_delivered_events"] = json!(*worker_counts.borrow());
    evidence["input_notifications"] = json!(host.borrow().model_notifications);
    evidence["final_widgets"] = lifecycle_widgets(&host.borrow());
    close?;
    stopped?;
    let mut receipt = result?;
    receipt["fixture_completion_requests"] = json!(served?);
    receipt["scope"] = json!(if model.is_some() {
        "Explicit existing local GGUF through one WorkerClient, Controller, actual OverlayOwner draw and owned NSTextView edits; not full64 semantic quality, physical keyboard, compositor or external AX-host proof."
    } else {
        "Controlled model-output fixture, not real-model quality: real loopback provider parsing, WorkerClient ready/proceed, actual OverlayOwner/NSButton, Controller/Engine claim mapping, owned NativeBridge NSTextView edit and independent widget/caret readback. Not global physical keyboard, compositor pixels, or arbitrary AX-host parity."
    });
    receipt["recording_and_context_disabled"] = json!(true);
    receipt["passed"] = json!(true);
    Ok(receipt)
}

fn capture_owned_ghost(app: &AnyObject, output: &Path, name: &str) -> Result<Value> {
    let panel = window(app, "inklow.suggestion")?;
    let view = content(&panel)?;
    unsafe {
        let bounds: NSRect = msg_send![&*view, bounds];
        let bitmap: *mut AnyObject = msg_send![&*view,bitmapImageRepForCachingDisplayInRect:bounds];
        ensure!(!bitmap.is_null(), "Owned ghost has no bitmap");
        let _: () = msg_send![&*view,cacheDisplayInRect:bounds,toBitmapImageRep:bitmap];
        let width: isize = msg_send![bitmap, pixelsWide];
        let height: isize = msg_send![bitmap, pixelsHigh];
        ensure!(
            (1..=4096).contains(&width) && (1..=1024).contains(&height),
            "Owned ghost bitmap dimensions are invalid"
        );
        let mut ink_pixels = 0usize;
        for y in 0..height {
            for x in 0..width {
                let color: *mut AnyObject = msg_send![bitmap,colorAtX:x,y:y];
                ensure!(!color.is_null(), "Owned ghost bitmap has no pixel");
                let alpha: f64 = msg_send![color, alphaComponent];
                if alpha > 0.05 {
                    ink_pixels += 1;
                }
            }
        }
        ensure!(
            ink_pixels >= 20,
            "Visible owned ghost has no rasterized glyph ink"
        );
        let properties = obj("NSDictionary");
        let data: *mut AnyObject =
            msg_send![bitmap,representationUsingType:4usize,properties:&*properties];
        ensure!(!data.is_null(), "Owned ghost PNG encoding failed");
        let length: usize = msg_send![data, length];
        let bytes: *const std::ffi::c_void = msg_send![data, bytes];
        ensure!(
            !bytes.is_null() && (33..=1024 * 1024).contains(&length),
            "Owned ghost PNG has invalid bytes"
        );
        let bytes = std::slice::from_raw_parts(bytes.cast::<u8>(), length);
        let filename = format!("{name}.png");
        write_new(&output.join(&filename), bytes)?;
        let frame: NSRect = msg_send![&*panel, frame];
        Ok(json!({
            "file":filename,"sha256":format!("{:x}",Sha256::digest(bytes)),
            "pixel_width":width,"pixel_height":height,"ink_pixels":ink_pixels,
            "actual_frame":layout_rect(frame),
            "capture":"actual OverlayOwner GhostSurface NSView bitmap; no preview renderer, screen capture or host writing"
        }))
    }
}

fn capture_inline_ghosts(app: &AnyObject, output: &Path) -> Result<Vec<Value>> {
    use crate::core::{InlineRender, Snapshot};
    use objc2_app_kit::{NSFont, NSScreen};
    let mtm = MainThreadMarker::new().context("Owned ghost smoke requires AppKit")?;
    let screens = NSScreen::screens(mtm);
    let screen = screens
        .firstObject()
        .context("Owned ghost smoke requires a screen")?;
    let frame = screen.frame();
    let visible = screen.visibleFrame();
    ensure!(
        visible.size.width >= 800. && visible.size.height >= 400.,
        "Owned ghost smoke requires an 800x400-point visible screen"
    );
    let top = frame.origin.y + frame.size.height;
    let base_x = visible.origin.x + 100.;
    let base_y = top - visible.origin.y - visible.size.height + 100.;
    let key_before: *mut AnyObject = unsafe { msg_send![app, keyWindow] };
    let mut overlay = OverlayOwner::new()?;
    overlay.configure(false, "Tab");
    let mut observations = Vec::new();
    let fonts = [
        (
            "system-13",
            NSFont::systemFontOfSize_weight(13., 0.),
            true,
            0.,
        ),
        (
            "system-15",
            NSFont::systemFontOfSize_weight(15., 0.),
            true,
            0.,
        ),
        (
            "system-medium",
            NSFont::systemFontOfSize_weight(17., 0.23),
            true,
            0.23,
        ),
        (
            "named-helvetica",
            NSFont::fontWithName_size(&NSString::from_str("Helvetica"), 15.)
                .context("Owned ghost named font is unavailable")?,
            false,
            0.,
        ),
    ];
    for (font_case, font, system_font, system_weight) in fonts {
        for (index, (fraction_x, fraction_y)) in [
            (0., 0.),
            (0.125, 0.875),
            (0.25, 0.75),
            (0.5, 0.5),
            (0.75, 0.25),
            (0.875, 0.125),
            (0., 0.5),
            (0.5, 0.),
        ]
        .into_iter()
        .enumerate()
        {
            let ascent = font.ascender();
            let descent = -font.descender();
            let caret = [
                base_x + fraction_x,
                base_y + fraction_y,
                1.,
                ascent + descent,
            ];
            let render = InlineRender {
                font_name: font.fontName().to_string(),
                font_size: font.pointSize(),
                baseline: caret[1] + ascent,
                clip_rect: [base_x - 20., base_y - 4., 600., ascent + descent + 10.],
                ascent,
                descent,
                system_font,
                system_weight,
                source: if system_font {
                    "macos-ax-caret-row"
                } else {
                    "macos-ax-attributed"
                }
                .into(),
                ..InlineRender::default()
            };
            let snapshot = Snapshot {
                app_id: "net.whatsapp.WhatsApp".into(),
                field_id: "owned-inline-editor".into(),
                caret_rect: Some(caret),
                inline_render: Some(render.clone()),
                native_inline: true,
                ..Snapshot::default()
            };
            ensure!(
                snapshot.inline_safe(),
                "Synthetic owned inline geometry is not ready"
            );
            let text = " synthetic owned completion";
            let event = json!({
                "type":"suggestion","mode":"completion","inline":true,"text":text,
                "advance":"","rect":caret,"render":render,
                "key":snapshot.key(),"authority":snapshot.authority(),"revision":1
            });
            let acknowledgments = overlay.handle(&event)?;
            ensure!(
                acknowledgments.iter().any(|command| matches!(
                    command,
                    Command::PresentationResult { mode, visible_text, .. }
                        if mode == "completion" && visible_text == text
                )),
                "Owned inline-ready ghost was rejected: font={font_case}, caret={caret:?}, diagnostic={}",
                overlay.smoke_ghost_diagnostic()
            );
            let mut evidence =
                capture_owned_ghost(app, output, &format!("ghost-{font_case}-{index}"))?;
            let diagnostic = overlay.smoke_ghost_diagnostic();
            let actual: [f64; 4] =
                serde_json::from_value(diagnostic["geometry"]["actual_frame"].clone())?;
            let offset: [f64; 2] =
                serde_json::from_value(diagnostic["geometry"]["drawing_origin"].clone())?;
            ensure!(
                (actual[0] + offset[0] - caret[0]).abs() <= 0.000_001
                    && (top - actual[1] - offset[1] - descent - render.baseline).abs() <= 0.000_001,
                "Outward frame rounding moved the host glyph origin or baseline"
            );
            evidence["exact_glyph_origin_and_baseline_preserved"] = json!(true);
            evidence["renderer_geometry"] = diagnostic["geometry"].clone();
            evidence["font_case"] = json!(font_case);
            evidence["font_name"] = json!(font.fontName().to_string());
            evidence["caret"] = json!(caret);
            evidence["inline_ready"] = json!(true);
            evidence["acknowledged"] = json!(true);
            let key_after: *mut AnyObject = unsafe { msg_send![app, keyWindow] };
            ensure!(
                key_before == key_after,
                "Owned inline ghost stole the key window"
            );
            evidence["key_window_preserved"] = json!(true);
            observations.push(evidence);
            overlay.handle(&json!({"type":"clear","hide_accessory":true}))?;

            // A valid inline row is not a promise that a whole word fits. Keep
            // this distinct from font/quantization failures and never widen it.
            let mut narrow = event;
            narrow["render"]["clip_rect"] =
                json!([caret[0], base_y - 4., 1., ascent + descent + 10.]);
            let narrow_render: InlineRender = serde_json::from_value(narrow["render"].clone())?;
            ensure!(
                narrow_render.valid(Some(caret)),
                "Narrow inline fixture is not ready"
            );
            let mut narrow_snapshot = snapshot.clone();
            narrow_snapshot.inline_render = Some(narrow_render);
            ensure!(
                narrow_snapshot.inline_safe(),
                "Narrow snapshot lost inline readiness"
            );
            let refused = overlay.handle(&narrow)?;
            ensure!(
                refused.iter().any(|command| matches!(
                    command, Command::PresentationResult { mode, visible_text, .. }
                        if mode == "completion" && visible_text.is_empty()
                )),
                "Narrow inline row incorrectly acknowledged invisible writing"
            );
            let panel = window(app, "inklow.suggestion")?;
            let shown: bool = unsafe { msg_send![&*panel, isVisible] };
            ensure!(!shown, "Refused inline ghost remained visible");
            overlay.handle(&json!({"type":"clear","hide_accessory":true}))?;
        }
    }
    overlay.close()?;
    Ok(observations)
}

fn known_typing_observer(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("brotypist") || name.contains("cotypist") || name.contains("inklow")
}

/// Metadata-only preflight, before displaying or seeding any native field.
/// CI alone is not proof that a desktop is disposable, so it cannot bypass this
/// check. No observer is stopped, reconfigured, or queried for profile contents.
pub(super) fn refuse_typing_observers(owned_peer_pid: Option<i32>) -> Result<()> {
    let own_pid = std::process::id() as i32;
    let mut observers = BTreeMap::new();
    unsafe {
        let workspace: *mut AnyObject = msg_send![
            AnyClass::get(c"NSWorkspace").context("Workspace metadata unavailable")?,
            sharedWorkspace
        ];
        ensure!(
            !workspace.is_null(),
            "Cannot inspect running application metadata safely"
        );
        let applications: *mut AnyObject = msg_send![workspace, runningApplications];
        ensure!(
            !applications.is_null(),
            "Running application metadata unavailable"
        );
        let count: usize = msg_send![applications, count];
        ensure!(
            count <= 4096,
            "Running application metadata exceeded preflight bounds"
        );
        for index in 0..count {
            let application: *mut AnyObject = msg_send![applications,objectAtIndex:index];
            ensure!(
                !application.is_null(),
                "Running application metadata disappeared"
            );
            let pid: i32 = msg_send![application, processIdentifier];
            if pid == own_pid || Some(pid) == owned_peer_pid {
                continue;
            }
            let bundle: *mut NSString = msg_send![application, bundleIdentifier];
            let name: *mut NSString = msg_send![application, localizedName];
            let bundle = if bundle.is_null() {
                String::new()
            } else {
                (&*bundle).to_string()
            };
            let name = if name.is_null() {
                String::new()
            } else {
                (&*name).to_string()
            };
            if known_typing_observer(&bundle) || known_typing_observer(&name) {
                observers.insert(pid, format!("{name} ({bundle})"));
            }
        }
    }
    // Include headless/source launches which need not register with NSWorkspace.
    // `comm`, unlike `args`, contains executable metadata, never typed prompts.
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,comm="])
        .output()
        .context("Cannot inspect executable metadata for typing observers")?;
    ensure!(
        output.status.success() && output.stdout.len() <= 4 * 1024 * 1024,
        "Executable metadata preflight failed; refusing owned typing"
    );
    for line in std::str::from_utf8(&output.stdout)?.lines() {
        let line = line.trim_start();
        let split = line
            .find(char::is_whitespace)
            .context("Malformed process metadata")?;
        let pid: i32 = line[..split].parse()?;
        if pid == own_pid || Some(pid) == owned_peer_pid {
            continue;
        }
        let executable = Path::new(line[split..].trim());
        let name = executable.file_name().unwrap_or_default().to_string_lossy();
        let known_bundle = executable.components().any(|part| {
            let name = part.as_os_str().to_string_lossy();
            name.to_ascii_lowercase().ends_with(".app") && known_typing_observer(&name)
        });
        if known_typing_observer(&name) || known_bundle {
            observers.entry(pid).or_insert_with(|| name.into_owned());
        }
    }
    ensure!(
        observers.is_empty(),
        "Owned-choice smoke refused: other typing assistants could observe synthetic text and alter normal history: {}. Manually close these observers before retrying, or use a genuinely disposable desktop account with no normal observers. The fixture will not quit/pause them, change permissions, inspect profiles, or treat CI=1 as proof of isolation.",
        observers
            .iter()
            .map(|(pid, name)| format!("{name} [pid {pid}]"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// Resolve existing ancestors without creating anything, including symlinked
/// parent directories when the normal profile itself does not exist.
fn owned_location(path: &Path) -> Result<PathBuf> {
    if path.try_exists()? {
        return Ok(path.canonicalize()?);
    }
    let parent = path.parent().context("Owned path has no parent")?;
    Ok(owned_location(parent)?.join(path.file_name().context("Owned path has no name")?))
}

/// Pathnames are needed by AppKit/bundle launch. Rather than claiming an open
/// descriptor confines those APIs, accept only canonical chains that a different
/// non-root UID cannot replace. Root and this UID (including its other processes)
/// are trusted. Every ancestor must be root/current-owned, have no extended ACL,
/// and be non-group/world-writable or sticky. Under a sticky ancestor, the next
/// entry must also be root/current-owned. Symlinks are resolved once; validation
/// walks canonical chains with no-follow descriptors and IO uses only those paths.
fn owned_trusted_chain(path: &Path) -> Result<()> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    };
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            entry: libc::c_int,
            result: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    fn check(directory: &fs::File) -> Result<()> {
        let metadata = directory.metadata()?;
        ensure!(
            metadata.is_dir()
                && (metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() })
                && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0),
            "Owned path has a replaceable ancestor (owner, write or sticky policy)"
        );
        // Conservative policy: even deny-only/inherited ACLs are rejected. Do
        // not guess group membership, ACL ordering, delete or delete_child rights.
        unsafe {
            let acl = acl_get_fd_np(directory.as_raw_fd(), 0x100); // ACL_TYPE_EXTENDED
            if acl.is_null() {
                // Darwin reports an absent FILESEC_ACL property as ENOENT,
                // including for the valid descriptor checked above.
                let error = std::io::Error::last_os_error();
                ensure!(
                    error.raw_os_error() == Some(libc::ENOENT),
                    "Cannot inspect owned ancestor ACL: {error}"
                );
                return Ok(());
            }
            let mut entry = std::ptr::null_mut();
            let status = acl_get_entry(acl, 0, &mut entry); // Darwin ACL_FIRST_ENTRY
            let error = std::io::Error::last_os_error();
            acl_free(acl);
            ensure!(
                status == -1 && error.raw_os_error() == Some(libc::EINVAL),
                "Owned paths require ancestors without extended ACL entries"
            );
        }
        Ok(())
    }
    ensure!(path.is_absolute(), "Owned canonical path must be absolute");
    let root = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(root >= 0, "Cannot open filesystem root");
    let mut directory = unsafe { fs::File::from_raw_fd(root) };
    check(&directory)?;
    for component in path.components().skip(1) {
        let std::path::Component::Normal(name) = component else {
            anyhow::bail!("Owned canonical path contains traversal");
        };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(
            fd >= 0,
            "Owned ancestor changed or cannot be opened: {}",
            std::io::Error::last_os_error()
        );
        directory = unsafe { fs::File::from_raw_fd(fd) };
        check(&directory)?;
    }
    Ok(())
}

pub(super) struct OwnedProfileOutput {
    pub(super) home: PathBuf,
    pub(super) output: PathBuf,
}

impl OwnedProfileOutput {
    /// Filesystem-only seam: no environment, AppKit, observer or real profile access.
    fn validate(home: &Path, output: &Path, normal: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        for path in [home, output, normal] {
            ensure!(
                path.is_absolute()
                    && path.components().all(|part| matches!(
                        part,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )),
                "Owned paths must be absolute without traversal components"
            );
        }
        ensure!(
            !fs::symlink_metadata(home)?.file_type().is_symlink(),
            "INKLOW_HOME must not be a symlink"
        );
        let home = home.canonicalize()?;
        let parent = output
            .parent()
            .context("Owned output has no parent")?
            .canonicalize()?;
        // Prove the canonical chains stable before inspecting or creating leaves.
        owned_trusted_chain(&home)?;
        owned_trusted_chain(&parent)?;
        let output = parent.join(output.file_name().context("Owned output has no name")?);
        let metadata = fs::symlink_metadata(&home)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "INKLOW_HOME must be a real private directory owned by the current user"
        );
        ensure!(
            fs::read_dir(&home)?.next().is_none(),
            "Owned acceptance refuses a previously used INKLOW_HOME"
        );
        ensure!(
            matches!(fs::symlink_metadata(&output), Err(error)
            if error.kind() == std::io::ErrorKind::NotFound),
            "Owned output must be new, never an existing file or symlink"
        );
        let normal = owned_location(normal)?;
        ensure!(
            !home.starts_with(&normal)
                && !normal.starts_with(&home)
                && !output.starts_with(&normal)
                && !normal.starts_with(&output),
            "Owned profile/output must be outside the normal profile"
        );
        Ok(Self { home, output })
    }

    fn create_output(&self) -> Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&self.output)?;
        Ok(())
    }
}

/// Shared authorization and canonical IO roots for both owned native entries.
pub(super) fn owned_profile_output(output: &Path) -> Result<OwnedProfileOutput> {
    ensure!(
        std::env::var("BROTYPIST_OWNED_DESKTOP_SMOKE").as_deref() == Ok("1"),
        "Owned-choice acceptance requires BROTYPIST_OWNED_DESKTOP_SMOKE=1"
    );
    let home = std::env::var_os("INKLOW_HOME")
        .map(PathBuf::from)
        .context("Owned-choice acceptance requires a fresh private INKLOW_HOME")?;
    let paths = OwnedProfileOutput::validate(&home, output, &crate::paths::platform_config_dir()?)?;
    ensure!(
        home == paths.home,
        "Owned acceptance requires a canonical INKLOW_HOME; resolve its path before launch"
    );
    refuse_typing_observers(None)?;
    paths.create_output()?;
    Ok(paths)
}

/// Authorized current-desktop acceptance, separate from CI and layout entries.
pub(super) fn owned_choices(output: &Path) -> Result<i32> {
    owned_desktop(output, false)
}

/// Real inference is opt-in and never resolves the normal-profile model fallback.
pub(super) fn owned_model(output: &Path) -> Result<i32> {
    owned_desktop(output, true)
}

/// A deliberately narrow dispatcher for canonical Welcome actions, not a Shell.
/// All model ownership comes from the actual Controller release handshake.
struct OwnedWelcome {
    welcome: Welcome,
    controller: crate::runtime::Controller,
    kit: Kit,
    config: Config,
    permissions: Value,
    handshakes: Vec<Value>,
}
impl OwnedWelcome {
    fn status(&self) -> Value {
        let mut status = self.controller.status();
        status["permissions"] = self.permissions.clone();
        status
    }
    fn commands(&mut self, commands: Vec<Command>) -> Result<()> {
        for command in commands {
            let name = match command {
                Command::SuspendForPreview => "SuspendForPreview",
                Command::ResumeFromPreview => "ResumeFromPreview",
                _ => anyhow::bail!("Owned Welcome refused an unrelated controller command"),
            };
            self.handshakes
                .push(json!({"command":name,"before":self.controller.status(),
                "welcome":self.welcome.smoke_observation()}));
            self.controller.send(command)?;
            self.controller.poll();
            self.handshakes
                .push(json!({"command":name,"after":self.controller.status()}));
        }
        Ok(())
    }
    fn dispatch(&mut self) -> Result<()> {
        for _ in 0..64 {
            let action = self.kit.queue.borrow_mut().pending.pop_front();
            let Some(action) = action else { return Ok(()) };
            match action {
                UiAction::WelcomeStep(delta) => {
                    ensure!(
                        delta == 1 || delta == -1,
                        "Owned Welcome refuses step jumps"
                    );
                    let status = self.status();
                    let commands =
                        self.welcome
                            .navigate(delta, &self.config, &status, &json!([]), None)?;
                    self.commands(commands)?;
                }
                UiAction::PreviewLanguage(language) => self.welcome.preview_language(&language)?,
                UiAction::PreviewAccept(full) => self.welcome.preview_accept(full)?,
                UiAction::PreviewDismiss => self.welcome.preview_dismiss(),
                UiAction::PreviewReleased => self.commands(vec![Command::ResumeFromPreview])?,
                UiAction::Deactivated(id) | UiAction::Closed(id) if id == "inklow.welcome" => {
                    self.welcome.window_deactivated()?;
                }
                UiAction::Resized => {}
                _ => anyhow::bail!("Owned Welcome refused an unrelated native action"),
            }
        }
        anyhow::bail!("Owned Welcome action queue exceeded its bound")
    }
    fn refresh(&mut self) -> Result<()> {
        self.controller.poll();
        self.dispatch()?;
        let status = self.status();
        let commands = self
            .welcome
            .refresh(&self.config, &status, &json!([]), None)?;
        self.commands(commands)
    }
    fn pump(&mut self) -> Result<()> {
        choice_owned_events()?;
        self.refresh()
    }
    fn close_preview(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let closed = self.welcome.close()?;
            self.refresh()?;
            if closed && self.controller.status()["preview"] == false {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "Welcome preview child did not terminate/release"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn welcome_control(app: &AnyObject, id: &str) -> Result<Obj> {
    let window = window(app, "inklow.welcome")?;
    let root = content(&window)?;
    find(&root, id)
}

fn welcome_disable_assistance(editor: &AnyObject) -> Result<()> {
    // Applied only to this synthetic owned editor, before any language action.
    macro_rules! disable {
        ($setter:ident, $getter:ident) => {
            unsafe {
                let supported: bool = msg_send![editor, respondsToSelector:sel!($setter:)];
                if supported {
                    let _: () = msg_send![editor, $setter:false];
                    let enabled: bool = msg_send![editor, $getter];
                    ensure!(!enabled, "Owned Welcome text assistance remained enabled: {}", stringify!($setter));
                }
            }
        };
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

fn owned_welcome_case(
    owner: &mut OwnedWelcome,
    app: &AnyObject,
    output: &Path,
    name: &str,
    language: &str,
    full: bool,
    row: &mut Value,
) -> Result<()> {
    choice_owned_events()?;
    let popup = welcome_control(app, "inklow.welcome.previewLanguage")?;
    unsafe {
        let _: () =
            msg_send![&*popup, selectItemAtIndex:if language == "pt" {1isize} else {0isize}];
        let selector: objc2::runtime::Sel = msg_send![&*popup, action];
        let target: *mut AnyObject = msg_send![&*popup, target];
        let sent: bool = msg_send![app, sendAction:selector, to:target, from:&*popup];
        ensure!(sent, "Canonical preview language action was not dispatched");
    }
    owner.refresh()?;
    let editor = welcome_control(app, "inklow.welcome.preview")?;
    let before = macos_welcome::read_editor(&editor)?;
    let seed = if language == "pt" {
        "Eu estava pensando que poderíamos"
    } else {
        "I was thinking we could"
    };
    row["before"] = json!({"text":before.text.as_ref(),"caret":before.caret,
        "selection_length":before.selection_length});
    ensure!(
        before.text.as_ref() == seed && before.caret == seed.chars().count(),
        "Canonical language action did not seed its unchanged example"
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    row["observations"] = json!([]);
    let mut offered = None;
    while Instant::now() < deadline {
        owner.pump()?;
        let observed = owner.welcome.smoke_observation();
        let inference_error = observed["last_poll"]["state"] == "error";
        let editor_focused = observed["focused"] == true;
        let states = row["observations"].as_array_mut().unwrap();
        if states.last() != Some(&observed) {
            ensure!(
                states.len() < 4096,
                "Preview observation history exceeded its bound"
            );
            states.push(observed);
        }
        row["controller_status"] = owner.controller.status();
        ensure!(editor_focused, OwnedInterruption::EditorFocus);
        if inference_error {
            anyhow::bail!("Actual PreviewSession returned an inference error");
        }
        if let Some((snapshot, generated, visible)) = owner.welcome.preview_offer() {
            // This value exists only after the current canonical view's drawRect.
            let expected = if full {
                let mut suggestion = crate::core::SuggestionState {
                    snapshot: Some(snapshot.clone()),
                    text: generated.clone(),
                    ..Default::default()
                };
                suggestion
                    .accept(
                        &snapshot,
                        true,
                        owner.config.trailing_space,
                        crate::features::include_punctuation(&owner.config.punctuation_mode, true),
                        Some(&visible),
                    )
                    .and_then(|insertion| Some((insertion, suggestion.snapshot?)))
            } else {
                preview_word_expectation(&snapshot, &generated, &visible, &owner.config)
            };
            if let Some((insertion, expected)) = expected {
                offered = Some((snapshot, generated, visible, insertion, expected));
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    choice_owned_events()?;
    row["after_wait_ownership"] = owner.welcome.smoke_observation();
    ensure!(
        row["after_wait_ownership"]["focused"] == true,
        OwnedInterruption::EditorFocus
    );
    let Some((snapshot, generated, visible, insertion, expected)) = offered else {
        row["outcome"] = json!("bounded_no_acceptable_draw");
        row["terminal_abstention_proved"] = json!(false);
        let unchanged = macos_welcome::read_editor(&editor)?;
        if owner.welcome.preview_offer().is_none() {
            owner.welcome.preview_accept(false)?;
            owner.welcome.preview_accept(true)?;
        }
        let after = macos_welcome::read_editor(&editor)?;
        row["unchanged_readback"] = json!(before.same_key(&unchanged) && before.same_key(&after));
        anyhow::bail!("No actual acceptable preview draw within the single 120s attempt");
    };
    ensure!(
        before.same_key(&snapshot),
        "Preview offer changed its canonical seed"
    );
    row["generated"] = json!(generated);
    row["drawn"] = json!(visible);
    row["expected_insertion"] = json!(insertion);
    row["offered_bitmap"] = screenshot(
        app,
        "inklow.welcome",
        output,
        &format!("{name}-offered"),
        false,
    )?;
    // Re-observe after capture; a bitmap is evidence, never an authority grant.
    choice_owned_events()?;
    row["before_acceptance_ownership"] = owner.welcome.smoke_observation();
    ensure!(
        row["before_acceptance_ownership"]["focused"] == true,
        OwnedInterruption::EditorFocus
    );
    let current = owner
        .welcome
        .preview_offer()
        .context("Current draw was revoked before acceptance")?;
    ensure!(
        current.0.same_key(&snapshot) && current.1 == generated && current.2 == visible,
        "Current-view offer changed before acceptance"
    );
    let shortcut = if full { "all" } else { "word" };
    ensure!(
        owner.config.shortcuts.get(shortcut).map(String::as_str)
            == Some(if full { "AboveTab" } else { "Tab" }),
        "Unexpected canonical acceptance shortcut"
    );
    let owned_window = window(app, "inklow.welcome")?;
    unsafe {
        let number: isize = msg_send![&*owned_window, windowNumber];
        let characters = NSString::from_str(if full { "`" } else { "\t" });
        let key: *mut AnyObject = msg_send![AnyClass::get(c"NSEvent").unwrap(),
            keyEventWithType:10usize, location:NSPoint::new(0., 0.), modifierFlags:0usize,
            timestamp:0.0f64, windowNumber:number, context:std::ptr::null::<AnyObject>(),
            characters:&*characters, charactersIgnoringModifiers:&*characters,
            isARepeat:false, keyCode:if full {50u16} else {48u16}];
        ensure!(
            !key.is_null(),
            "AppKit refused the owned preview acceptance event"
        );
        let _: () = msg_send![&*editor, keyDown:key];
    }
    owner.dispatch()?;
    row["after_acceptance_ownership"] = owner.welcome.smoke_observation();
    ensure!(
        row["after_acceptance_ownership"]["focused"] == true,
        OwnedInterruption::EditorFocus
    );
    row["acceptance_action"] = json!(
        "Synthetic NSEvent delivered only to canonical owned NSTextView keyDown/target action"
    );
    let actual = macos_welcome::read_editor(&editor)?;
    row["readback"] = json!({"text":actual.text.as_ref(),"caret":actual.caret,
        "selection_length":actual.selection_length});
    ensure!(
        expected.same_key(&actual),
        "Canonical PreviewSession acceptance readback mismatch"
    );
    // No refresh/redraw between repeats: neither action may reuse consumed ink.
    owner.welcome.preview_accept(false)?;
    owner.welcome.preview_accept(true)?;
    let repeated = macos_welcome::read_editor(&editor)?;
    row["one_use"] = json!(actual.same_key(&repeated));
    ensure!(
        actual.same_key(&repeated),
        "Consumed preview draw accepted twice"
    );
    row["outcome"] = json!("drawn_and_accepted");
    row["accepted_bitmap"] = screenshot(
        app,
        "inklow.welcome",
        output,
        &format!("{name}-accepted"),
        false,
    )?;
    row["completed_case_ownership"] = owner.welcome.smoke_observation();
    ensure!(
        row["completed_case_ownership"]["focused"] == true,
        OwnedInterruption::EditorFocus
    );
    Ok(())
}

fn owned_welcome_revoke(
    owner: &mut OwnedWelcome,
    app: &AnyObject,
    kind: &str,
    row: &mut Value,
) -> Result<()> {
    choice_owned_events()?;
    let editor = welcome_control(app, "inklow.welcome.preview")?;
    let owned_window = window(app, "inklow.welcome")?;
    row["before"] = owner.welcome.smoke_observation();
    ensure!(
        row["before"]["session_present"] == true,
        "Revocation requires an actual live PreviewSession, not an invented offer"
    );
    unsafe {
        match kind {
            "focus" => {
                let accepted: bool =
                    msg_send![&*owned_window, makeFirstResponder:std::ptr::null::<AnyObject>()];
                ensure!(accepted, "Owned Welcome refused focus revocation");
            }
            "selection" => {
                let _: () = msg_send![&*editor, setSelectedRange:NSRange::new(0, 1)];
            }
            "marked_text" => {
                let selected: NSRange = msg_send![&*editor, selectedRange];
                let _: () = msg_send![&*editor, setMarkedText:&*NSString::from_str("あ"),
                    selectedRange:NSRange::new(1, 0), replacementRange:selected];
                let marked: bool = msg_send![&*editor, hasMarkedText];
                ensure!(
                    marked,
                    "Owned NSTextView did not enter actual marked-text state"
                );
            }
            _ => anyhow::bail!("Unknown owned revocation case"),
        }
    }
    let baseline = macos_welcome::read_editor(&editor)?;
    owner.refresh()?;
    owner.welcome.preview_accept(false)?;
    owner.welcome.preview_accept(true)?;
    let after = macos_welcome::read_editor(&editor)?;
    row["after_mutation"] = owner.welcome.smoke_observation();
    ensure!(
        row["after_mutation"]["active"] == false
            && match kind {
                "focus" => row["after_mutation"]["focused"] == false,
                "selection" => after.selection_length == 1,
                "marked_text" => row["after_mutation"]["marked"] == true,
                _ => false,
            },
        "Actual native widget did not retain the requested revocation state"
    );
    row["readback"] = json!({"text":after.text.as_ref(),"caret":after.caret,
        "selection_length":after.selection_length});
    row["stale_acceptance_refused"] = json!(baseline.same_key(&after));
    ensure!(
        baseline.same_key(&after) && owner.welcome.preview_offer().is_none(),
        "Revoked native widget accepted or retained a preview offer"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while owner.welcome.smoke_observation()["session_present"] == true
        || owner.controller.status()["preview"] != false
    {
        owner.refresh()?;
        ensure!(
            Instant::now() < deadline,
            "Revocation did not terminate the preview worker"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    row["after_release"] = owner.welcome.smoke_observation();
    row["controller_after_release"] = owner.controller.status();
    row["passed"] = json!(true);
    row["scope"] = json!(
        "Actual owned focus/selection/marked-text state and session termination; not physical IME or unconsumed-offer replay"
    );
    if kind == "marked_text" {
        unsafe {
            let _: () = msg_send![&*editor, unmarkText];
        }
    }
    Ok(())
}

pub(super) fn owned_welcome(output: &Path) -> Result<i32> {
    use crate::runtime::{AsyncPreparer, Controller, Engine, MonotonicClock, SharedStorage};
    use std::io::Read;
    use std::sync::{Arc, Mutex, atomic::AtomicU64};
    let paths = owned_profile_output(output)?;
    let output = paths.output.as_path();
    let mut evidence = json!({"cases":[],"revocations":[],"bilingual_positive_coverage":false,
        "scenario":"canonical-welcome-en-word-en-full-pt-full-v1",
        "planned_cases":[{"name":"en-word","language":"en","full":false},
            {"name":"en-full","language":"en","full":true},
            {"name":"pt-full","language":"pt","full":true}],
        "request_deadline_seconds":120,"cleanup_deadline_seconds":10});
    let result = (|| -> Result<()> {
        let supplied = std::env::var_os("BROTYPIST_SMOKE_MODEL").filter(|v| !v.is_empty())
            .context("--owned-welcome requires an explicit existing E4B BROTYPIST_SMOKE_MODEL; no fallback/download")?;
        let supplied = PathBuf::from(supplied);
        ensure!(
            supplied.is_absolute(),
            "BROTYPIST_SMOKE_MODEL must be absolute"
        );
        let model = supplied.canonicalize()?;
        let preset = crate::models::get_preset("gemma4-e4b")?;
        let mut file = fs::File::open(&model)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.len() == preset.size_bytes
                && model.file_name().and_then(|name| name.to_str())
                    == Some(preset.filename.as_str()),
            "Owned Welcome requires the explicit catalog E4B filename and size; no model substitution"
        );
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;
        ensure!(&magic == b"GGUF", "Explicit E4B file has no GGUF header");
        evidence["model"] = json!({"path":model,"bytes":metadata.len(),"preset":preset.id,
            "access":"read-only","checksum_verified_by_this_run":false,
            "identity_scope":"explicit caller-supplied previously verified artifact; catalog filename/size and GGUF header checked"});
        let config = Config {
            backend: "gguf".into(),
            model_path: model.to_string_lossy().into_owned(),
            model_id: String::new(),
            endpoint: String::new(),
            prompt_style: preset.prompt_style.clone(),
            enabled: false,
            global_enabled: false,
            setup_completed: false,
            launch_at_login: false,
            check_updates_automatically: false,
            record_history: false,
            record_unaccepted: false,
            use_history: false,
            personalization_strength: 0,
            statistics_enabled: false,
            screen_context: false,
            clipboard_context: false,
            screenshot_context: false,
            screenshot_appearance: false,
            ..Config::default()
        };
        evidence["config"] = json!(config);
        let mtm =
            MainThreadMarker::new().context("Owned Welcome requires the AppKit main thread")?;
        let app: Obj = unsafe {
            let raw: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            Retained::retain(raw).context("AppKit unavailable")?
        };
        unsafe {
            let _: bool = msg_send![&*app, setActivationPolicy:1isize];
            let _: () = msg_send![&*app, finishLaunching];
        }
        activate_app(&app);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !choice_app_events(None)? {
            ensure!(
                Instant::now() < deadline,
                "Owned Welcome activation timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let kit = Kit::new(mtm, true);
        // Hidden, empty synthetic Controller host. It cannot capture Welcome or
        // external text; only Welcome's independent PreviewSession performs inference.
        let host = Rc::new(RefCell::new(ChoiceHost {
            window: kit.window(
                "Owned preview controller",
                "inklow.preview-controller",
                320.,
                160.,
                false,
            ),
            editors: [
                kit.editor(
                    "inklow.preview-controller.first",
                    "Unused owned field",
                    rect(0., 0., 100., 40.),
                    "",
                ),
                kit.editor(
                    "inklow.preview-controller.second",
                    "Unused owned field",
                    rect(0., 50., 100., 40.),
                    "",
                ),
            ],
            revision: 1,
            edits: 0,
            captures: Cell::new(0),
            enabled: false,
            started: false,
            shortcut: None,
            changed: None,
            model_clock: None,
            model_input_at: None,
            model_notifications: Vec::new(),
        }));
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(
            crate::storage::HistoryStore::with_key_loader(
                output.join("owned-welcome-private.sqlite3"),
                Box::new(|| anyhow::bail!("Owned Welcome must never unlock recording")),
            ),
        )));
        let epoch = Arc::new(AtomicU64::new(1));
        let preparer = AsyncPreparer::new(storage.clone(), epoch.clone(), Arc::new(ChoiceReaders))?;
        let bridge_host = host.clone();
        let model_path = config.model_path.clone();
        let controller = Controller::with_engine(Engine::with_native_factory(
            config.clone(),
            Box::new(crate::process_client::WorkerClient::new()?),
            storage,
            Box::new(preparer),
            None,
            Box::new(ChoiceReaders),
            Box::new(MonotonicClock),
            epoch,
            Box::new(move |_| {
                Ok(Box::new(ChoiceBridge(
                    bridge_host.clone(),
                    Some(model_path.clone()),
                )))
            }),
        )?)?;
        let mut owner = OwnedWelcome {
            welcome: Welcome::new(kit.clone())?,
            controller,
            kit,
            config,
            // Exactly the canonical nonprompting prerequisite checks, with no
            // automation/external AX capture queries or permission request actions.
            permissions: json!({"accessibility":crate::platform::macos::ax::trusted(),
                "input_monitoring":unsafe { CGPreflightListenEventAccess()!=0 }}),
            handshakes: Vec::new(),
        };
        let run = (|| -> Result<()> {
            let status = owner.status();
            let commands = owner
                .welcome
                .show(&owner.config, &status, &json!([]), None)?;
            owner.commands(commands)?;
            for step in 0..4 {
                owner.pump()?;
                evidence["navigation"] = owner.welcome.smoke_observation();
                evidence["actual_status"] = owner.status();
                let next = welcome_control(&app, "inklow.welcome.next")?;
                if !is_enabled(&next) {
                    evidence["blocked"] = json!({"step":step,"next_enabled":false,
                        "prerequisite":if step == 2 {"Actual Accessibility and Input Monitoring permission"} else {"Actual selected local model readiness"},
                        "status":owner.status(),"before_inference":true});
                    evidence["blocked_bitmap"] =
                        screenshot(&app, "inklow.welcome", output, "welcome-blocked", false)?;
                    anyhow::bail!(
                        "Canonical Welcome Next is disabled at step {step}; prerequisite was not bypassed"
                    );
                }
                press(&owner.kit, &next)?;
                owner.refresh()?;
            }
            let editor = welcome_control(&app, "inklow.welcome.preview")?;
            welcome_disable_assistance(&editor)?;
            evidence["native_text_assistance_disabled"] = json!(true);
            for (name, language, full, revoke) in [
                ("en-word", "en", false, "focus"),
                ("en-full", "en", true, "selection"),
                ("pt-full", "pt", true, "marked_text"),
            ] {
                let mut row = json!({"name":name,"language":language,"full":full,"attempts":1});
                let case =
                    owned_welcome_case(&mut owner, &app, output, name, language, full, &mut row);
                row["passed"] = json!(case.is_ok());
                row["error"] = json!(case.as_ref().err().map(|e| format!("{e:#}")));
                row["welcome_at_result"] = owner.welcome.smoke_observation();
                row["controller_at_result"] = owner.controller.status();
                if case.is_err() {
                    match screenshot(
                        &app,
                        "inklow.welcome",
                        output,
                        &format!("{name}-failure"),
                        false,
                    ) {
                        Ok(bitmap) => row["failure_bitmap"] = bitmap,
                        Err(error) => row["failure_bitmap_error"] = json!(format!("{error:#}")),
                    }
                }
                evidence["cases"].as_array_mut().unwrap().push(row);
                // Retain the failed row, then abort before revocation can consume
                // another event or make a later fixed case appear safe to seed.
                owned_matrix_continue(&case)?;
                let mut revocation = json!({"kind":revoke,"case":name});
                let revoked = owned_welcome_revoke(&mut owner, &app, revoke, &mut revocation);
                revocation["error"] = json!(revoked.as_ref().err().map(|e| format!("{e:#}")));
                evidence["revocations"]
                    .as_array_mut()
                    .unwrap()
                    .push(revocation);
                // Inference/no-draw errors do not replace or skip later fixed
                // cases. A failed ownership fence/cleanup must stop further work.
                revoked?;
                choice_owned_events()?;
            }
            let cases = evidence["cases"].as_array().unwrap();
            let bilingual = ["en", "pt"].iter().all(|language| {
                cases
                    .iter()
                    .any(|row| row["language"] == *language && row["passed"] == true)
            });
            let all_passed = cases.iter().all(|row| row["passed"] == true);
            evidence["bilingual_positive_coverage"] = json!(bilingual);
            ensure!(
                bilingual && all_passed,
                "Canonical preview matrix lacks required actual positive cases; negative receipts retained"
            );
            Ok(())
        })();
        // Always reap the real PreviewSession worker before resuming/shutting down
        // its Controller, even when navigation, inference, drawing or readback fails.
        let preview_close = owner.close_preview();
        evidence["preview_cleanup"] = json!({"passed":preview_close.is_ok(),
            "error":preview_close.as_ref().err().map(|e| format!("{e:#}")),
            "welcome":owner.welcome.smoke_observation(),"controller":owner.controller.status()});
        evidence["handshakes"] = json!(owner.handshakes);
        let deadline = Instant::now() + Duration::from_secs(10);
        let shutdown = (|| -> Result<()> {
            while !owner.controller.shutdown()? {
                ensure!(
                    Instant::now() < deadline,
                    "Owned Welcome Controller shutdown timed out"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        })();
        evidence["controller_cleanup"] = json!({"passed":shutdown.is_ok(),
            "error":shutdown.as_ref().err().map(|e| format!("{e:#}")),
            "status":owner.controller.status(),"owned_host_edits":host.borrow().edits});
        preview_close?;
        shutdown?;
        run
    })();
    write_new(
        &output.join("receipt.json"),
        &serde_json::to_vec_pretty(&json!({
            "schema":"brotypist-owned-welcome-preview/1","passed":result.is_ok(),
            "scope":"Canonical Welcome controls/PreviewSession only; no general onboarding, installation, ordinary-writing quality, persistence, physical input/IME or external AX-host claim",
            "safety":{"explicit_owned_authorization":true,"private_fresh_profile":true,
                "observer_preflight":true,"resident_shell":false,"global_native_hooks":false,
                "external_capture":false,"permission_prompts":false,"model_downloads":false},
            "filesystem_scope":"Private application profile, not OS-framework or daemon filesystem confinement",
            "evidence":evidence,"error":result.as_ref().err().map(|e| format!("{e:#}"))
        }))?,
    )?;
    result?;
    Ok(0)
}

fn owned_desktop(output: &Path, real_model: bool) -> Result<i32> {
    let paths = owned_profile_output(output)?;
    let home = &paths.home;
    let output = paths.output.as_path();
    let mut evidence = json!({});
    let result = (|| -> Result<Value> {
        let model = if real_model {
            use std::io::Read;
            let supplied = std::env::var_os("BROTYPIST_SMOKE_MODEL")
            .filter(|value| !value.is_empty())
            .context("--owned-model requires an explicitly supplied existing GGUF in BROTYPIST_SMOKE_MODEL; no fallback or download")?;
            let supplied = PathBuf::from(supplied);
            ensure!(
                supplied.is_absolute(),
                "BROTYPIST_SMOKE_MODEL must be absolute"
            );
            // The explicit nonempty prerequisite above prevents smoke_model_path's
            // legacy Tiny fallback from participating in this entry.
            let path = super::super::smoke_model_path()?.canonicalize()?;
            ensure!(
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf")),
                "BROTYPIST_SMOKE_MODEL must identify a GGUF file"
            );
            let mut file = fs::File::open(&path)?;
            let mut magic = [0; 4];
            file.read_exact(&mut magic)?;
            ensure!(
                &magic == b"GGUF",
                "Explicit model does not have a GGUF header"
            );
            evidence["model"] = json!({"path":path,"bytes":file.metadata()?.len(),
            "selection":"explicit BROTYPIST_SMOKE_MODEL only","checksum_verified_by_this_run":false});
            Some(path)
        } else {
            None
        };
        write_new(
            &home.join(if real_model {
                ".owned-model-smoke"
            } else {
                ".owned-choice-smoke"
            }),
            if real_model {
                b"Explicit local GGUF owned-widget smoke\n"
            } else {
                b"Controlled synthetic model-output fixture\n"
            },
        )?;
        let mtm = MainThreadMarker::new()
            .context("Owned-choice acceptance requires the main AppKit thread")?;
        let app: Obj = unsafe {
            let raw: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            Retained::retain(raw).context("AppKit is unavailable")?
        };
        unsafe {
            let _: bool = msg_send![&*app,setActivationPolicy:1isize];
            let _: () = msg_send![&*app, finishLaunching];
        }
        // Explicit authorization covers displaying/focusing only this process's two
        // synthetic fields. A later focus loss aborts instead of stealing focus back.
        activate_app(&app);
        let kit = Kit::new(mtm, true);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !choice_app_events(None)? {
            ensure!(
                Instant::now() < deadline,
                "Owned application activation timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        capture_owned_acceptance(&app, &kit, output, model.as_deref(), &mut evidence)
    })();
    let receipt = json!({
        "schema":if real_model { "brotypist-owned-model-timeline/1" } else { "brotypist-owned-choice-fixture/2" },
        "passed":result.is_ok(),
        "scope":if real_model {
            "Explicit local GGUF, one WorkerClient, existing diagnostics and actual owned OverlayOwner/NSTextView draw/edit; not full64 semantic quality, physical keys, compositor or external AX-host proof"
        } else {
            "controlled provider/WorkerClient/Controller/OverlayOwner path with owned NSTextView choice and ghost lifecycle; not real-model quality, physical typing, external AX-host acceptance or persisted provenance"
        },
        "filesystem_scope":"Private INKLOW_HOME is application-profile isolation, not OS-framework filesystem confinement. AppKit/Foundation and system daemons can use normal login-account preferences/caches; no host-wide no-write claim.",
        "evidence":evidence,
        "safety":{"explicit_desktop_authorization":true,"private_fresh_profile":true,
            "resident_shell_started":false,"global_native_bridge":false,"global_hooks":false,
            "known_observer_preflight_passed":true,
            "native_text_assistance_disabled":evidence["native_text_assistance_disabled"],
            "external_text_capture":false,"external_screenshots":false,"keyboard_injection":false,
            "permission_prompts":false,"credentials_unlocked":false,"recording":false,
            "history_use":false,"personalization":false,"statistics":false,"context":false,
            "appearance_capture":false,"startup":false,"updates":false},
        "acceptance":result.as_ref().ok(),
        "error":result.as_ref().err().map(|error| format!("{error:#}"))
    });
    write_new(
        &output.join("receipt.json"),
        &serde_json::to_vec_pretty(&receipt)?,
    )?;
    result?;
    Ok(0)
}

/// Bounded rendering only: no Shell, Controller, profile, input hooks, services or activation.
pub(super) fn capture_layout(output: &Path) -> Result<i32> {
    ensure!(
        output.is_absolute() && !output.exists(),
        "Layout output must be a new absolute directory"
    );
    refuse_typing_observers(None)?;
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
    let native_status_message = "Native input: native_available. Keyboard: ready. Last ordinary field capture: attributed_text_unverified.";
    let status = json!({
        "state":"paused", "model_name":"Gemma 4 E2B", "current_app":"",
        "message":"This field does not expose verified same-line rendering; ordinary completion is silent.",
        "paused_remaining":0, "permissions":{},
        "native_status":{"message":native_status_message}
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
    let mut render_inventory = Value::Null;
    let mut surface_evidence = Vec::new();
    let result = (|| -> Result<()> {
        // Embedded catalog metadata only: never enumerate model files, inspect the
        // normal profile, or imply that fixture availability measures this Mac.
        let presets = crate::models::presets();
        let selected = presets
            .iter()
            .find(|preset| preset.id == config.model_id)
            .or_else(|| presets.first())
            .context("The embedded model catalog is empty")?;
        render_inventory = Value::Array(
            presets
                .iter()
                .map(|preset| -> Result<Value> {
                    let mut row = serde_json::to_value(preset)?;
                    row["installed"] = json!(false);
                    row["recommended"] = json!(false);
                    row["availability"] = json!(
                        "Render fixture only; installation and hardware availability unmeasured."
                    );
                    Ok(row)
                })
                .collect::<Result<Vec<_>>>()?,
        );
        settings.model_selection = Some(selected.id.clone());
        // Keep all twelve baseline panes, then exercise the same representative
        // forms at both size extremes. Geometry comes from alignment_report's
        // current text/control contract rather than copied alignment thresholds.
        for (appearance, panes, width, height, suffix, advanced) in [
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
                false,
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
                    "appSettings",
                ][..],
                760.,
                560.,
                "minimum",
                false,
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
                    "appSettings",
                ][..],
                1000.,
                740.,
                "resized",
                false,
            ),
            (
                "NSAppearanceNameDarkAqua",
                &["models"][..],
                900.,
                640.,
                "advanced-baseline",
                true,
            ),
            (
                "NSAppearanceNameDarkAqua",
                &["models"][..],
                760.,
                560.,
                "advanced-minimum",
                true,
            ),
            (
                "NSAppearanceNameAqua",
                &["models"][..],
                1000.,
                740.,
                "advanced-resized",
                true,
            ),
            (
                "NSAppearanceNameDarkAqua",
                &["models"][..],
                900.,
                640.,
                "memory-unspecified",
                false,
            ),
            (
                "NSAppearanceNameDarkAqua",
                &["models"][..],
                900.,
                640.,
                "memory-specified",
                false,
            ),
        ] {
            unsafe {
                let appearance: *mut AnyObject = msg_send![AnyClass::get(c"NSAppearance").unwrap(),appearanceNamed:&*NSString::from_str(appearance)];
                ensure!(!appearance.is_null(), "Native appearance is unavailable");
                let _: () = msg_send![&*app,setAppearance:appearance];
            }
            for pane in panes {
                let selected = match suffix {
                    "memory-unspecified" => presets
                        .iter()
                        .find(|preset| preset.minimum_memory_bytes == 0)
                        .context("Catalogue has no unspecified-memory render case")?,
                    "memory-specified" => presets
                        .iter()
                        .find(|preset| preset.minimum_memory_bytes > 0)
                        .context("Catalogue has no positive-memory render case")?,
                    _ => selected,
                };
                settings.model_selection = Some(selected.id.clone());
                settings.advanced = advanced;
                settings.prepare(pane, &config, &status, &render_inventory, &json!({}))?;
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
                let root = content(&owned)?;
                let mut surfaces = Vec::new();
                if *pane == "models" {
                    let description = find(&root, "inklow.models.description")?;
                    let text = string(&description);
                    ensure!(
                        text.contains(&selected.license_name)
                            && text.contains(&selected.license_url)
                            && text.contains("Catalog download:")
                            && text.contains("Minimum physical memory:")
                                == (selected.minimum_memory_bytes > 0)
                            && text.contains("Render fixture only"),
                        "Models lost its canonical resource/license fixture details"
                    );
                    let picker = find(&root, "inklow.models.model_preset")?;
                    unsafe {
                        let index: isize = msg_send![&*picker, indexOfSelectedItem];
                        let expected = presets
                            .iter()
                            .position(|preset| preset.id == selected.id)
                            .context("Selected catalogue entry disappeared")?
                            + 1;
                        ensure!(
                            index == expected as isize,
                            "Models did not select the requested native catalogue row"
                        );
                        surface_evidence.push(json!({
                            "pane":pane,"size_case":suffix,"surface":"catalog-selection",
                            "catalog_entry":selected,"native_selected_index":index,
                            "minimum_memory_specified":selected.minimum_memory_bytes > 0,
                            "detail_text":text,"installed":false,
                            "actions_dispatched":false
                        }));
                    }
                    surfaces.push(("resource-details", description));
                    surfaces.push(("license", find(&root, "inklow.models.license")?));
                    if advanced {
                        let token_cap = find(&root, "inklow.models.max_tokens")?;
                        ensure!(
                            string(&token_cap) == config.max_tokens.to_string(),
                            "Advanced Models lost the configured ordinary token cap"
                        );
                        surfaces.push(("token-cap", token_cap));
                    }
                } else if *pane == "appSettings" {
                    surfaces.push((
                        "hard-exclusion",
                        find(&root, "inklow.appSettings.addExclusion")?,
                    ));
                    for (name, expected) in [
                        (
                            "rule-help",
                            "Add App creates an app-specific settings rule, not a hard exclusion.",
                        ),
                        (
                            "hard-exclusion-help",
                            "Case-insensitive substring matching against app identifiers and names, not website hostnames.",
                        ),
                    ] {
                        let help = descendants(&root)?
                            .into_iter()
                            .find(|view| unsafe {
                                let readable: bool =
                                    msg_send![&**view,respondsToSelector:sel!(stringValue)];
                                readable && string(view).contains(expected)
                            })
                            .with_context(|| format!("App Settings lost {name}"))?;
                        surfaces.push((name, help));
                    }
                }
                for (name, view) in surfaces {
                    let id = native_id(&view);
                    unsafe {
                        let bounds: NSRect = msg_send![&*view, bounds];
                        let _: bool = msg_send![&*view,scrollRectToVisible:bounds];
                        let _: () = msg_send![&*root, layoutSubtreeIfNeeded];
                        let visible: NSRect = msg_send![&*view, visibleRect];
                        ensure!(
                            bounds.size.width > 0.
                                && bounds.size.height > 0.
                                && visible.size.width >= bounds.size.width - 1.
                                && visible.size.height >= bounds.size.height - 1.,
                            "{pane}/{suffix}/{name} is not fully reachable"
                        );
                        let mut accessibility_id = None;
                        if !id.is_empty() {
                            let native: *mut NSString = msg_send![&*view, accessibilityIdentifier];
                            ensure!(
                                !native.is_null(),
                                "{id} has no native accessibility identity"
                            );
                            let native = (&*native).to_string();
                            ensure!(native == id, "{id} has a different accessibility identity");
                            accessibility_id = Some(native);
                        }
                        let readable: bool = msg_send![&*view,respondsToSelector:sel!(stringValue)];
                        surface_evidence.push(json!({
                            "pane":pane,"size_case":suffix,"surface":name,
                            "native_id":id,"accessibility_id":accessibility_id,
                            "text":if readable { Some(string(&view)) } else { None },
                            "bounds":layout_rect(bounds),"visible_rect":layout_rect(visible),
                            "fully_reachable":true,"actions_dispatched":false
                        }));
                    }
                    geometry.push(layout_geometry(&settings, &app, pane, suffix, name)?);
                    captures.push(screenshot(
                        &app,
                        "inklow.settings",
                        output,
                        &format!("{pane}-{suffix}-{name}"),
                        true,
                    )?);
                }
                if matches!(
                    *pane,
                    "general" | "setup" | "models" | "context" | "personalization" | "appSettings"
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
    let menu_glyphs = capture_menu_glyphs(output);
    let emoji = capture_choice_surfaces(&app, output);
    let ghosts = capture_inline_ghosts(&app, output);
    let receipt = json!({
        "schema":"brotypist-macos-layout-capture/7", "passed":result.is_ok() && menu_glyphs.is_ok() && emoji.is_ok() && ghosts.is_ok(),
        "scope":"Canonical owned native Settings controls, theme frame, chooser and same-line OverlayOwner ghost; synthetic preferences/status/writing and embedded catalog render fixtures, no runtime services. Native menu inspected without opening it.",
        "safety":{"controller_started":false,"profile_loaded":false,"input_hooks_started":false,"foreground_activation_allowed":false,"external_screen_capture":false,"model_files_inspected":false,"model_loaded":false,"license_urls_opened":false,"modal_actions_dispatched":false},
        "limits":"Native geometry assertions and NSView rasterization do not prove WindowServer vibrancy/compositing, runtime choice acceptance, insertion, external-host typing, persistence, license consent or measured model availability. Visual review of images is required.",
        "model_render_fixture":{"source":"models::presets embedded canonical metadata","installed":false,"availability":"Explicit render fixture, not installed-model or hardware-fit evidence","rows":render_inventory},
        "canonical_surfaces":surface_evidence,
        "menu":menu_items,"screenshots":captures,"geometry":geometry,
        "menu_glyphs":menu_glyphs.as_ref().ok(),
        "draft_retention":draft_retention,
        "emoji":emoji.as_ref().ok(),
        "inline_ghosts":ghosts.as_ref().ok(),
        "error":result.as_ref().err().or_else(||menu_glyphs.as_ref().err()).or_else(||emoji.as_ref().err()).or_else(||ghosts.as_ref().err()).map(|error|format!("{error:#}"))
    });
    write_new(
        &output.join("receipt.json"),
        &serde_json::to_vec_pretty(&receipt)?,
    )?;
    result?;
    menu_glyphs?;
    emoji?;
    ghosts?;
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
        "Allow local writing history?",
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
    consent(
        shell,
        "inklow.personalization.use_history",
        "Allow local writing history?",
        "Cancel",
    )?;
    assert_record(
        receipt,
        "privacy.use_consent_independent",
        !shell.config().record_history && !shell.config().use_history,
        json!({"record":shell.config().record_history,"use":shell.config().use_history}),
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
fn set_native_preference(shell: &mut Shell, id: &str, value: &str) -> Result<()> {
    let view = control(shell, id)?;
    set_string(&view, value);
    unsafe {
        let selector: objc2::runtime::Sel = msg_send![&*view, action];
        let target: *mut AnyObject = msg_send![&*view, target];
        let sent: bool = msg_send![&*shell.app, sendAction:selector, to:target, from:&*view];
        ensure!(
            sent,
            "Native preference did not dispatch its captured value"
        );
    }
    settle(shell)
}

fn settings_operations(shell: &mut Shell, receipt: &mut Value) -> Result<()> {
    navigate(shell, "general")?;
    set_native_preference(shell, "inklow.general.writing_language", "pt-br")?;
    set_native_preference(shell, "inklow.general.spelling_language", "zz-ZZ")?;
    assert_record(
        receipt,
        "language.canonical_tags_and_exact_unavailability",
        shell.config().writing_language == "pt-BR"
            && shell.config().spelling_language == "zz-ZZ"
            && string(&*control(shell, "inklow.general.spellingAvailability")?)
                .contains("exact macOS dictionary unavailable"),
        json!({"writing_language":shell.config().writing_language,
            "spelling_language":shell.config().spelling_language,
            "native_availability":string(&*control(shell, "inklow.general.spellingAvailability")?)}),
    )?;
    set_native_preference(shell, "inklow.general.writing_language", "auto")?;
    set_native_preference(shell, "inklow.general.spelling_language", "auto")?;
    let invalid_guard = ModalGuard::arm("BroTypist couldn’t complete that change", "OK", true)?;
    set_native_preference(shell, "inklow.general.writing_language", "invalid_tag")?;
    invalid_guard.finish()?;
    navigate(shell, "emoji")?;
    navigate(shell, "general")?;
    assert_record(
        receipt,
        "language.invalid_native_draft_survives_refresh_and_navigation",
        shell.config().writing_language == "auto"
            && string(&*control(shell, "inklow.general.writing_language")?) == "invalid_tag",
        json!({"saved":shell.config().writing_language,
            "native_draft":string(&*control(shell, "inklow.general.writing_language")?)}),
    )?;
    set_native_preference(shell, "inklow.general.writing_language", "auto")?;
    navigate(shell, "personalization")?;
    set_native_preference(shell, "inklow.personalization.history_retention_days", "7")?;
    assert_record(
        receipt,
        "history.retention_does_not_enable_record_or_use",
        shell.config().history_retention_days == 7
            && !shell.config().record_history
            && !shell.config().use_history,
        json!({"retention_days":shell.config().history_retention_days,"record":shell.config().record_history,"use":shell.config().use_history}),
    )?;
    set_native_preference(shell, "inklow.personalization.history_retention_days", "0")?;

    shell.add_rules(vec![(0, "test.brotypist.owned-settings".into())])?;
    settle(shell)?;
    navigate(shell, "appSettings")?;
    for field in [
        "use_history",
        "screen_context",
        "clipboard_context",
        "screenshot_context",
        "screenshot_appearance",
    ] {
        let id = format!("inklow.appSettings.rule_{field}");
        choose(shell, &id, 2)?;
        ensure!(
            shell.config().app_rules["app:test.brotypist.owned-settings"][field] == false,
            "Explicit scoped false was not saved"
        );
        choose(shell, &id, 0)?;
        ensure!(
            !shell.config().app_rules["app:test.brotypist.owned-settings"].contains_key(field),
            "Scoped inheritance was not restored"
        );
    }
    assert_record(
        receipt,
        "policy.scoped_false_and_inheritance_native_controls",
        true,
        json!({"scope":"owned synthetic app","fields":["use_history","screen_context","clipboard_context","screenshot_context","screenshot_appearance"],
            "native_context":string(&*control(shell, "inklow.appSettings.contextPolicy")?)}),
    )?;

    navigate(shell, "personalization")?;
    let editor = control(shell, "inklow.personalization.globalInstructions")?;
    let submitted = "  Exact submitted draft e\u{301} 😀\n";
    insert_owned(&editor, submitted)?;
    shell.instructions("global", true, false)?;
    // Editing after submission must not be acknowledged by that operation's Saved.
    let later = "  Later unsaved draft ação 🦀\n";
    insert_owned(&editor, later)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !shell.pending_settings.is_empty() {
        pump(shell, deadline)?;
    }
    assert_record(
        receipt,
        "receipts.saved_does_not_reset_later_edit",
        shell.config().custom_instructions == submitted
            && string(&editor) == later
            && shell.settings.drafts().get("global").map(String::as_str) == Some(later),
        json!({"saved_matches_submission":shell.config().custom_instructions==submitted,
            "native_draft_preserved":string(&editor)==later}),
    )?;

    let baseline = shell.config();
    let mut concurrent = baseline.clone();
    concurrent.trailing_space = !concurrent.trailing_space;
    let operation = shell
        .controller
        .borrow()
        .apply_settings(SettingsDraft::from_configs(&baseline, &concurrent)?, true)?;
    let external = loop {
        pump(shell, deadline)?;
        if let Some(receipt) = operation.try_recv()? {
            break receipt;
        }
    };
    ensure!(
        matches!(external, ConfigApplyReceipt::Saved { .. }),
        "External preference did not receive Saved"
    );
    settle(shell)?;
    assert_record(
        receipt,
        "receipts.unrelated_saved_preserves_instruction_draft",
        string(&editor) == later && shell.config().trailing_space == concurrent.trailing_space,
        json!({"native_draft_preserved":string(&editor)==later,"external_field_refreshed":shell.config().trailing_space==concurrent.trailing_space}),
    )?;

    // The private profile was created by this CI-only runner. Block only its
    // settings destination, restore it even when the assertion fails.
    let path = crate::config::config_path()?;
    let backup = path.with_extension("native-smoke-backup");
    ensure!(
        !backup.exists(),
        "Refusing an existing smoke settings backup"
    );
    std::fs::rename(&path, &backup)?;
    let failure_case = (|| -> Result<bool> {
        std::fs::create_dir(&path)?;
        let unchanged = shell.config();
        let pending = shell
            .controller
            .borrow()
            .apply_settings(SettingsDraft::from_configs(&unchanged, &unchanged)?, true)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let outcome = loop {
            pump(shell, deadline)?;
            if let Some(outcome) = pending.try_recv()? {
                break outcome;
            }
        };
        let failed =
            matches!(outcome, ConfigApplyReceipt::Failed { .. }) && shell.config() == unchanged;
        let guard = ModalGuard::arm("BroTypist couldn’t complete that change", "OK", true)?;
        shell.instructions("global", true, false)?;
        while !shell.pending_settings.is_empty() {
            ensure!(
                Instant::now() < deadline,
                "Native failed-save receipt timed out"
            );
            settle(shell)?;
        }
        guard.finish()?;
        Ok(failed
            && shell.config() == unchanged
            && string(&editor) == later
            && shell.settings.drafts().get("global").map(String::as_str) == Some(later))
    })();
    if path.is_dir() {
        std::fs::remove_dir(&path)?;
    }
    std::fs::rename(&backup, &path)?;
    assert_record(
        receipt,
        "receipts.explicit_save_failure_preserves_config_and_native_draft",
        failure_case?,
        json!({"destination":"owned private profile","unchanged_save_failed":true,
            "native_failed_save_draft_preserved":string(&editor)==later}),
    )?;
    action(shell, "inklow.personalization.global.revertInstructions")?;

    let before_model = shell.config();
    shell.begin_model(
        before_model.model_path.clone(),
        before_model.prompt_style.clone(),
        None,
    )?;
    shell.refresh_state()?;
    navigate(shell, "models")?;
    let cancellation = control(shell, "inklow.models.cancelVerification")?;
    press(&shell.kit, &cancellation)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !shell.pending_settings.is_empty() {
        pump(shell, deadline)?;
    }
    assert_record(
        receipt,
        "receipts.native_verification_cancel_preserves_saved_model",
        shell.config() == before_model
            && shell.status["settings_operation"]
                .as_str()
                .is_some_and(|s| s.starts_with("Change cancelled")),
        json!({"captured_operation_cancelled":true,"saved_config_preserved":shell.config()==before_model}),
    )?;
    navigate(shell, "personalization")?;
    let retained_editor = control(shell, "inklow.personalization.globalInstructions")?;
    let unsaved = "  Survives supersession e\u{301} 😀\n";
    insert_owned(&retained_editor, unsaved)?;
    let first = shell.controller.borrow().select_model(
        before_model.model_path.clone(),
        before_model.prompt_style.clone(),
        None,
    )?;
    let replacement = shell.controller.borrow().select_model(
        before_model.model_path.clone(),
        before_model.prompt_style.clone(),
        None,
    )?;
    ensure!(
        first.operation_id() != replacement.operation_id(),
        "Operation identities were reused"
    );
    shell
        .controller
        .borrow()
        .cancel_config_apply(first.operation_id())?;
    shell
        .controller
        .borrow()
        .cancel_config_apply(replacement.operation_id())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut first_receipt = None;
    let mut replacement_receipt = None;
    while first_receipt.is_none() || replacement_receipt.is_none() {
        pump(shell, deadline)?;
        if first_receipt.is_none() {
            first_receipt = first.try_recv()?;
        }
        if replacement_receipt.is_none() {
            replacement_receipt = replacement.try_recv()?;
        }
    }
    assert_record(
        receipt,
        "receipts.supersession_and_cancellation_preserve_unsaved_native_draft",
        matches!(first_receipt, Some(ConfigApplyReceipt::Superseded))
            && matches!(replacement_receipt, Some(ConfigApplyReceipt::Cancelled))
            && shell.config() == before_model
            && string(&retained_editor) == unsaved,
        json!({"distinct_operations":true,"saved_config_preserved":shell.config()==before_model,
            "native_draft_preserved":string(&retained_editor)==unsaved}),
    )?;
    action(shell, "inklow.personalization.global.revertInstructions")?;

    navigate(shell, "statistics")?;
    ensure!(
        is_enabled(&*control(shell, "inklow.statistics.exportDiagnostics")?),
        "Explicit diagnostics export must be available"
    );
    let exported = shell.controller.borrow().export_diagnostics()?;
    let diagnostic: Value = serde_json::from_slice(&std::fs::read(&exported)?)?;
    assert_record(
        receipt,
        "diagnostics.typed_private_export",
        diagnostic["version"] == 2 && diagnostic["traces"].is_array(),
        json!({"version":diagnostic["version"],"explicit_local_export":true}),
    )?;
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
        if offered.is_none() {
            receipt["preview_failure"] = json!({
                "language": language,
                "welcome": shell.welcome.smoke_observation(),
                "controller_state": shell.status["state"],
                "controller_preview": shell.status["preview"],
                "controller_preview_suspended": shell.status["preview_suspended"],
            });
            snapshot(
                shell,
                receipt,
                output,
                "inklow.welcome",
                &format!("preview-{language}-deadline"),
            )?;
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
    let guard = ModalGuard::arm("Unsaved settings", "Discard Changes", false)?;
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
        native_wake_cycles(&mut shell, &mut receipt)?;
        sidebar_press_contract(&mut shell, &mut receipt)?;
        navigate_panes(&mut shell, &mut receipt, output)?;
        drafts(&mut shell, &mut receipt, output)?;
        privacy(&mut shell, &mut receipt, output)?;
        settings_operations(&mut shell, &mut receipt)?;
        welcome(&mut shell, &mut receipt, output)?;
        // The full shell already owns its model; this stage uses the controlled
        // provider and retains partial evidence even when acceptance fails.
        let acceptance = capture_owned_acceptance(
            &shell.app,
            &shell.kit,
            output,
            None,
            &mut receipt["owned_choice_evidence"],
        );
        receipt["owned_choice_acceptance"] = acceptance?;
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
    fn owned_matrix_continuation_stops_after_retaining_an_interrupted_case() {
        for cause in [
            OwnedInterruption::PhysicalInput,
            OwnedInterruption::ApplicationFocus,
            OwnedInterruption::EditorFocus,
        ] {
            let outcomes: [Result<()>; 3] = [
                Err(anyhow::anyhow!(
                    "Actual inference returned no acceptable draw"
                )),
                Err(anyhow::Error::new(cause).context("Current owned case")),
                Ok(()),
            ];
            let mut retained = Vec::new();
            let mut recovery = Vec::new();
            let stopped: Result<()> =
                outcomes.iter().enumerate().try_for_each(|(index, result)| {
                    retained.push((index, result.is_ok()));
                    owned_matrix_continue(result)?;
                    recovery.push(index);
                    Ok(())
                });
            assert_eq!(retained, [(0, false), (1, false)]);
            assert_eq!(recovery, [0]);
            assert!(stopped.unwrap_err().is::<OwnedInterruption>());
        }
    }

    #[test]
    fn owned_matrix_continuation_keeps_ordinary_failures_without_string_matching() {
        let outcomes: [Result<()>; 4] = [
            Err(anyhow::anyhow!(
                "Actual PreviewSession returned an inference error"
            )),
            Err(anyhow::anyhow!(
                "No actual acceptable preview draw within the single 120s attempt"
            )),
            // Identical wording is not terminal without the typed fence cause.
            Err(anyhow::anyhow!(
                OwnedInterruption::PhysicalInput.to_string()
            )),
            Ok(()),
        ];
        let mut retained = Vec::new();
        let continued: Result<()> = outcomes.iter().try_for_each(|result| {
            retained.push(result.is_ok());
            owned_matrix_continue(result)
        });
        assert!(continued.is_ok());
        assert_eq!(retained, [false, false, false, true]);
    }

    #[test]
    fn owned_paths_refuse_extended_acl_before_creating_output() {
        use std::os::unix::fs::DirBuilderExt;
        let fixture = tempfile::Builder::new()
            .prefix("brotypist-path-acl-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let output = home.join("evidence");
        let normal = root.join("synthetic-normal");
        assert!(OwnedProfileOutput::validate(&home, &output, &normal).is_ok());
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow delete_child"])
            .arg(&home)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(OwnedProfileOutput::validate(&home, &output, &normal).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn owned_paths_do_not_revisit_substituted_ancestor_aliases() {
        use std::os::unix::fs::{DirBuilderExt, symlink};
        let fixture = tempfile::Builder::new()
            .prefix("brotypist-path-contract-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let real = root.join("private");
        let normal = root.join("synthetic-normal");
        let alias = root.join("alias");
        for path in [&real, &normal, &real.join("home"), &normal.join("home")] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        symlink(&real, &alias).unwrap();
        let paths = OwnedProfileOutput::validate(
            &alias.join("home"),
            &alias.join("home/evidence"),
            &normal,
        )
        .unwrap();
        // Substitute the original ancestor after validation, before any IO.
        fs::remove_file(&alias).unwrap();
        symlink(&normal, &alias).unwrap();
        paths.create_output().unwrap();
        write_new(&paths.home.join("marker"), b"synthetic").unwrap();
        write_new(&paths.output.join("receipt"), b"synthetic").unwrap();
        assert!(real.join("home/evidence/receipt").is_file());
        assert!(real.join("home/marker").is_file());
        assert!(fs::read_dir(normal.join("home")).unwrap().next().is_none());
    }

    #[test]
    fn owned_paths_refuse_nonsticky_writable_ancestors_before_io() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
        let fixture = tempfile::Builder::new()
            .prefix("brotypist-path-contract-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let writable = root.join("replaceable");
        let normal = root.join("synthetic-normal");
        for path in [
            &writable,
            &normal,
            &writable.join("home"),
            &normal.join("home"),
        ] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o777)).unwrap();
        let result = OwnedProfileOutput::validate(
            &writable.join("home"),
            &writable.join("home/evidence"),
            &normal,
        );
        assert!(
            result.is_err(),
            "A private leaf does not secure a replaceable ancestor"
        );
        fs::rename(writable.join("home"), writable.join("displaced")).unwrap();
        symlink(normal.join("home"), writable.join("home")).unwrap();
        assert!(
            OwnedProfileOutput::validate(
                &writable.join("home"),
                &writable.join("home/evidence"),
                &normal
            )
            .is_err()
        );
        assert!(fs::read_dir(normal.join("home")).unwrap().next().is_none());
        assert!(
            fs::read_dir(writable.join("displaced"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn owned_paths_refuse_normal_overlap_and_used_profiles() {
        use std::os::unix::fs::DirBuilderExt;
        let fixture = tempfile::Builder::new()
            .prefix("brotypist-path-contract-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        assert!(OwnedProfileOutput::validate(&home, &home.join("evidence"), &home).is_err());
        write_new(&home.join("used"), b"synthetic").unwrap();
        assert!(
            OwnedProfileOutput::validate(
                &home,
                &home.join("evidence"),
                &root.join("synthetic-normal")
            )
            .is_err()
        );
    }

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
