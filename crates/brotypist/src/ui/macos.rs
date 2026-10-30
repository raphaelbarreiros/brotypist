//! Main-thread-owned AppKit resident shell. Native callbacks only enqueue captured intents.
use super::LaunchOptions;
use crate::{
    config::Config,
    runtime::{Command, Controller},
    updates::UpdateService,
};
use anyhow::{Context, Result, ensure};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[path = "macos_settings.rs"]
mod macos_settings;
#[path = "macos_settings_contract.rs"]
mod macos_settings_contract;
#[cfg(feature = "native-smoke")]
#[path = "macos_smoke.rs"]
mod macos_smoke;
#[path = "macos_welcome.rs"]
mod macos_welcome;
use super::{macos_overlay, macos_power};
use macos_overlay::OverlayOwner;
use macos_power::PowerUi;
use macos_settings::Settings;
use macos_settings_contract::{Binding, ReadValue, UiAction, UiQueue};
use macos_welcome::Welcome;
pub(super) type Obj = Retained<AnyObject>;
fn activate_app(app: &AnyObject) {
    unsafe {
        let _: () = msg_send![app,activateIgnoringOtherApps:true];
    }
}
fn present_window(window: &AnyObject, activate: bool) {
    unsafe {
        let _: () = msg_send![window,makeKeyAndOrderFront:std::ptr::null::<AnyObject>()];
        if activate {
            let app: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            activate_app(&*app);
        }
    }
}
#[cfg(feature = "native-smoke")]
pub fn smoke(output: &std::path::Path) -> Result<i32> {
    objc2::rc::autoreleasepool(|_| macos_smoke::run(output))
}
/// Render canonical Settings without starting services, acquiring input, or activating the app.
#[cfg(feature = "native-smoke")]
pub fn capture_layout(output: &std::path::Path) -> Result<i32> {
    objc2::rc::autoreleasepool(|_| macos_smoke::capture_layout(output))
}
pub(super) fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
pub(super) fn obj(name: &str) -> Obj {
    let name = std::ffi::CString::new(name).expect("native class name");
    let class = AnyClass::get(&name).expect("AppKit class");
    unsafe {
        let p: *mut AnyObject = msg_send![class, new];
        Retained::from_raw(p).expect("AppKit allocation")
    }
}
pub(super) fn add(parent: &AnyObject, child: &AnyObject) {
    unsafe {
        let _: () = msg_send![parent,addSubview:child];
    }
}
pub(super) fn frame(object: &AnyObject, value: &NSRect) {
    unsafe {
        let _: () = msg_send![object,setFrame:*value];
    }
}
pub(super) fn string(object: &AnyObject) -> String {
    unsafe {
        let responds: bool = msg_send![object,respondsToSelector:sel!(string)];
        let s: *mut NSString = if responds {
            msg_send![object, string]
        } else {
            msg_send![object, stringValue]
        };
        if s.is_null() {
            String::new()
        } else {
            (&*s).to_string()
        }
    }
}
pub(super) fn set_string(object: &AnyObject, value: &str) {
    unsafe {
        let s = NSString::from_str(value);
        let responds: bool = msg_send![object,respondsToSelector:sel!(setString:)];
        if responds {
            let _: () = msg_send![object,setString:&*s];
        } else {
            let _: () = msg_send![object,setStringValue:&*s];
        }
    }
}
pub(super) fn identify(object: &AnyObject, id: &str, label: &str) {
    unsafe {
        let id = NSString::from_str(id);
        let label = NSString::from_str(label);
        let _: () = msg_send![object,setIdentifier:&*id];
        let supports: bool = msg_send![object,respondsToSelector:sel!(setAccessibilityIdentifier:)];
        if supports {
            let _: () = msg_send![object,setAccessibilityIdentifier:&*id];
        }
        let supports: bool = msg_send![object,respondsToSelector:sel!(setAccessibilityLabel:)];
        if supports {
            let _: () = msg_send![object,setAccessibilityLabel:&*label];
        }
    }
}
fn native_title(object: &AnyObject, title: &str) {
    unsafe {
        let _: () = msg_send![object,setTitle:&*NSString::from_str(title)];
    }
}
fn enabled(object: &AnyObject, value: bool) {
    unsafe {
        let _: () = msg_send![object,setEnabled:value];
    }
}
fn value_action(mut action: UiAction, value: Value) -> UiAction {
    match &mut action {
        UiAction::Set(_, v) => *v = value,
        UiAction::Rule(_, _, v) => {
            *v = if value == "inherit" || value == "auto" {
                None
            } else {
                Some(match value.as_str() {
                    Some("yes" | "on") => json!(true),
                    Some("no" | "off") => json!(false),
                    _ => value,
                })
            }
        }
        UiAction::ModelSelect(v) | UiAction::FilterRules(v) | UiAction::PreviewLanguage(v) => {
            *v = value.as_str().unwrap_or("").to_owned()
        }
        UiAction::ModelAdvanced(v) => *v = value.as_bool().unwrap_or(false),
        UiAction::Statistics {
            period,
            group,
            metric,
        } => {
            let s = value.as_str().unwrap_or("").to_owned();
            if period.is_some() {
                *period = Some(s)
            } else if group.is_some() {
                *group = Some(s)
            } else {
                *metric = Some(s)
            }
        }
        _ => {}
    }
    action
}
struct DelegateIvars {
    queue: Rc<RefCell<UiQueue>>,
    bindings: Rc<RefCell<BTreeMap<isize, Binding>>>,
    next: Cell<isize>,
    no_tray: Cell<bool>,
    suspend: RefCell<Option<Box<dyn Fn(bool)>>>,
    menu_notice: RefCell<Option<Box<dyn Fn(bool)>>>,
}
define_class!(
 #[unsafe(super=NSObject)] #[thread_kind=MainThreadOnly] #[ivars=DelegateIvars]
 struct NativeDelegate;
 unsafe impl NSObjectProtocol for NativeDelegate {}
 impl NativeDelegate {
  #[unsafe(method(perform:))]
  fn perform(&self,sender:&AnyObject){
   let tag:isize=unsafe{msg_send![sender,tag]};
   let binding=self.ivars().bindings.borrow().get(&tag).cloned();
   if let Some(binding)=binding{
    let value=unsafe{match binding.read{
     ReadValue::None=>None,
     ReadValue::Boolean=>{let state:isize=msg_send![sender,state];Some(json!(state!=0))},
     ReadValue::Text=>Some(json!(string(sender))),
     ReadValue::Integer=>{let text=string(sender);text.parse::<u32>().ok().map(|n|json!(n)).or(Some(json!(text)))},
     ReadValue::Slider=>{let value:f64=msg_send![sender,doubleValue];if value.is_finite()&&value>=0.&&value<=u32::MAX as f64{Some(json!(value.round() as u32))}else{return}},
     ReadValue::Choice(values)=>{let index:isize=msg_send![sender,indexOfSelectedItem];usize::try_from(index).ok().and_then(|i|values.get(i).cloned())}
    }};
    self.ivars().queue.borrow_mut().pending.push_back(if let Some(value)=value{value_action(binding.action,value)}else{binding.action});
   }
  }
  #[unsafe(method(menuWillOpen:))] fn menu_open(&self,_menu:&AnyObject){let first={let mut queue=self.ivars().queue.borrow_mut();queue.tracking+=1;queue.tracking==1};if first{if let Some(suspend)=self.ivars().suspend.borrow().as_ref(){suspend(true);}if let Some(notice)=self.ivars().menu_notice.borrow().as_ref(){notice(true);}}}
  #[unsafe(method(menuDidClose:))] fn menu_close(&self,_menu:&AnyObject){let remaining={let mut q=self.ivars().queue.borrow_mut();q.tracking=q.tracking.saturating_sub(1);if q.tracking==0{q.resume_shortcuts=true;}q.tracking};if remaining==0{if let Some(notice)=self.ivars().menu_notice.borrow().as_ref(){notice(false);}}}
  #[unsafe(method(windowDidResize:))] fn resized(&self,_notification:&AnyObject){self.ivars().queue.borrow_mut().pending.push_back(UiAction::Resized);}
  #[unsafe(method(windowDidResignKey:))] fn resigned(&self,notification:&AnyObject){unsafe{let window:*mut AnyObject=msg_send![notification,object];let id:*mut NSString=msg_send![window,identifier];if !id.is_null(){self.ivars().queue.borrow_mut().pending.push_back(UiAction::Deactivated((&*id).to_string()));}}}
  #[unsafe(method(windowWillClose:))] fn closed(&self,notification:&AnyObject){unsafe{let window:*mut AnyObject=msg_send![notification,object];let id:*mut NSString=msg_send![window,identifier];if !id.is_null(){self.ivars().queue.borrow_mut().pending.push_back(UiAction::Closed((&*id).to_string()));}}}
  #[unsafe(method(windowShouldClose:))] fn should_close(&self,_window:&AnyObject)->bool{if self.ivars().no_tray.get(){self.ivars().queue.borrow_mut().pending.push_back(UiAction::Quit);false}else{true}}
  #[unsafe(method(applicationShouldTerminate:))] fn terminate(&self,_app:&AnyObject)->usize{self.ivars().queue.borrow_mut().pending.push_back(UiAction::Quit);0}
  #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))] fn after_close(&self,_app:&AnyObject)->bool{false}
  #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))] fn reopen(&self,_app:&AnyObject,visible:bool)->bool{if !visible{self.ivars().queue.borrow_mut().pending.push_back(UiAction::Reopened);}true}
  #[unsafe(method(application:openURLs:))] fn urls(&self,_app:&AnyObject,urls:&AnyObject){unsafe{let n:usize=msg_send![urls,count];for i in 0..n{let url:*mut AnyObject=msg_send![urls,objectAtIndex:i];let text:*mut NSString=msg_send![url,absoluteString];if !text.is_null(){if let Some(pane)=url_target(&(&*text).to_string()){self.ivars().queue.borrow_mut().pending.push_back(if pane=="welcome"{UiAction::Welcome}else{UiAction::Navigate(pane)});}}}}}
 }
);
define_class!(#[unsafe(super=objc2_app_kit::NSView)] #[thread_kind=MainThreadOnly] pub(super) struct FlippedView;impl FlippedView{#[unsafe(method(isFlipped))] fn flipped(&self)->bool{true}});
#[derive(Clone)]
struct Kit {
    target: Retained<NativeDelegate>,
    queue: Rc<RefCell<UiQueue>>,
}
impl Kit {
    fn new(mtm: MainThreadMarker, no_tray: bool) -> Self {
        let queue = Rc::new(RefCell::new(UiQueue::default()));
        let this = NativeDelegate::alloc(mtm).set_ivars(DelegateIvars {
            queue: queue.clone(),
            bindings: Rc::new(RefCell::new(BTreeMap::new())),
            next: Cell::new(0),
            no_tray: Cell::new(no_tray),
            suspend: RefCell::new(None),
            menu_notice: RefCell::new(None),
        });
        let target = unsafe { msg_send![super(this), init] };
        Self { target, queue }
    }
    pub(super) fn rebind(&self, control: &AnyObject, action: UiAction, read: ReadValue) {
        let tag: isize = unsafe { msg_send![control, tag] };
        let tag = if tag > 0 && self.target.ivars().bindings.borrow().contains_key(&tag) {
            tag
        } else {
            let n = self.target.ivars().next.get() + 1;
            self.target.ivars().next.set(n);
            n
        };
        unsafe {
            let _: () = msg_send![control,setTag:tag];
            let _: () = msg_send![control,setTarget:&*self.target];
            let _: () = msg_send![control,setAction:sel!(perform:)];
        }
        self.target
            .ivars()
            .bindings
            .borrow_mut()
            .insert(tag, Binding { action, read });
    }
    pub(super) fn release_bindings(&self, root: &AnyObject) {
        unsafe {
            let tagged: bool = msg_send![root,respondsToSelector:sel!(tag)];
            if tagged {
                let tag: isize = msg_send![root, tag];
                self.target.ivars().bindings.borrow_mut().remove(&tag);
            }
            let supports: bool = msg_send![root,respondsToSelector:sel!(subviews)];
            if supports {
                let children: *mut AnyObject = msg_send![root, subviews];
                let count: usize = msg_send![children, count];
                for i in 0..count {
                    let child: *mut AnyObject = msg_send![children,objectAtIndex:i];
                    self.release_bindings(&*child);
                }
            }
        }
    }
    pub(super) fn view(&self, frame: NSRect) -> Obj {
        let view: Retained<FlippedView> =
            unsafe { msg_send![FlippedView::alloc(self.target.mtm()),initWithFrame:frame] };
        // Erasing the static subclass preserves the same initialized object's +1 ownership.
        unsafe { Retained::cast_unchecked(view) }
    }
    pub(super) fn label(&self, text: &str, frame: NSRect, size: f64, bold: bool) -> Obj {
        unsafe {
            let class = AnyClass::get(c"NSTextField").unwrap();
            let raw: *mut AnyObject =
                msg_send![class,wrappingLabelWithString:&*NSString::from_str(text)];
            let result = Retained::retain(raw).unwrap();
            super_frame_font(&result, frame, size, bold);
            result
        }
    }
    pub(super) fn button(&self, title: &str, id: &str, frame: NSRect, action: UiAction) -> Obj {
        let result = obj("NSButton");
        native_title(&result, title);
        unsafe {
            let _: () = msg_send![&*result,setBezelStyle:1usize];
        }
        self.rebind(&result, action, ReadValue::None);
        super_frame(&result, frame);
        identify(&result, id, title);
        result
    }
    pub(super) fn checkbox(
        &self,
        title: &str,
        id: &str,
        frame: NSRect,
        value: bool,
        action: UiAction,
    ) -> Obj {
        let result = self.button(title, id, frame, action.clone());
        unsafe {
            let _: () = msg_send![&*result,setButtonType:3usize];
            let _: () = msg_send![&*result,setState:isize::from(value)];
        }
        self.rebind(&result, action, ReadValue::Boolean);
        result
    }
    pub(super) fn popup(
        &self,
        id: &str,
        label: &str,
        frame: NSRect,
        options: &[(String, Value)],
        selected: &Value,
        action: UiAction,
    ) -> Obj {
        let result = obj("NSPopUpButton");
        unsafe {
            for (title, _) in options {
                let _: () = msg_send![&*result,addItemWithTitle:&*NSString::from_str(title)];
            }
            if let Some(i) = options.iter().position(|(_, v)| v == selected) {
                let _: () = msg_send![&*result,selectItemAtIndex:i as isize];
            }
        }
        self.rebind(
            &result,
            action,
            ReadValue::Choice(options.iter().map(|(_, v)| v.clone()).collect()),
        );
        super_frame(&result, frame);
        identify(&result, id, label);
        result
    }
    pub(super) fn text(
        &self,
        id: &str,
        label: &str,
        frame: NSRect,
        value: &str,
        integer: bool,
        action: UiAction,
    ) -> Obj {
        let result = obj("NSTextField");
        set_string(&result, value);
        self.rebind(
            &result,
            action,
            if integer {
                ReadValue::Integer
            } else {
                ReadValue::Text
            },
        );
        super_frame(&result, frame);
        identify(&result, id, label);
        result
    }
    pub(super) fn editor(&self, id: &str, label: &str, frame: NSRect, value: &str) -> Obj {
        let result = obj("NSTextView");
        super_frame_font(&result, frame, 13.0, false);
        unsafe {
            let _: () = msg_send![&*result,setRichText:false];
            let _: () = msg_send![&*result,setAllowsUndo:true];
            let _: () = msg_send![&*result,setVerticallyResizable:true];
            let _: () = msg_send![&*result,setHorizontallyResizable:false];
            let _: () = msg_send![&*result,setAutoresizingMask:2usize];
            let _: () = msg_send![&*result,setTextContainerInset:NSSize::new(9.0,8.0)];
            let container: *mut AnyObject = msg_send![&*result, textContainer];
            let _: () = msg_send![container,setWidthTracksTextView:true];
        }
        set_string(&result, value);
        identify(&result, id, label);
        result
    }
    pub(super) fn progress(&self, id: &str, frame: NSRect) -> Obj {
        let result = obj("NSProgressIndicator");
        unsafe {
            let _: () = msg_send![&*result,setStyle:0usize];
            let _: () = msg_send![&*result,setIndeterminate:false];
            let _: () = msg_send![&*result,setMinValue:0.0f64];
            let _: () = msg_send![&*result,setMaxValue:1.0f64];
        }
        super_frame(&result, frame);
        identify(&result, id, "Download progress");
        result
    }
    pub(super) fn window(
        &self,
        title: &str,
        id: &str,
        width: f64,
        height: f64,
        resizable: bool,
    ) -> Obj {
        unsafe {
            let class = AnyClass::get(c"NSWindow").unwrap();
            let raw: *mut AnyObject = msg_send![class, alloc];
            let raw: *mut AnyObject = msg_send![raw,initWithContentRect:rect(0.0,0.0,width,height),styleMask:if resizable{15usize|32768}else{3usize|32768},backing:2usize,defer:false];
            let result = Retained::from_raw(raw).unwrap();
            native_title(&result, title);
            identify(&result, id, title);
            let _: () = msg_send![&*result,setReleasedWhenClosed:false];
            let _: () = msg_send![&*result,setDelegate:&*self.target];
            let _: () = msg_send![&*result,setTitleVisibility:1isize];
            let _: () = msg_send![&*result,setTitlebarAppearsTransparent:true];
            let _: () = msg_send![&*result,setAutorecalculatesKeyViewLoop:true];
            let _: () = msg_send![&*result, center];
            result
        }
    }
}
fn super_frame(object: &AnyObject, value: NSRect) {
    frame(object, &value)
}
fn super_frame_font(object: &AnyObject, value: NSRect, size: f64, bold: bool) {
    super_frame(object, value);
    unsafe {
        let class = AnyClass::get(c"NSFont").unwrap();
        let font: *mut AnyObject =
            msg_send![class,systemFontOfSize:size,weight:if bold{0.23f64}else{0.0f64}];
        let _: () = msg_send![object,setFont:font];
    }
}
fn url_target(text: &str) -> Option<String> {
    let url = url::Url::parse(text).ok()?;
    if !url.scheme().eq_ignore_ascii_case("brotypist") {
        return None;
    }
    let tail = url
        .path_segments()
        .and_then(|s| s.filter(|s| !s.is_empty()).last())
        .or(url.host_str())
        .unwrap_or("general");
    Some(canonical_pane(tail).unwrap_or("general").to_owned())
}
fn canonical_pane(value: &str) -> Option<&'static str> {
    [
        "setup",
        "general",
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
        "welcome",
    ]
    .into_iter()
    .find(|p| p.eq_ignore_ascii_case(value))
}

fn alert(title: &str, message: &str, buttons: &[&str]) -> isize {
    let alert = obj("NSAlert");
    unsafe {
        let _: () = msg_send![&*alert,setMessageText:&*NSString::from_str(title)];
        let _: () = msg_send![&*alert,setInformativeText:&*NSString::from_str(message)];
        for title in buttons {
            let _: *mut AnyObject =
                msg_send![&*alert,addButtonWithTitle:&*NSString::from_str(title)];
        }
        msg_send![&*alert, runModal]
    }
}
fn confirm(title: &str, message: &str) -> bool {
    alert(title, message, &["Continue", "Cancel"]) == 1000
}
fn history_consent() -> bool {
    confirm(
        "Enable encrypted local learning?",
        "Eligible writing will be stored encrypted on this Mac. New observed edits can teach local style and vocabulary; existing drafts, bulk pastes and BroTypist-generated suggestions do not establish authorship. Your OS secure storage must be available. Nothing is uploaded and there is no plaintext fallback.",
    )
}
fn context_consent(field: &str) -> bool {
    let (title, body) = match field {
        "screen_context" => (
            "Use focused-window text for context?",
            "BroTypist will read readable Accessibility text from the verified focused window only for eligible suggestion requests. Protected content and excluded apps are skipped. Context is not saved or sent to a remote service.",
        ),
        "clipboard_context" => (
            "Use the clipboard for context?",
            "BroTypist will read clipboard text only for eligible suggestion requests. Context is not saved or sent to a remote service. Protected content and excluded apps are skipped.",
        ),
        "screenshot_context" => (
            "Use screenshots for context?",
            "For eligible suggestion requests, BroTypist captures the focused window and recognizes text on this Mac. Images are processed in memory, discarded immediately, never saved or uploaded. Accessibility text is tried first. macOS requires Screen & System Audio Recording permission and may show its recording indicator.",
        ),
        "screenshot_appearance" => (
            "Use screenshots to improve suggestion appearance?",
            "BroTypist captures the focused window on this Mac and crops it in memory to measure the font, size and baseline around the caret. Images are discarded immediately, never saved or uploaded. Suggestions stay hidden when measurement cannot be verified. Screen & System Audio Recording permission is separate.",
        ),
        _ => return true,
    };
    confirm(title, body)
}
fn open_url(text: &str) -> Result<()> {
    unsafe {
        let workspace: *mut AnyObject =
            msg_send![AnyClass::get(c"NSWorkspace").unwrap(), sharedWorkspace];
        let url: *mut AnyObject =
            msg_send![AnyClass::get(c"NSURL").unwrap(),URLWithString:&*NSString::from_str(text)];
        ensure!(!url.is_null(), "The link is invalid.");
        let opened: bool = msg_send![workspace,openURL:url];
        ensure!(opened, "macOS could not open this link.");
    }
    Ok(())
}
fn app_name(id: &str) -> String {
    unsafe {
        let workspace: *mut AnyObject =
            msg_send![AnyClass::get(c"NSWorkspace").unwrap(), sharedWorkspace];
        let url: *mut AnyObject =
            msg_send![workspace,URLForApplicationWithBundleIdentifier:&*NSString::from_str(id)];
        if !url.is_null() {
            let path: *mut NSString = msg_send![url, path];
            if !path.is_null() {
                let component: *mut NSString = msg_send![path, lastPathComponent];
                if !component.is_null() {
                    return (&*component)
                        .to_string()
                        .trim_end_matches(".app")
                        .to_owned();
                }
            }
        }
    }
    id.to_owned()
}
fn menu(kit: &Kit, title: &str) -> Obj {
    let result = obj("NSMenu");
    native_title(&result, title);
    unsafe {
        let _: () = msg_send![&*result,setAutoenablesItems:false];
        let _: () = msg_send![&*result,setDelegate:&*kit.target];
    }
    result
}
fn item(
    kit: &Kit,
    menu: &AnyObject,
    title: &str,
    id: &str,
    key: &str,
    action: Option<UiAction>,
) -> Obj {
    let result = obj("NSMenuItem");
    native_title(&result, title);
    unsafe {
        let _: () = msg_send![&*result,setKeyEquivalent:&*NSString::from_str(key)];
        let _: () = msg_send![&*result,setIdentifier:&*NSString::from_str(id)];
        if let Some(action) = action {
            kit.rebind(&result, action, ReadValue::None);
        } else {
            enabled(&result, id.ends_with("Menu"));
        }
        let _: () = msg_send![menu,addItem:&*result];
    }
    result
}
fn separator(menu: &AnyObject) {
    unsafe {
        let s: *mut AnyObject = msg_send![AnyClass::get(c"NSMenuItem").unwrap(), separatorItem];
        let _: () = msg_send![menu,addItem:s];
    }
}
fn main_menu(kit: &Kit, app: &AnyObject) -> Obj {
    let main = menu(kit, "BroTypist");
    let root = item(kit, &main, "BroTypist", "inklow.appMenu", "", None);
    let app_menu = menu(kit, "BroTypist");
    item(
        kit,
        &app_menu,
        "Settings…",
        "inklow.main.settings",
        ",",
        Some(UiAction::Navigate("general".into())),
    );
    separator(&app_menu);
    item(
        kit,
        &app_menu,
        "Quit BroTypist",
        "inklow.main.quit",
        "q",
        Some(UiAction::Quit),
    );
    let close = item(
        kit,
        &app_menu,
        "Close Window",
        "inklow.main.close",
        "w",
        None,
    );
    unsafe {
        enabled(&close, true);
        let _: () = msg_send![&*close,setAction:sel!(performClose:)];
        let _: () = msg_send![&*root,setSubmenu:&*app_menu];
    }
    let edit = item(kit, &main, "Edit", "inklow.editMenu", "", None);
    let edit_menu = menu(kit, "Edit");
    unsafe {
        let _: () = msg_send![&*edit_menu,setAutoenablesItems:true];
        for (title, key, selector) in [
            ("Undo", "z", sel!(undo:)),
            ("Redo", "Z", sel!(redo:)),
            ("Cut", "x", sel!(cut:)),
            ("Copy", "c", sel!(copy:)),
            ("Paste", "v", sel!(paste:)),
            ("Select All", "a", sel!(selectAll:)),
        ] {
            let control = item(
                kit,
                &edit_menu,
                title,
                &format!("inklow.edit.{key}"),
                key,
                None,
            );
            enabled(&control, true);
            let _: () = msg_send![&*control,setAction:selector];
        }
        let _: () = msg_send![&*edit,setSubmenu:&*edit_menu];
    }
    let navigation = item(kit, &main, "View", "inklow.viewMenu", "", None);
    let view_menu = menu(kit, "View");
    for (i, (pane, title)) in [
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
    .iter()
    .enumerate()
    {
        item(
            kit,
            &view_menu,
            title,
            &format!("inklow.navigation.{pane}"),
            &if i < 9 {
                (i + 1).to_string()
            } else {
                String::new()
            },
            Some(UiAction::Navigate((*pane).into())),
        );
    }
    unsafe {
        let _: () = msg_send![&*navigation,setSubmenu:&*view_menu];
        let _: () = msg_send![app,setMainMenu:&*main];
    }
    main
}
struct Menus {
    resident: Obj,
    field: Obj,
    toggle: Obj,
    pause: Obj,
    pause_hour: Obj,
    pause_tomorrow: Obj,
    resume: Obj,
    current: Obj,
    record: Obj,
    tab: Obj,
    docs: Obj,
    update: Obj,
}
impl Menus {
    fn new(kit: &Kit) -> Self {
        let resident = menu(kit, "BroTypist");
        let toggle = item(
            kit,
            &resident,
            "Enable predictions",
            "inklow.menu.toggle",
            "",
            Some(UiAction::Set("global_enabled".into(), json!(true))),
        );
        let pause = item(kit, &resident, "Pause", "inklow.menu.pause", "", None);
        let pause_menu = menu(kit, "Pause");
        let pause_hour = item(
            kit,
            &pause_menu,
            "For 1 hour",
            "inklow.menu.pause.60",
            "",
            Some(UiAction::Pause(60)),
        );
        let pause_tomorrow = item(
            kit,
            &pause_menu,
            "Until tomorrow",
            "inklow.menu.pause.tomorrow",
            "",
            Some(UiAction::PauseTomorrow),
        );
        let resume = item(
            kit,
            &pause_menu,
            "Resume now",
            "inklow.menu.resume",
            "",
            Some(UiAction::Resume),
        );
        unsafe {
            enabled(&pause, true);
            let _: () = msg_send![&*pause,setSubmenu:&*pause_menu];
        }
        let current = item(
            kit,
            &resident,
            "Predictions in Current App",
            "inklow.menu.currentApp",
            "",
            Some(UiAction::ToggleApp {
                app: String::new(),
                domain: String::new(),
            }),
        );
        separator(&resident);
        item(
            kit,
            &resident,
            "Settings…",
            "inklow.menu.settings",
            ",",
            Some(UiAction::Navigate("general".into())),
        );
        let update = item(
            kit,
            &resident,
            "Check for Updates…",
            "inklow.menu.checkUpdates",
            "",
            Some(UiAction::UpdateCheck),
        );
        separator(&resident);
        item(
            kit,
            &resident,
            "Quit BroTypist",
            "inklow.menu.quit",
            "q",
            Some(UiAction::Quit),
        );
        let field = menu(kit, "");
        let app = item(
            kit,
            &field,
            "Completions in Current App",
            "inklow.field.currentApp",
            "",
            Some(UiAction::ToggleApp {
                app: String::new(),
                domain: String::new(),
            }),
        );
        let record = item(
            kit,
            &field,
            "Record Writing",
            "inklow.field.record",
            "",
            None,
        );
        let tab = item(kit, &field, "Accept with Tab", "inklow.field.tab", "", None);
        let docs = item(
            kit,
            &field,
            "Google Docs accessibility setup…",
            "inklow.field.docsAccessibility",
            "",
            Some(UiAction::Docs(None)),
        );
        separator(&field);
        item(
            kit,
            &field,
            "Settings…",
            "inklow.field.settings",
            "",
            Some(UiAction::Navigate("general".into())),
        );
        kit.rebind(
            &app,
            UiAction::ToggleApp {
                app: String::new(),
                domain: String::new(),
            },
            ReadValue::None,
        );
        Self {
            resident,
            field,
            toggle,
            pause,
            current,
            pause_hour,
            pause_tomorrow,
            resume,
            record,
            tab,
            docs,
            update,
        }
    }
    fn refresh(
        &self,
        kit: &Kit,
        config: &Config,
        status: &Value,
        updates: &Value,
        docs: Option<&str>,
    ) {
        let app = status["current_app"].as_str().unwrap_or("");
        let domain = status["current_domain"].as_str().unwrap_or("");
        let name = if app.is_empty() {
            "Current App".into()
        } else {
            menu_scope_name(&app_name(app), 24)
        };
        let scoped = if domain.is_empty() {
            name.clone()
        } else {
            domain.into()
        };
        let scope = scope_key(app, domain);
        kit.rebind(
            &self.toggle,
            UiAction::Set("global_enabled".into(), json!(!config.global_enabled)),
            ReadValue::None,
        );
        unsafe {
            let _: () = msg_send![&*self.toggle,setState:isize::from(config.global_enabled)];
        }
        let paused = status["paused_remaining"].as_f64().unwrap_or(0.) > 0.;
        enabled(&self.pause_hour, config.global_enabled);
        enabled(&self.pause_tomorrow, config.global_enabled);
        unsafe {
            let _: () = msg_send![&*self.resume,setHidden:!paused];
        }
        enabled(&self.pause, config.global_enabled || paused);
        native_title(&self.current, &format!("Predictions in {name}"));
        kit.rebind(
            &self.current,
            UiAction::ToggleApp {
                app: app.into(),
                domain: domain.into(),
            },
            ReadValue::None,
        );
        enabled(&self.current, !app.is_empty());
        unsafe {
            let _: () = msg_send![&*self.current,setState:isize::from(!app.is_empty() && status["current_app_enabled"].as_bool().unwrap_or(config.enabled))];
            let current: *mut AnyObject = msg_send![&*self.field,itemAtIndex:0isize];
            native_title(&*current, &format!("Completions in {name}"));
            enabled(&*current, !app.is_empty());
            let _: () = msg_send![current,setState:isize::from(status["current_app_enabled"].as_bool().unwrap_or(config.enabled))];
            kit.rebind(
                &*current,
                UiAction::ToggleApp {
                    app: app.into(),
                    domain: domain.into(),
                },
                ReadValue::None,
            );
        }
        for (control, field, title, state) in [
            (
                &self.record,
                "record_history",
                "Record Writing",
                status["current_recording"].as_bool().unwrap_or(false),
            ),
            (
                &self.tab,
                "tab_enabled",
                "Accept with Tab",
                status["current_tab_enabled"].as_bool().unwrap_or(true),
            ),
        ] {
            native_title(control, &format!("{title} in {scoped}"));
            enabled(control, scope.is_some());
            unsafe {
                let _: () = msg_send![&**control,setState:isize::from(state)];
            }
            if let Some(scope) = &scope {
                kit.rebind(
                    control,
                    UiAction::Rule(scope.clone(), field.into(), Some(json!(!state))),
                    ReadValue::None,
                );
            }
        }
        unsafe {
            let _: () = msg_send![&*self.docs,setHidden:docs.is_none()];
        }
        enabled(&self.docs, docs.is_some());
        kit.rebind(
            &self.docs,
            UiAction::Docs(docs.map(str::to_owned)),
            ReadValue::None,
        );
        enabled(
            &self.update,
            !matches!(
                updates["state"].as_str(),
                Some("checking" | "downloading" | "installing")
            ),
        );
    }
}
fn menu_scope_name(name: &str, limit: usize) -> String {
    let mut chars = name.chars();
    let mut display: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        display.pop();
        display.push('…');
    }
    display
}
fn scope_key(app: &str, domain: &str) -> Option<String> {
    if app.is_empty() {
        None
    } else if domain.is_empty() {
        Some(format!("app:{app}"))
    } else {
        crate::config::normalize_domain(domain)
            .ok()
            .map(|s| format!("domain:{s}"))
    }
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightListenEventAccess() -> u8;
}
fn permission_status() -> Value {
    json!({"accessibility":crate::platform::macos::ax::trusted(),"input_monitoring":unsafe{CGPreflightListenEventAccess()!=0},"screen_recording":crate::system::screen_capture_granted(),"terminal_automation":crate::platform::macos::terminal::permission_state("com.apple.Terminal",false),"iterm_automation":crate::platform::macos::terminal::permission_state("com.googlecode.iterm2",false)})
}
fn choose_files(title: &str, multiple: bool, apps: bool) -> Vec<String> {
    unsafe {
        let panel = obj("NSOpenPanel");
        let _: () = msg_send![&*panel,setTitle:&*NSString::from_str(title)];
        let _: () = msg_send![&*panel,setCanChooseFiles:true];
        let _: () = msg_send![&*panel,setCanChooseDirectories:apps];
        let _: () = msg_send![&*panel,setTreatsFilePackagesAsDirectories:false];
        let _: () = msg_send![&*panel,setAllowsMultipleSelection:multiple];
        let response: isize = msg_send![&*panel, runModal];
        if response != 1 {
            return Vec::new();
        }
        let urls: *mut AnyObject = msg_send![&*panel, URLs];
        let n: usize = msg_send![urls, count];
        let mut paths = Vec::new();
        for i in 0..n {
            let url: *mut AnyObject = msg_send![urls,objectAtIndex:i];
            let path: *mut NSString = msg_send![url, path];
            if !path.is_null() {
                paths.push((&*path).to_string());
            }
        }
        paths
    }
}
fn bundle_id(path: &str) -> Result<String> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![AnyClass::get(c"NSBundle").unwrap(),bundleWithPath:&*NSString::from_str(path)];
        ensure!(!bundle.is_null(), "Choose a macOS application bundle.");
        let id: *mut NSString = msg_send![bundle, bundleIdentifier];
        ensure!(!id.is_null(), "This application has no bundle identifier.");
        Ok((&*id).to_string())
    }
}

struct Shell {
    kit: Kit,
    app: Obj,
    controller: Rc<RefCell<Controller>>,
    settings: Settings,
    welcome: Welcome,
    overlay: OverlayOwner,
    power: Rc<RefCell<PowerUi>>,
    updates: UpdateService,
    menus: Menus,
    _main_menu: Obj,
    status_item: Option<Obj>,
    status: Value,
    inventory: Value,
    docs: Option<String>,
    last_external_app: String,
    clipboard: Option<crate::system::ClipboardService>,
    sampler: Option<crate::power::PowerSampler>,
    handoff: Option<Arc<Mutex<crate::update_install::Handoff>>>,
    quitting: bool,
    shutdown_started: bool,
    complete: bool,
    next_refresh: Instant,
    next_shutdown: Instant,
    next_statistics: Instant,
    shutdown_error: String,
    acknowledged: bool,
    images: BTreeMap<String, Obj>,
}
impl Shell {
    fn new(config: Config, options: &LaunchOptions) -> Result<Self> {
        let mtm =
            MainThreadMarker::new().context("The native app must launch on the main thread.")?;
        let app = unsafe {
            let p: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            Retained::retain(p).context("AppKit is unavailable.")?
        };
        unsafe {
            let _: bool = msg_send![&*app,setActivationPolicy:1isize];
        }
        install_source_icon(&app);
        let kit = Kit::new(mtm, options.no_tray);
        let clipboard = crate::system::ClipboardService::install()?;
        let sampler = crate::power::PowerSampler::start()?;
        let controller = Rc::new(RefCell::new(Controller::start(config.clone())?));
        let weak = Rc::downgrade(&controller);
        kit.target
            .ivars()
            .suspend
            .replace(Some(Box::new(move |suspended| {
                if let Some(controller) = weak.upgrade() {
                    let _ = controller.borrow().suspend_shortcuts(suspended);
                }
            })));
        let mut power = PowerUi::new()?;
        let menus = Menus::new(&kit);
        let main = main_menu(&kit, &app);
        let mut overlay = OverlayOwner::new()?;
        let menu_ref =
            unsafe { &*(&*menus.field as *const AnyObject as *const objc2_app_kit::NSMenu) };
        overlay.set_field_menu(menu_ref);
        let mut settings = Settings::new(kit.clone());
        let card = power.make_card("pane");
        let card = unsafe {
            Retained::retain(&*card as *const objc2_app_kit::NSView as *mut AnyObject).unwrap()
        };
        settings.set_power_card(card);
        let welcome = Welcome::new(kit.clone())?;
        let updates = UpdateService::new()?;
        let status_item = if options.no_tray {
            None
        } else {
            unsafe {
                let bar: *mut AnyObject =
                    msg_send![AnyClass::get(c"NSStatusBar").unwrap(), systemStatusBar];
                let raw: *mut AnyObject = msg_send![bar,statusItemWithLength:-1.0f64];
                let item = Retained::retain(raw);
                if let Some(item) = &item {
                    let button: *mut AnyObject = msg_send![&**item, button];
                    if button.is_null() {
                        None
                    } else {
                        let _: () = msg_send![&**item,setMenu:&*menus.resident];
                        let _: () = msg_send![&**item,setAutosaveName:&*NSString::from_str("com.inklow.Inklow")];
                        let _: () = msg_send![&**item,setBehavior:0usize];
                        let _: () = msg_send![&**item,setVisible:true];
                        identify(&*button, "inklow.statusItem", "BroTypist, active");
                        power.set_help_anchor(Some(
                            &*(button as *const objc2_app_kit::NSStatusBarButton),
                        ));
                        item.clone().into()
                    }
                } else {
                    None
                }
            }
        };
        kit.target.ivars().no_tray.set(status_item.is_none());
        let power = Rc::new(RefCell::new(power));
        let weak = Rc::downgrade(&power);
        kit.target
            .ivars()
            .menu_notice
            .replace(Some(Box::new(move |visible| {
                if let Some(power) = weak.upgrade() {
                    let mut power = power.borrow_mut();
                    if visible {
                        power.dismiss_help();
                    }
                }
            })));
        let mut this = Self {
            kit,
            app,
            controller,
            settings,
            welcome,
            overlay,
            power,
            updates,
            menus,
            _main_menu: main,
            status_item,
            status: json!({}),
            inventory: json!([]),
            docs: None,
            last_external_app: String::new(),
            clipboard: Some(clipboard),
            sampler: Some(sampler),
            handoff: None,
            quitting: false,
            shutdown_started: false,
            complete: false,
            next_refresh: Instant::now(),
            next_shutdown: Instant::now(),
            next_statistics: Instant::now(),
            shutdown_error: String::new(),
            acknowledged: false,
            images: BTreeMap::new(),
        };
        unsafe {
            let _: () = msg_send![&*this.app,setDelegate:&*this.kit.target];
            let _: () = msg_send![&*this.app, finishLaunching];
        }
        this.send(Command::Models)?;
        this.send(Command::HistoryInfo)?;
        this.poll_controller()?;
        this.refresh_state()?;
        if config.check_updates_automatically {
            this.updates.start_automatic_check()?;
        }
        let pane = options.pane.as_deref();
        if pane == Some("welcome")
            || (pane.is_none() && (!config.setup_completed || !model_ready(&config)))
        {
            this.show_welcome()?;
        } else if pane.is_some() || this.status_item.is_none() {
            this.navigate(pane.unwrap_or("general"))?;
        }
        Ok(this)
    }
    fn send(&self, command: Command) -> Result<()> {
        self.controller.borrow().send(command)
    }
    fn commands(&self, commands: Vec<Command>) -> Result<()> {
        for command in commands {
            self.send(command)?;
        }
        Ok(())
    }
    fn config(&self) -> Config {
        self.controller.borrow().config()
    }
    fn poll_controller(&mut self) -> Result<()> {
        let events = self.controller.borrow().poll();
        for event in events {
            self.event(event)?;
        }
        Ok(())
    }
    fn event(&mut self, event: Value) -> Result<()> {
        match event["type"].as_str().unwrap_or("") {
            "settings" => {
                let pane = event["pane"].as_str().unwrap_or("general");
                self.navigate(pane)?;
            }
            "quit" => self.request_quit(false)?,
            "models" => {
                self.inventory = event["models"].clone();
                let recommended =
                    &crate::models::recommended_preset(crate::models::physical_memory_bytes()).id;
                if let Some(models) = self.inventory.as_array_mut() {
                    for model in models {
                        model["recommended"] =
                            json!(model["id"].as_str() == Some(recommended.as_str()));
                    }
                }
            }
            "history_info" => self.status["history_info"] = event["result"].clone(),
            "history_counts" => {
                self.status["history_counts"] = event["result"].clone();
                if self.settings.visible()
                    && matches!(self.settings.pane(), "personalization" | "appSettings")
                {
                    let pane = self.settings.pane().to_owned();
                    self.navigate(&pane)?;
                }
            }
            "statistics" => self.status["statistics"] = event["result"].clone(),
            "history_deleted" => {
                self.send(Command::HistoryCounts { unlock: false })?;
                self.send(Command::HistoryInfo)?;
            }
            "statistics_cleared" => self.request_statistics()?,
            "docs_help" => self.docs = event["hint"]["token"].as_str().map(str::to_owned),
            "docs_result" => {
                let good = matches!(
                    event["result"]["status"].as_str(),
                    Some("enabled" | "already_enabled")
                );
                alert(
                    if good {
                        "Google Docs accessibility is enabled"
                    } else {
                        "Google Docs accessibility could not be confirmed"
                    },
                    if good {
                        "Google Docs reported its accessibility setting as enabled. A readable field and verified inline caret are still required; this does not confirm completion support in this document."
                    } else {
                        "Automatic setup is unavailable or focus changed. In Google Docs, open Tools → Accessibility, enable screen reader support and braille support, then click OK. Completion support has not been confirmed."
                    },
                    &["OK"],
                );
            }
            "download" => {
                if event["state"] == "complete" {
                    let id = event["id"].as_str().unwrap_or("");
                    if self.settings.model_selection.as_deref() == Some(id) {
                        self.send(Command::SetModel {
                            path: event["path"].as_str().unwrap_or("").into(),
                            prompt_style: event["prompt_style"]
                                .as_str()
                                .unwrap_or("continuation")
                                .into(),
                            model_id: Some(id.into()),
                        })?;
                    }
                    self.send(Command::Models)?;
                } else if event["state"] == "error" {
                    self.error(
                        event["error"]
                            .as_str()
                            .unwrap_or("The model could not be downloaded."),
                    );
                }
            }
            "error" => self.error(
                event["message"]
                    .as_str()
                    .unwrap_or("The operation could not be completed."),
            ),
            _ => {}
        }
        let commands = self.overlay.handle(&event)?;
        self.commands(commands)?;
        Ok(())
    }
    fn error(&mut self, message: &str) {
        alert("BroTypist couldn’t complete that change", message, &["OK"]);
    }
    fn apply(&mut self, mut config: Config) -> Result<()> {
        config.validate()?;
        self.send(Command::ApplyConfig {
            config: config.clone(),
            persist: true,
        })?;
        self.poll_controller()?;
        ensure!(
            self.config() == config,
            "The preference was not saved. Your previous setting is unchanged."
        );
        Ok(())
    }
    fn set(&mut self, field: &str, value: Value) -> Result<()> {
        ensure!(
            !self.shutdown_started,
            "Local cleanup has started. Quit and reopen BroTypist to make more changes."
        );
        if value == true
            && matches!(
                field,
                "screen_context"
                    | "clipboard_context"
                    | "screenshot_context"
                    | "screenshot_appearance"
            )
            && !context_consent(field)
        {
            return Ok(());
        }
        if field == "record_history" {
            if value == true && !history_consent() {
                return Ok(());
            }
            self.send(Command::SetHistoryEnabled {
                enabled: value
                    .as_bool()
                    .context("History requires a boolean choice.")?,
            })?;
            self.poll_controller()?;
            self.send(Command::HistoryInfo)?;
            return Ok(());
        }
        let mut replacement = serde_json::to_value(self.config())?;
        ensure!(
            replacement.get(field).is_some(),
            "Unknown preference: {field}"
        );
        replacement[field] = value.clone();
        let mut candidate: Config = serde_json::from_value(replacement)?;
        candidate.validate()?;
        if field == "launch_at_login" {
            let old = crate::system::launch_at_login_state()?;
            ensure!(
                crate::system::set_launch_at_login(
                    value
                        .as_bool()
                        .context("Login startup requires a boolean choice.")?
                )?,
                "macOS could not change login startup."
            );
            if let Err(error) = self.apply(candidate) {
                let rollback = crate::system::set_launch_at_login(old);
                return match rollback {
                    Ok(_) => Err(error),
                    Err(rollback) => Err(anyhow::anyhow!(
                        "{error}; login startup rollback failed: {rollback}"
                    )),
                };
            }
        } else {
            self.apply(candidate)?;
        }
        if value == true
            && matches!(field, "screenshot_context" | "screenshot_appearance")
            && !crate::system::screen_capture_granted()
        {
            let _ = crate::system::request_screen_capture_permission()?;
        }
        if field == "backend" && self.settings.visible() && self.settings.pane() == "models" {
            self.navigate("models")?;
        }
        if field == "setup_completed" && value == true && self.status_item.is_none() {
            self.navigate("general")?;
        }
        Ok(())
    }
    fn navigate(&mut self, pane: &str) -> Result<()> {
        if pane == "welcome" {
            return self.show_welcome();
        }
        let config = self.config();
        let commands = self
            .overlay
            .handle(&json!({"type":"clear","hide_accessory":true}))?;
        self.commands(commands)?;
        let update = self.update_snapshot();
        self.settings
            .show(pane, &config, &self.status, &self.inventory, &update)?;
        activate_app(&self.app);
        if pane == "statistics" {
            self.request_statistics()?;
        }
        if matches!(pane, "personalization" | "appSettings") {
            self.send(Command::HistoryInfo)?;
        }
        Ok(())
    }
    fn show_welcome(&mut self) -> Result<()> {
        ensure!(
            !self.shutdown_started,
            "Quit and reopen BroTypist before opening Welcome."
        );
        let config = self.config();
        let commands = self.welcome.show(
            &config,
            &self.status,
            &self.inventory,
            self.settings.model_selection.as_deref(),
        )?;
        self.commands(commands)
    }
    fn request_statistics(&self) -> Result<()> {
        self.send(Command::Statistics {
            period: if self.settings.stat_period == "today" {
                "day".into()
            } else {
                self.settings.stat_period.clone()
            },
            group_by: self.settings.stat_group.clone(),
        })
    }
    fn update_snapshot(&self) -> Value {
        let mut value = self.updates.snapshot();
        value["termination_requested"] = json!(self.quitting);
        value["shutdown_started"] = json!(self.shutdown_started);
        value["shutdown_error"] = json!(self.shutdown_error);
        value["update_handoff"] = json!(self.handoff.is_some());
        value
    }
    fn refresh_state(&mut self) -> Result<()> {
        let current = self.controller.borrow().status();
        let mut previous = std::mem::replace(&mut self.status, current);
        if let Some(previous) = previous.as_object_mut() {
            for field in ["history_info", "history_counts", "statistics"] {
                if let Some(value) = previous.remove(field) {
                    self.status[field] = value;
                }
            }
        }
        self.status["permissions"] = permission_status();
        self.status["inline_prediction_enabled"] =
            json!(crate::system::macos_inline_prediction_enabled());
        self.status["launch_at_login"] = json!(crate::system::launch_at_login_state()?);
        let config = self.config();
        self.status["model_ready"] = json!(model_ready(&config));
        if self.status["enabled"] == true {
            if self.status["permissions"]["accessibility"] != true {
                self.status["state"] = json!("permission");
                self.status["message"] = json!("Accessibility permission required — open Setup");
            } else if self.status["permissions"]["input_monitoring"] != true {
                self.status["state"] = json!("permission");
                self.status["message"] =
                    json!("Input Monitoring required for acceptance shortcuts — open Setup");
            } else if !model_ready(&config) {
                self.status["state"] = json!("empty");
                self.status["message"] = json!("No model installed — choose a model in AI Models");
            } else if matches!(self.status["state"].as_str(), Some("idle" | "ready"))
                && self.status["inline_available"] == false
            {
                self.status["message"] = self.status["inline_message"].clone();
            }
        }
        self.status["shutdown_started"] = json!(self.shutdown_started);
        self.status["termination_requested"] = json!(self.quitting);
        self.status["update_handoff"] = json!(self.handoff.is_some());
        self.status["update_committed"] = json!(
            self.handoff
                .as_ref()
                .is_some_and(|h| h.lock().unwrap().committed())
        );
        self.status["shutdown_error"] = json!(self.shutdown_error);
        unsafe {
            let workspace: *mut AnyObject =
                msg_send![AnyClass::get(c"NSWorkspace").unwrap(), sharedWorkspace];
            let motion: bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
            let contrast: bool = msg_send![workspace, accessibilityDisplayShouldIncreaseContrast];
            let transparency: bool =
                msg_send![workspace, accessibilityDisplayShouldReduceTransparency];
            self.status["reduce_motion"] = json!(motion);
            self.status["increase_contrast"] = json!(contrast);
            self.status["reduce_transparency"] = json!(transparency);
        }
        if let Some(app) = self.status["current_app"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            self.last_external_app = app.into();
        }
        let update = self.update_snapshot();
        self.menus.refresh(
            &self.kit,
            &config,
            &self.status,
            &update,
            self.docs.as_deref(),
        );
        self.overlay.configure(
            config.show_accessory,
            config.shortcuts.get("all").map_or("", String::as_str),
        );
        self.settings
            .refresh(&config, &self.status, &self.inventory, &update)?;
        let commands = self.welcome.refresh(
            &config,
            &self.status,
            &self.inventory,
            self.settings.model_selection.as_deref(),
        )?;
        self.commands(commands)?;
        let _ = self.power.borrow_mut().present(
            &crate::system::power_status(),
            !self.quitting && self.kit.queue.borrow().tracking == 0,
        );
        self.power
            .borrow_mut()
            .set_pane_visible(self.settings.visible() && self.settings.pane() == "battery");
        self.power.borrow_mut().tick();
        self.refresh_icon()?;
        Ok(())
    }
    fn refresh_icon(&mut self) -> Result<()> {
        let Some(item) = &self.status_item else {
            return Ok(());
        };
        let paused = self.status["paused_remaining"].as_f64().unwrap_or(0.) > 0.;
        let attention = matches!(
            self.status["state"].as_str(),
            Some("error" | "permission" | "empty")
        );
        let state = if self.shutdown_started || !self.shutdown_error.is_empty() || attention {
            "off"
        } else if paused {
            "paused"
        } else if !self.config().global_enabled {
            "off"
        } else if matches!(
            self.status["state"].as_str(),
            Some("loading" | "starting" | "warming")
        ) {
            "loading"
        } else {
            "active"
        };
        if !self.images.contains_key(state) {
            self.images.insert(state.into(), status_image(state));
        }
        let image = &self.images[state];
        unsafe {
            let button: *mut AnyObject = msg_send![&**item, button];
            let _: () = msg_send![button,setImage:&**image];
            let _: () = msg_send![button,setTitle:&*NSString::from_str("")];
            let description = if attention || !self.shutdown_error.is_empty() {
                "Needs attention"
            } else {
                match state {
                    "paused" => "Paused",
                    "off" => "Off",
                    "loading" => "Loading",
                    _ => "Active",
                }
            };
            let label = NSString::from_str(&format!("BroTypist, {description}"));
            let _: () = msg_send![button,setAccessibilityLabel:&*label];
            let _: () = msg_send![button,setToolTip:&*label];
            let visible: bool = msg_send![&**item, isVisible];
            if !visible {
                let _: () = msg_send![&**item,setVisible:true];
            }
        }
        Ok(())
    }
    fn select_model(&mut self, id: &str) -> Result<()> {
        ensure!(
            !self.shutdown_started,
            "Quit and reopen before selecting a model."
        );
        if id.is_empty() {
            self.settings.model_selection = None;
            return Ok(());
        }
        let model = self
            .inventory
            .as_array()
            .and_then(|a| a.iter().find(|m| m["id"] == id))
            .context("This model is no longer in the catalogue.")?
            .clone();
        self.settings.model_selection = Some(id.into());
        if model["installed"] == true {
            self.use_model(&model)?;
        }
        Ok(())
    }
    fn use_model(&self, model: &Value) -> Result<()> {
        self.send(Command::SetModel {
            path: model["path"]
                .as_str()
                .context("Model file path is missing.")?
                .into(),
            prompt_style: model["prompt_style"]
                .as_str()
                .unwrap_or("continuation")
                .into(),
            model_id: model["id"].as_str().map(str::to_owned),
        })
    }
    fn download_model(&mut self, id: &str) -> Result<()> {
        ensure!(
            !self.shutdown_started,
            "Quit and reopen before downloading a model."
        );
        let model = self
            .inventory
            .as_array()
            .and_then(|a| a.iter().find(|m| m["id"] == id))
            .context("Choose a model from the catalogue first.")?
            .clone();
        self.settings.model_selection = Some(id.into());
        if model["installed"] == true {
            return self.use_model(&model);
        }
        if let Some(reason) = model["availability"].as_str() {
            anyhow::bail!("{reason}")
        }
        let mut accepted = false;
        if model["requires_license_acceptance"] == true {
            let url = model["license_url"].as_str().unwrap_or("");
            let response = alert(
                &format!(
                    "Accept {}?",
                    model["license_name"]
                        .as_str()
                        .unwrap_or("the model license")
                ),
                &format!(
                    "This separately downloaded model is not covered by BroTypist’s software license. Use is subject to the model terms and incorporated Prohibited Use Policy.\n\n{url}\nhttps://ai.google.dev/gemma/prohibited_use_policy"
                ),
                &["Agree and Download", "Cancel", "Read Terms"],
            );
            if response == 1002 {
                open_url(url)?;
                return Ok(());
            }
            if response != 1000 {
                return Ok(());
            }
            accepted = true;
        }
        self.send(Command::DownloadModel {
            id: id.into(),
            accept_license: accepted,
        })
    }
    fn add_rules(&mut self, items: Vec<(u8, String)>) -> Result<()> {
        let mut config = self.config();
        let mut selected = None;
        for (kind, value) in items {
            let key = if kind == 0 {
                ensure!(!value.is_empty(), "Choose an app with a bundle identity.");
                format!("app:{value}")
            } else {
                format!("domain:{}", normalize_rule_domain(&value)?)
            };
            config.app_rules.entry(key.clone()).or_default();
            selected = Some(key);
        }
        self.apply(config)?;
        if let Some(key) = selected {
            self.settings.selected_rule = key;
        }
        self.navigate("appSettings")
    }
    fn rule(&mut self, key: &str, field: &str, value: Option<Value>) -> Result<()> {
        ensure!(!key.is_empty(), "Switch to an app or website first.");
        if field == "record_history" && value == Some(json!(true)) && !history_consent() {
            return Ok(());
        }
        let mut config = self.config();
        let rule = config.app_rules.entry(key.into()).or_default();
        if let Some(value) = value {
            rule.insert(field.into(), value);
        } else {
            rule.remove(field);
        }
        self.apply(config)
    }
    fn instructions(&mut self, key: &str, save: bool, reset: bool) -> Result<()> {
        self.settings.remember_drafts();
        if reset {
            self.settings
                .set_draft(key, &crate::config::default_instructions(None));
            return Ok(());
        }
        if !save {
            let text = saved_instructions(&self.config(), key);
            self.settings.set_draft(key, &text);
            self.settings.saved(key);
            return Ok(());
        }
        let value = self
            .settings
            .drafts()
            .get(key)
            .cloned()
            .unwrap_or_else(|| saved_instructions(&self.config(), key));
        ensure!(
            value.chars().count() <= 8000,
            "Custom instructions are limited to 8,000 characters."
        );
        let mut config = self.config();
        set_instructions(&mut config, key, &value)?;
        self.apply(config)?;
        self.settings.saved(key);
        Ok(())
    }
    fn dispatch(&mut self, action: UiAction) -> Result<()> {
        let local = self.settings.apply_ui_action(&action);
        if local {
            let pane = self.settings.pane().to_owned();
            let config = self.config();
            let update = self.update_snapshot();
            self.settings
                .show(&pane, &config, &self.status, &self.inventory, &update)?;
        }
        match action {
            UiAction::Navigate(pane) => self.navigate(&pane)?,
            UiAction::Welcome => self.show_welcome()?,
            UiAction::Quit => self.request_quit(false)?,
            UiAction::Set(field, value) => self.set(&field, value)?,
            UiAction::Rule(scope, field, value) => self.rule(&scope, &field, value)?,
            UiAction::AddCurrentApp => {
                ensure!(
                    !self.last_external_app.is_empty(),
                    "Switch to the target application first, then open App Settings from its menu."
                );
                self.add_rules(vec![(0, self.last_external_app.clone())])?;
            }
            UiAction::AddApp => {
                let paths = choose_files("Choose applications", true, true);
                let items = paths
                    .iter()
                    .map(|path| bundle_id(path).map(|id| (0, id)))
                    .collect::<Result<Vec<_>>>()?;
                self.add_rules(items)?;
            }
            UiAction::AddDomain => {
                if let Some(text) = input_dialog(
                    "Add a website",
                    "Enter a hostname such as docs.example.com. Its settings apply in any browser exposing the page URL.",
                ) {
                    self.add_rules(vec![(1, text)])?;
                }
            }
            UiAction::RuleDrop(items) => self.add_rules(items)?,
            UiAction::RemoveRule(key) => {
                if confirm(
                    &format!("Remove settings for {}?", key),
                    "This app or website will inherit general settings. Recorded texts are not deleted.",
                ) {
                    let mut config = self.config();
                    config.app_rules.remove(&key);
                    self.apply(config)?;
                    self.navigate("appSettings")?;
                }
            }
            UiAction::RemoveExclusion(value) => {
                let mut config = self.config();
                config.excluded_apps.retain(|s| s != &value);
                self.apply(config)?;
                self.navigate("appSettings")?;
            }
            UiAction::ModelSelect(id) => self.select_model(&id)?,
            UiAction::ModelDownload(id) => {
                let id = if id.is_empty() {
                    self.settings
                        .model_selection
                        .clone()
                        .unwrap_or_else(|| self.config().model_id)
                } else {
                    id
                };
                self.download_model(&id)?;
            }
            UiAction::ModelImport => {
                ensure!(
                    !self.shutdown_started,
                    "Quit and reopen before importing a model."
                );
                if let Some(path) = choose_files("Choose a local GGUF model", false, false)
                    .into_iter()
                    .next()
                {
                    ensure!(
                        std::path::Path::new(&path)
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("gguf")),
                        "Choose a GGUF model file."
                    );
                    self.send(Command::SetModel {
                        path,
                        prompt_style: self.config().prompt_style,
                        model_id: Some(String::new()),
                    })?;
                    self.settings.model_selection = None;
                }
            }
            UiAction::ModelCancel => self.send(Command::CancelDownload)?,
            UiAction::RevealModels => crate::system::reveal_models()?,
            UiAction::Permission(kind) => {
                if kind == "automation" {
                    open_url(
                        "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation",
                    )?;
                } else {
                    crate::system::open_permissions(&kind)?;
                }
            }
            UiAction::RequestScreen => {
                let granted = crate::system::request_screen_capture_permission()?;
                if !granted {
                    crate::system::open_permissions("screen_recording")?;
                }
            }
            UiAction::RequestAutomation(app) => {
                let state = crate::platform::macos::terminal::permission_state(&app, true);
                if state != "granted" {
                    alert(
                        "Automation permission",
                        &format!(
                            "macOS reports {state}. Automation does not by itself confirm a supported AI-prompt field or verified caret."
                        ),
                        &["OK"],
                    );
                }
            }
            UiAction::HistoryCounts => self.send(Command::HistoryCounts { unlock: true })?,
            UiAction::DeleteHistory { app, domain } => {
                if confirm(
                    "Delete recorded writing?",
                    &format!(
                        "Permanently delete encrypted history and derived style {}. This cannot be undone. Aggregate statistics are unchanged.",
                        app.as_deref()
                            .or(domain.as_deref())
                            .map(|s| format!("for {s}"))
                            .unwrap_or_else(|| "for every app and website".into())
                    ),
                ) {
                    self.send(Command::DeleteHistory { app, domain })?;
                }
            }
            UiAction::ClearStatistics => {
                if confirm(
                    "Clear aggregate statistics?",
                    "This permanently clears acceptance counts and estimated time saved. Encrypted writing history is unchanged.",
                ) {
                    self.send(Command::ClearStatistics)?;
                }
            }
            UiAction::Statistics { .. } => self.request_statistics()?,
            UiAction::SaveInstructions(key) => self.instructions(&key, true, false)?,
            UiAction::RevertInstructions(key) => self.instructions(&key, false, false)?,
            UiAction::ResetInstructions(key) => self.instructions(&key, false, true)?,
            UiAction::RecordShortcut(action) => {
                let current = self.kit.queue.borrow().recording.clone();
                let next = if current.as_deref() == Some(&action) {
                    None
                } else {
                    Some(action)
                };
                self.kit.queue.borrow_mut().recording = next.clone();
                self.settings.recording(next.as_deref());
                self.controller.borrow().suspend_shortcuts(next.is_some())?;
            }
            UiAction::ShortcutCaptured(action, value) => {
                let mut config = self.config();
                config.shortcuts.insert(action, value);
                self.apply(config)?;
            }
            UiAction::UpdateCheck => {
                if !self.quitting {
                    self.navigate("about")?;
                    self.updates.start_check()?;
                }
            }
            UiAction::UpdateDownload => {
                if !self.quitting {
                    self.updates.start_download()?;
                }
            }
            UiAction::UpdateCancel => {
                if let Some(h) = self.handoff.take() {
                    h.lock().unwrap().abort();
                    self.quitting = false;
                    if self.shutdown_started {
                        self.shutdown_error="Installation was cancelled. Local activity is stopped; use Quit to finish cleanup, then reopen BroTypist.".into();
                    }
                }
                self.updates.cancel();
            }
            UiAction::UpdateInstall => {
                let update = self.updates.snapshot();
                if update["state"] == "ready"
                    && update["installable"] == true
                    && !self.quitting
                    && confirm(
                        "Install update and relaunch?",
                        "BroTypist will stop local activity, quit, replace this app with the verified update and relaunch. Your models, history and settings are not replaced. This free-signed app is not notarized; macOS may require per-app approval. Never disable Gatekeeper.",
                    )
                    && self.allow_drafts()?
                {
                    self.handoff = Some(self.updates.prepare_install()?);
                    self.request_quit(true)?;
                }
            }
            UiAction::Pause(minutes) => self.send(Command::Pause { minutes })?,
            UiAction::PauseTomorrow => self.send(Command::Pause {
                minutes: minutes_until_tomorrow(),
            })?,
            UiAction::Resume => self.send(Command::Resume)?,
            UiAction::ToggleApp { app, domain } => self.send(Command::ToggleApp { app, domain })?,
            UiAction::Docs(token) => self.send(Command::EnableDocs {
                token: token.or_else(|| self.docs.clone()).unwrap_or_default(),
            })?,
            UiAction::Url(url) => open_url(&url)?,
            UiAction::WelcomeStep(delta) => {
                let config = self.config();
                let commands = self.welcome.navigate(
                    delta,
                    &config,
                    &self.status,
                    &self.inventory,
                    self.settings.model_selection.as_deref(),
                )?;
                self.commands(commands)?;
            }
            UiAction::PreviewLanguage(lang) => self.welcome.preview_language(&lang)?,
            UiAction::PreviewAccept(full) => self.welcome.preview_accept(full)?,
            UiAction::PreviewDismiss => self.welcome.preview_dismiss(),
            UiAction::PreviewReleased => self.send(Command::ResumeFromPreview)?,
            UiAction::Resized => self.settings.reflow(),
            UiAction::Reopened => {
                if self.config().setup_completed {
                    let pane = self.settings.pane().to_owned();
                    self.navigate(&pane)?;
                } else {
                    self.show_welcome()?;
                }
            }
            UiAction::Deactivated(id) | UiAction::Closed(id) => {
                self.cancel_recording()?;
                if id == "inklow.welcome" {
                    self.welcome.window_deactivated()?;
                }
            }
            UiAction::SelectRule(_) | UiAction::FilterRules(_) | UiAction::ModelAdvanced(_) => {}
        }
        Ok(())
    }
    fn cancel_recording(&mut self) -> Result<()> {
        if self.kit.queue.borrow_mut().recording.take().is_some() {
            self.settings.recording(None);
            self.controller
                .borrow()
                .suspend_shortcuts(self.kit.queue.borrow().tracking > 0)?;
        }
        Ok(())
    }
    fn allow_drafts(&mut self) -> Result<bool> {
        self.settings.remember_drafts();
        let config = self.config();
        let drafts = self.settings.drafts();
        let dirty: Vec<_> = drafts
            .iter()
            .filter(|(key, text)| **text != saved_instructions(&config, key))
            .collect();
        if dirty.is_empty() {
            return Ok(true);
        }
        match alert(
            "Unsaved custom instructions",
            "Save all changed instruction drafts before quitting, discard them, or cancel and keep editing.",
            &["Save Changes", "Cancel", "Discard Changes"],
        ) {
            1000 => {
                let mut config = config;
                for (key, text) in dirty {
                    set_instructions(&mut config, key, text)?;
                }
                self.apply(config)?;
                Ok(true)
            }
            1002 => Ok(true),
            _ => Ok(false),
        }
    }
    fn request_quit(&mut self, drafts_approved: bool) -> Result<()> {
        if self.complete || self.quitting {
            return Ok(());
        }
        if !drafts_approved && !self.allow_drafts()? {
            return Ok(());
        }
        self.quitting = true;
        self.next_shutdown = Instant::now();
        Ok(())
    }
    fn shutdown_step(&mut self) -> Result<()> {
        if !self.quitting || Instant::now() < self.next_shutdown {
            return Ok(());
        }
        self.shutdown_started = true;
        self.power.borrow_mut().dismiss_help();
        let mut pending = false;
        let mut failures = Vec::new();
        match self.welcome.close() {
            Ok(done) => pending |= !done,
            Err(e) => failures.push(format!("Preview: {e}")),
        }
        match self.controller.borrow_mut().shutdown() {
            Ok(done) => pending |= !done,
            Err(e) => failures.push(format!("Controller: {e}")),
        }
        if let Err(e) = self.overlay.close() {
            failures.push(format!("Overlay: {e}"))
        }
        if self.handoff.is_none() {
            if let Err(e) = self.updates.close() {
                if e.to_string().contains("Finishing cancellation") {
                    pending = true
                } else {
                    failures.push(format!("Updates: {e}"));
                }
            }
        }
        if !failures.is_empty() {
            return self.cleanup_failed(failures);
        }
        if pending {
            self.next_shutdown = Instant::now() + Duration::from_millis(100);
            return Ok(());
        }
        // Both services outlive all context and model readers.
        if let Some(sampler) = self.sampler.as_mut() {
            if let Err(e) = sampler.stop() {
                failures.push(format!("Power sampling: {e}"));
            }
        }
        if let Err(e) = self.power.borrow_mut().close() {
            failures.push(format!("Power UI: {e}"))
        }
        if !failures.is_empty() {
            return self.cleanup_failed(failures);
        }
        self.sampler.take();
        self.clipboard.take();
        if let Some(handoff) = &self.handoff {
            let result = handoff.lock().unwrap().commit();
            if let Err(e) = result {
                return self.cleanup_failed(vec![format!("Update handoff: {e}")]);
            }
        }
        self.settings.close();
        self.complete = true;
        Ok(())
    }
    fn cleanup_failed(&mut self, failures: Vec<String>) -> Result<()> {
        if let Some(h) = self.handoff.take() {
            h.lock().unwrap().abort();
            self.updates.cancel();
        }
        self.quitting = false;
        self.shutdown_error = format!(
            "Quit was cancelled because local cleanup failed. No update was installed. Local activity is stopped; try Quit again, then reopen BroTypist.\n{}",
            failures.join("\n")
        );
        let error = self.shutdown_error.clone();
        self.navigate("about")?;
        self.error(&error);
        Ok(())
    }
    fn tick(&mut self) -> Result<()> {
        if self.kit.queue.borrow().tracking > 0 {
            return Ok(());
        }
        if let Some(clipboard) = &self.clipboard {
            if let Err(error) = clipboard.poll() {
                if !self.quitting {
                    return Err(error);
                }
            }
        }
        loop {
            let action = self.kit.queue.borrow_mut().pending.pop_front();
            let Some(action) = action else { break };
            if let Err(e) = self.dispatch(action) {
                self.error(&e.to_string());
            }
        }
        let restore = {
            let mut queue = self.kit.queue.borrow_mut();
            let restore = queue.resume_shortcuts && queue.recording.is_none();
            if restore {
                queue.resume_shortcuts = false;
            }
            restore
        };
        if restore && !self.shutdown_started {
            self.controller.borrow().suspend_shortcuts(false)?;
        }
        if self.quitting {
            return self.shutdown_step();
        }
        if !self.shutdown_started {
            self.poll_controller()?;
            let commands = self.overlay.tick()?;
            self.commands(commands)?;
        }
        if self.settings.visible()
            && self.settings.pane() == "statistics"
            && !self.shutdown_started
            && Instant::now() >= self.next_statistics
        {
            self.request_statistics()?;
            self.next_statistics = Instant::now() + Duration::from_secs(3);
        }
        if Instant::now() >= self.next_refresh {
            self.refresh_state()?;
            self.next_refresh = Instant::now() + Duration::from_millis(100);
        }
        if !self.acknowledged {
            crate::update_install::acknowledge_startup()?;
            self.acknowledged = true;
        }
        Ok(())
    }
}
fn model_ready(config: &Config) -> bool {
    config.backend != "gguf"
        || (!config.model_path.is_empty() && std::path::Path::new(&config.model_path).is_file())
}
fn saved_instructions(config: &Config, key: &str) -> String {
    if key == "global" {
        config.custom_instructions.clone()
    } else {
        key.strip_prefix("rule:")
            .and_then(|key| config.app_rules.get(key))
            .and_then(|r| r.get("custom_instructions"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .into()
    }
}
fn set_instructions(config: &mut Config, key: &str, text: &str) -> Result<()> {
    ensure!(
        text.chars().count() <= 8000,
        "Custom instructions are limited to 8,000 characters."
    );
    if key == "global" {
        config.custom_instructions = text.into();
    } else {
        let key = key
            .strip_prefix("rule:")
            .context("Unknown instruction editor.")?;
        let rule = config
            .app_rules
            .get_mut(key)
            .context("The app or website rule was removed; its draft has not been saved.")?;
        rule.insert("custom_instructions".into(), json!(text));
    }
    Ok(())
}
fn normalize_rule_domain(value: &str) -> Result<String> {
    let value = value.trim();
    let host = if value.contains("://") {
        let url = url::Url::parse(value)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none(),
            "Enter a website hostname without credentials or port."
        );
        url.host_str()
            .context("Enter a complete website hostname.")?
            .to_owned()
    } else {
        value.to_owned()
    };
    let domain = crate::config::normalize_domain(&host)?;
    ensure!(
        domain.contains('.'),
        "Enter a complete hostname such as docs.example.com."
    );
    Ok(domain)
}
fn minutes_until_tomorrow() -> u32 {
    use chrono::{Datelike, TimeZone};
    let now = chrono::Local::now();
    let tomorrow = now.date_naive().succ_opt().unwrap_or(now.date_naive());
    let midnight = chrono::Local
        .with_ymd_and_hms(tomorrow.year(), tomorrow.month(), tomorrow.day(), 0, 0, 0)
        .earliest();
    midnight
        .map(|then| ((then - now).num_seconds().max(60) / 60) as u32)
        .unwrap_or(60)
}
fn input_dialog(title: &str, message: &str) -> Option<String> {
    let dialog = obj("NSAlert");
    let field = obj("NSTextField");
    frame(&field, &rect(0., 0., 360., 26.));
    unsafe {
        let _: () = msg_send![&*dialog,setMessageText:&*NSString::from_str(title)];
        let _: () = msg_send![&*dialog,setInformativeText:&*NSString::from_str(message)];
        let _: () = msg_send![&*dialog,setAccessoryView:&*field];
        let _: *mut AnyObject = msg_send![&*dialog,addButtonWithTitle:&*NSString::from_str("Add")];
        let _: *mut AnyObject =
            msg_send![&*dialog,addButtonWithTitle:&*NSString::from_str("Cancel")];
        let response: isize = msg_send![&*dialog, runModal];
        (response == 1000).then(|| string(&field))
    }
}
fn status_image(state: &str) -> Obj {
    unsafe {
        let class = AnyClass::get(c"NSImage").unwrap();
        let p: *mut AnyObject = msg_send![class, alloc];
        let p: *mut AnyObject = msg_send![p,initWithSize:NSSize::new(18.,16.)];
        let image = Retained::from_raw(p).unwrap();
        let _: () = msg_send![&*image, lockFocus];
        let black: *mut AnyObject = msg_send![AnyClass::get(c"NSColor").unwrap(), blackColor];
        let _: () = msg_send![black, setFill];
        let paths = AnyClass::get(c"NSBezierPath").unwrap();
        let caret: *mut AnyObject = msg_send![paths,bezierPathWithRoundedRect:rect(2.,2.,2.4,12.),xRadius:1.2f64,yRadius:1.2f64];
        let _: () = msg_send![caret, fill];
        match state {
            "paused" => {
                for x in [9., 13.] {
                    let path: *mut AnyObject = msg_send![paths,bezierPathWithRoundedRect:rect(x,5.,1.8,6.),xRadius:0.9f64,yRadius:0.9f64];
                    let _: () = msg_send![path, fill];
                }
            }
            "loading" => {
                for (x, alpha) in [(8., 1.), (11.5, 0.6), (15., 0.3)] {
                    let color: *mut AnyObject = msg_send![black,colorWithAlphaComponent:alpha];
                    let _: () = msg_send![color, setFill];
                    let path: *mut AnyObject =
                        msg_send![paths,bezierPathWithOvalInRect:rect(x-1.3,6.7,2.6,2.6)];
                    let _: () = msg_send![path, fill];
                }
            }
            _ => {
                let color: *mut AnyObject =
                    msg_send![black,colorWithAlphaComponent:if state=="off"{0.45f64}else{0.5f64}];
                let _: () = msg_send![color, setFill];
                let path: *mut AnyObject = msg_send![paths,bezierPathWithRoundedRect:rect(7.,6.5,9.,3.),xRadius:1.5f64,yRadius:1.5f64];
                if state == "off" {
                    let _: () = msg_send![color, setStroke];
                    let _: () = msg_send![path, stroke];
                } else {
                    let _: () = msg_send![path, fill];
                }
            }
        }
        let _: () = msg_send![&*image, unlockFocus];
        let _: () = msg_send![&*image,setTemplate:true];
        image
    }
}
fn record_event(event: &AnyObject) -> Result<Option<String>> {
    unsafe {
        let code: u16 = msg_send![event, keyCode];
        let flags: usize = msg_send![event, modifierFlags];
        if code == 53 && flags & 0x1e0000 == 0 {
            return Ok(None);
        }
        if matches!(code, 51 | 117) {
            return Ok(Some(String::new()));
        }
        let mapped = crate::shortcuts::mac_key_name(u32::from(code));
        let raw: *mut NSString = msg_send![event, charactersIgnoringModifiers];
        let text = if raw.is_null() {
            String::new()
        } else {
            (&*raw).to_string()
        };
        let key = if mapped.len() > 1 { &mapped } else { &text };
        let modifiers: Vec<_> = [
            ("Ctrl", 1 << 18),
            ("Alt", 1 << 19),
            ("Shift", 1 << 17),
            ("Super", 1 << 20),
        ]
        .into_iter()
        .filter_map(|(name, bit)| (flags & bit != 0).then_some(name))
        .collect();
        Ok(Some(crate::shortcuts::shortcut_from_event(
            key,
            &modifiers,
            "macos",
            Some(u32::from(code)),
        )?))
    }
}
/// Run AppKit without surrendering ownership of the controller to a background thread.
pub fn launch(config: Config, options: LaunchOptions) -> Result<i32> {
    let mut shell = objc2::rc::autoreleasepool(|_| Shell::new(config, &options))?;
    let mut failure = None;
    while !shell.complete {
        let step = objc2::rc::autoreleasepool(|_| -> Result<()> {
            unsafe {
                let date: *mut AnyObject = msg_send![AnyClass::get(c"NSDate").unwrap(),dateWithTimeIntervalSinceNow:0.025f64];
                let event: *mut AnyObject = msg_send![&*shell.app,nextEventMatchingMask:usize::MAX,untilDate:date,inMode:&*NSString::from_str("kCFRunLoopDefaultMode"),dequeue:true];
                if !event.is_null() {
                    let kind: usize = msg_send![event, type];
                    let recording = shell.kit.queue.borrow().recording.clone();
                    let mut consume = false;
                    if let Some(action) = recording {
                        if kind == 10 {
                            match record_event(&*event) {
                                Ok(value) => {
                                    shell.cancel_recording()?;
                                    if let Some(value) = value {
                                        shell
                                            .kit
                                            .queue
                                            .borrow_mut()
                                            .pending
                                            .push_back(UiAction::ShortcutCaptured(action, value));
                                    }
                                }
                                Err(_) => objc2_app_kit::NSBeep(),
                            }
                            consume = true;
                        } else if matches!(kind, 1 | 3 | 25) {
                            shell.cancel_recording()?;
                        }
                    }
                    if !consume {
                        let _: () = msg_send![&*shell.app,sendEvent:event];
                    }
                }
                let _: () = msg_send![&*shell.app, updateWindows];
            }
            shell.tick()
        });
        if let Err(error) = step {
            if failure.is_none() {
                failure = Some(error);
            }
            if let Some(handoff) = shell.handoff.take() {
                handoff.lock().unwrap().abort();
                shell.updates.cancel();
            }
            shell.quitting = true;
            shell.next_shutdown = Instant::now();
        }
    }
    let _ = shell.updates.close();
    let _ = shell.power.borrow_mut().close();
    if let Some(sampler) = shell.sampler.as_mut() {
        let _ = sampler.stop();
    }
    shell.clipboard.take();
    if let Some(item) = shell.status_item.take() {
        unsafe {
            let bar: *mut AnyObject =
                msg_send![AnyClass::get(c"NSStatusBar").unwrap(), systemStatusBar];
            let _: () = msg_send![bar,removeStatusItem:&*item];
        }
    }
    unsafe {
        let _: () = msg_send![&*shell.app,setDelegate:std::ptr::null::<AnyObject>()];
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_are_navigation_only() {
        assert_eq!(
            url_target("brotypist://settings/appsettings?global_enabled=false"),
            Some("appSettings".into())
        );
        assert_eq!(url_target("brotypist://welcome"), Some("welcome".into()));
        assert_eq!(
            url_target("brotypist://deleteHistory"),
            Some("general".into())
        );
        assert_eq!(url_target("https://settings/models"), None);
    }
    #[test]
    fn inherited_false_is_not_absence() {
        let key = "app:com.example.Editor".to_owned();
        let action = value_action(
            UiAction::Rule(key.clone(), "tab_enabled".into(), None),
            json!("no"),
        );
        assert!(matches!(
            action,
            UiAction::Rule(_, _, Some(Value::Bool(false)))
        ));
        let action = value_action(
            UiAction::Rule(key, "tab_enabled".into(), Some(json!(false))),
            json!("inherit"),
        );
        assert!(matches!(action, UiAction::Rule(_, _, None)));
    }
    #[test]
    fn inactive_rule_draft_never_recreates_deleted_rule() {
        let mut config = Config::default();
        assert!(set_instructions(&mut config, "rule:app:removed", "private draft").is_err());
        assert!(config.app_rules.is_empty());
        let text = "🦀".repeat(8001);
        assert!(set_instructions(&mut config, "global", &text).is_err());
    }
    #[test]
    fn website_intent_is_canonical_but_not_url_authority() {
        assert_eq!(
            normalize_rule_domain("https://DOCS.Example.com/path").unwrap(),
            "docs.example.com"
        );
        for value in [
            "https://user@example.com",
            "https://example.com:8443",
            "localhost",
            "https://user:secret@example.com",
        ] {
            assert!(normalize_rule_domain(value).is_err());
        }
        assert_eq!(
            scope_key("com.example.Browser", "EXAMPLE.COM"),
            Some("domain:example.com".into())
        );
        assert_eq!(scope_key("", "example.com"), None);
    }
}

fn install_source_icon(app: &AnyObject) {
    // Packaged adaptive artwork belongs to macOS. Only source launches need this fallback.
    unsafe {
        let bundle: *mut AnyObject = msg_send![AnyClass::get(c"NSBundle").unwrap(), mainBundle];
        let adaptive: *mut AnyObject =
            msg_send![bundle,objectForInfoDictionaryKey:&*NSString::from_str("CFBundleIconName")];
        if !adaptive.is_null() {
            return;
        }
        let bytes = include_bytes!("../../assets/app.icns");
        let data: *mut AnyObject = msg_send![AnyClass::get(c"NSData").unwrap(),dataWithBytes:bytes.as_ptr().cast::<std::ffi::c_void>(),length:bytes.len()];
        let image: *mut AnyObject = msg_send![AnyClass::get(c"NSImage").unwrap(), alloc];
        let image: *mut AnyObject = msg_send![image,initWithData:data];
        if let Some(image) = Retained::from_raw(image) {
            let _: () = msg_send![app,setApplicationIconImage:&*image];
        }
    }
}
