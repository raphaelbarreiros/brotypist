//! Main-thread, nonactivating AppKit presentation. Acknowledgments describe only
//! verified visible glyphs; an ordering request alone never arms host insertion.
use anyhow::{Result, anyhow};
use crate::{core::{Authority, InlineRender, Rect, SnapshotKey}, platform::Action, runtime::Command};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::*;
use objc2_core_text::CTLine;
use objc2_foundation::{MainThreadMarker, NSAttributedString, NSData, NSDictionary, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSNumber};
use serde_json::Value;
use std::{cell::RefCell, ffi::c_void};

#[link(name="CoreGraphics", kind="framework")]
unsafe extern "C" {
    fn CGContextSetTextPosition(context: *const c_void, x: f64, y: f64);
    fn CGContextSetTextMatrix(context: *mut c_void, matrix: Transform);
    fn CGContextSetFillColorWithColor(context:*mut c_void,color:*const c_void);
}
#[repr(C)] struct Transform { a:f64,b:f64,c:f64,d:f64,tx:f64,ty:f64 }
#[link(name="CoreText", kind="framework")]
unsafe extern "C" { fn CTLineDraw(line: *const c_void, context: *const c_void); }

define_class!(
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    pub(crate) struct PassivePanel;
    impl PassivePanel {
        #[unsafe(method(canBecomeKeyWindow))] fn no_key(&self)->bool { false }
        #[unsafe(method(canBecomeMainWindow))] fn no_main(&self)->bool { false }
    }
);
impl PassivePanel {
    pub(crate) fn create(mtm:MainThreadMarker, id:&str, label:&str, level:isize, all_spaces:bool)->Retained<Self> {
        let panel:Retained<Self> = unsafe { msg_send![Self::alloc(mtm), initWithContentRect: rect([0.,0.,100.,30.]), styleMask:NSWindowStyleMask::Borderless|NSWindowStyleMask::NonactivatingPanel, backing:NSBackingStoreType::Buffered, defer:false] };
        unsafe { panel.setReleasedWhenClosed(false); }
        panel.setIdentifier(Some(&NSString::from_str(id)));
        unsafe { let _:()=msg_send![&*panel,setAccessibilityIdentifier:&*NSString::from_str(id)]; let _:()=msg_send![&*panel,setAccessibilityLabel:&*NSString::from_str(label)]; }
        panel.setLevel(level); panel.setHidesOnDeactivate(false); panel.setBecomesKeyOnlyIfNeeded(true); panel.setFloatingPanel(true);
        panel.setOpaque(false); panel.setBackgroundColor(Some(&NSColor::clearColor())); panel.setHasShadow(true);
        let mut behavior=NSWindowCollectionBehavior::Transient|NSWindowCollectionBehavior::IgnoresCycle|NSWindowCollectionBehavior::FullScreenAuxiliary;
        if all_spaces { behavior|=NSWindowCollectionBehavior::CanJoinAllSpaces; }
        panel.setCollectionBehavior(behavior); panel
    }
}

define_class!(
    #[unsafe(super = NSButton)]
    #[thread_kind = MainThreadOnly]
    struct PassiveButton;
    impl PassiveButton {
        #[unsafe(method(acceptsFirstMouse:))] fn first_mouse(&self,_event:Option<&NSEvent>)->bool { true }
        #[unsafe(method(acceptsFirstResponder))] fn no_responder(&self)->bool { false }
    }
);
struct ActionsIvars { queue:RefCell<Vec<Command>>, menu:RefCell<Option<Retained<NSMenu>>>, enabled:RefCell<Vec<usize>> }
define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ActionsIvars]
    struct OverlayActions;
    unsafe impl NSObjectProtocol for OverlayActions {}
    impl OverlayActions {
        #[unsafe(method(pressed:))]
        fn pressed(&self,sender:&NSButton) {
            let tag=sender.tag();
            if tag==100 {
                if NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Option) {
                    self.ivars().queue.borrow_mut().push(Command::Action(Action::Bypass));
                } else if let Some(menu)=self.ivars().menu.borrow().as_ref().cloned() {
                    menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.,-4.), Some(sender));
                }
            } else if tag>0 && self.ivars().enabled.borrow().contains(&(tag as usize)) {
                self.ivars().queue.borrow_mut().push(Command::Action(Action::Choice(tag as usize)));
            }
        }
    }
);
impl OverlayActions {
    fn new(mtm:MainThreadMarker)->Retained<Self> {
        let this=Self::alloc(mtm).set_ivars(ActionsIvars{queue:RefCell::new(Vec::new()),menu:RefCell::new(None),enabled:RefCell::new(Vec::new())});
        unsafe { msg_send![super(this),init] }
    }
}
struct DrawRun { text:String, x:f64, color:Retained<NSColor> }
struct GhostIvars { font:Retained<NSFont>, baseline:f64, runs:Vec<DrawRun>, fills:Vec<(NSRect,Retained<NSColor>)> }
define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = GhostIvars]
    struct GhostSurface;
    impl GhostSurface {
        #[unsafe(method(isOpaque))] fn transparent(&self)->bool { false }
        #[unsafe(method(hitTest:))] fn mouse_through(&self,_point:NSPoint)->Option<&NSView> { None }
        #[unsafe(method(drawRect:))]
        fn draw(&self,_dirty:NSRect) {
            let Some(context)=NSGraphicsContext::currentContext() else {return};
            let cg:*mut c_void=unsafe {msg_send![&*context,CGContext]};
            if cg.is_null() {return;}
            for (frame,color) in &self.ivars().fills { color.setFill(); NSBezierPath::fillRect(*frame); }
            unsafe { CGContextSetTextMatrix(cg,Transform{a:1.,b:0.,c:0.,d:1.,tx:0.,ty:0.}); }
            for run in &self.ivars().runs {
                // CFString/NSString are toll-free bridged. Use CoreText's
                // context-color flag, not an AppKit NSColor as a CGColor value.
                let color_key=unsafe{&*(objc2_core_text::kCTForegroundColorFromContextAttributeName as *const _ as *const NSString)};
                let from_context=NSNumber::new_bool(true);
                let attributes=NSDictionary::from_slices(&[unsafe{NSFontAttributeName},color_key], &[&*self.ivars().font as &AnyObject,&*from_context as &AnyObject]);
                unsafe{let color:*const c_void=msg_send![&*run.color,CGColor];CGContextSetFillColorWithColor(cg,color);}
                let attributed=unsafe {NSAttributedString::new_with_attributes(&NSString::from_str(&run.text),&attributes)};
                let line=unsafe {CTLine::with_attributed_string((&*attributed).as_ref())};
                unsafe {CGContextSetTextPosition(cg,run.x,self.ivars().baseline);CTLineDraw(&*line as *const _ as *const c_void,cg);}
            }
        }
    }
);
impl GhostSurface {
    fn new(mtm:MainThreadMarker,size:NSSize,ivars:GhostIvars)->Retained<Self> {
        let this=Self::alloc(mtm).set_ivars(ivars);
        let view:Retained<Self>=unsafe {msg_send![super(this),initWithFrame:NSRect::new(NSPoint::new(0.,0.),size)]};
        unsafe{let _:()=msg_send![&*view,setAccessibilityElement:false];}view
    }
}

fn rect(r:Rect)->NSRect {NSRect::new(NSPoint::new(r[0],r[1]),NSSize::new(r[2],r[3]))}
fn finite_rect(value:&Value)->Option<Rect> {
    let values=value.as_array()?; if values.len()!=4{return None;}
    let r=[values[0].as_f64()?,values[1].as_f64()?,values[2].as_f64()?,values[3].as_f64()?];
    (r.iter().all(|v|v.is_finite())&&r[2]>=0.&&r[3]>0.).then_some(r)
}
fn rgb(v:[f64;3])->Retained<NSColor> {NSColor::colorWithSRGBRed_green_blue_alpha(v[0],v[1],v[2],1.)}
fn width(font:&NSFont,text:&str)->f64 {
    let attrs=NSDictionary::from_slices(&[unsafe{NSFontAttributeName}],&[font as &AnyObject]);
    let text=unsafe{NSAttributedString::new_with_attributes(&NSString::from_str(text),&attrs)};
    let line=unsafe{CTLine::with_attributed_string((&*text).as_ref())};
    unsafe{line.typographic_bounds(std::ptr::null_mut(),std::ptr::null_mut(),std::ptr::null_mut())}
}
fn resolve_font(render:&InlineRender)->Option<Retained<NSFont>> {
    if render.font_name.is_empty()||!render.font_size.is_finite()||!(1.0..=300.0).contains(&render.font_size)||!render.system_weight.is_finite()||!render.fixed_advance.is_finite()||!(0.0..=300.0).contains(&render.fixed_advance) {return None;}
    // The fitted AX row is legal only at a line end, with no repaint of host text.
    if render.source=="macos-ax-caret-row" && (!render.system_font||!render.suffix.is_empty()||render.suffix_width!=0.) {return None;}
    let mut font=if render.system_font {Some(NSFont::systemFontOfSize_weight(render.font_size,render.system_weight))} else if render.font_name.starts_with('.') {None} else {NSFont::fontWithName_size(&NSString::from_str(&render.font_name),render.font_size)}?;
    if render.fixed_advance>0. {
        let value=NSNumber::new_f64(render.fixed_advance);
        let attrs=NSDictionary::from_slices(&[unsafe{NSFontFixedAdvanceAttribute}],&[&*value as &AnyObject]);
        let descriptor=unsafe{font.fontDescriptor().fontDescriptorByAddingAttributes(&attrs)};
        font=NSFont::fontWithDescriptor_size(&descriptor,render.font_size)?;
    }
    (font.fontName().to_string()==render.font_name).then_some(font)
}
fn fit_text<'a>(text:&'a str,available:f64,measure:impl Fn(&str)->f64)->&'a str {
    if !available.is_finite()||available<=0.||text.is_empty(){return "";}
    if measure(text)<=available{return text;}
    let mut best="";
    for (i,c) in text.char_indices() {if c.is_whitespace()&&!text[..i].trim().is_empty(){if measure(&text[..i])>available{break;}best=&text[..i];}}
    best
}
fn chip_text(text:&str)->String {
    let clean=text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars:Vec<char>=clean.chars().collect(); if chars.len()<=44{return clean;}
    let mut end=43; if !chars[43].is_whitespace(){if let Some(i)=chars[..43].iter().rposition(|c|c.is_whitespace()){if i>0{end=i;}}}
    format!("{}…",chars[..end].iter().collect::<String>().trim_end())
}
fn visible_choices(rows:&[String],available:f64,measure:impl Fn(&str)->f64)->usize {
    rows.iter().take(9).enumerate().take_while(|(i,row)|chip_text(row)==**row&&measure(&format!("{}   {}",i+1,row))<=available).count()
}
fn visible(panel:&PassivePanel)->bool {panel.isVisible()&&panel.isOnActiveSpace()}
fn cached(identity:&Value,previous:Option<&(Value,String)>,shown:bool)->bool {previous.is_some_and(|(old,text)|old==identity&&!text.is_empty()&&shown)}
fn position(mtm:MainThreadMarker,r:Rect,w:f64,h:f64,accessory:bool)->Option<NSRect> {
    let screens=NSScreen::screens(mtm); let first=screens.firstObject()?; let top=first.frame().origin.y+first.frame().size.height;
    let bottom=top-r[1]-r[3];
    let screen=screens.iter().find(|s|contains_point(s.frame(),r[0],bottom)).unwrap_or(first);
    let v=screen.visibleFrame(); let minx=v.origin.x; let maxx=minx+v.size.width; let miny=v.origin.y;let maxy=miny+v.size.height;
    let mut x=if accessory {r[0]-w-6.}else{r[0]}; let mut y=if accessory {top-r[1]+6.}else{bottom-h-6.};
    if accessory {if x<minx+4.||x+w>maxx-4.||y<miny+4.||y+h>maxy-4.{return None;}} else {
        if bottom-miny<h+40.{y=bottom+r[3]+6.;} x=x.max(minx+8.).min(maxx-w-8.);y=y.max(miny+8.).min(maxy-h-8.);
    }
    Some(rect([x,y,w,h]))
}
fn contains_point(r:NSRect,x:f64,y:f64)->bool {x>=r.origin.x&&x<=r.origin.x+r.size.width&&y>=r.origin.y&&y<=r.origin.y+r.size.height}
fn contains_rect(outer:NSRect,inner:NSRect)->bool {contains_point(outer,inner.origin.x,inner.origin.y)&&contains_point(outer,inner.origin.x+inner.size.width,inner.origin.y+inner.size.height)}

fn accessory_image(mtm:MainThreadMarker)->Result<Retained<NSImage>> {
    let image=NSImage::initWithSize(NSImage::alloc(),NSSize::new(18.,16.));
    for bytes in [
        include_bytes!("../../assets/active-template-18.png").as_slice(),
        include_bytes!("../../assets/active-template-36.png").as_slice(),
    ] {
        let data=NSData::with_bytes(bytes);
        let representation=NSBitmapImageRep::imageRepWithData(&data).ok_or_else(||anyhow!("Invalid embedded accessory image"))?;
        representation.setSize(NSSize::new(18.,16.));
        image.addRepresentation(&representation);
    }
    let _=mtm;
    image.setTemplate(true);Ok(image)
}

pub struct OverlayOwner {
    mtm:MainThreadMarker, ghost:Retained<PassivePanel>,chooser:Retained<PassivePanel>,accessory:Retained<PassivePanel>,button:Retained<PassiveButton>,target:Retained<OverlayActions>,
    show_accessory:bool,caret:Option<Rect>,field:Option<Rect>,docs:Option<Rect>,ghost_state:Option<(Value,String)>,chooser_state:Option<(Value,String)>,ghost_event:Option<Value>,chooser_event:Option<Value>,environment:Value,closed:bool,
}
impl OverlayOwner {
    pub fn new()->Result<Self> {
        let mtm=MainThreadMarker::new().ok_or_else(||anyhow!("OverlayOwner requires the main AppKit thread"))?;
        let ghost=PassivePanel::create(mtm,"inklow.suggestion","BroTypist suggestion",NSPopUpMenuWindowLevel,true); ghost.setHasShadow(false);ghost.setIgnoresMouseEvents(true);
        let chooser=PassivePanel::create(mtm,"inklow.chooser","BroTypist suggestions",NSPopUpMenuWindowLevel,true);
        let accessory=PassivePanel::create(mtm,"inklow.accessory","BroTypist accessory",NSPopUpMenuWindowLevel,true);accessory.setHasShadow(false);
        let target=OverlayActions::new(mtm);
        let button:Retained<PassiveButton>=unsafe{msg_send![PassiveButton::alloc(mtm),initWithFrame:rect([1.,1.,22.,22.])]};
        button.setBordered(false);button.setTag(100);button.setImagePosition(NSCellImagePosition::ImageOnly);button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        let accessory_image=accessory_image(mtm)?;
        button.setImage(Some(&*accessory_image));
        button.setIdentifier(Some(&NSString::from_str("inklow.accessory.menu")));
        unsafe {button.setTarget(Some(&target));button.setAction(Some(sel!(pressed:)));let _:()=msg_send![&*button,setAccessibilityIdentifier:&*NSString::from_str("inklow.accessory.menu")];}
        let surface=NSView::initWithFrame(NSView::alloc(mtm),rect([0.,0.,24.,24.]));surface.addSubview(&button);accessory.setContentView(Some(&surface));
        let mut this=Self{mtm,ghost,chooser,accessory,button,target,show_accessory:true,caret:None,field:None,docs:None,ghost_state:None,chooser_state:None,ghost_event:None,chooser_event:None,environment:Value::Null,closed:false};this.environment=this.environment_signature();this.label(false);Ok(this)
    }
    pub fn configure(&mut self,show_accessory:bool,_accept_key:&str) {self.show_accessory=show_accessory;self.place_accessory();}
    /// The shell builds this minimal native menu with captured app/site/token
    /// intents and retains its own action target through synchronous tracking.
    pub fn set_field_menu(&mut self,menu:&NSMenu) {self.target.ivars().menu.replace(unsafe{Retained::retain(menu as *const _ as *mut _)});}
    fn environment_signature(&self)->Value {
        let screens:Vec<Value>=NSScreen::screens(self.mtm).iter().map(|screen|{
            let frame=screen.frame();let visible=screen.visibleFrame();
            serde_json::json!([frame.origin.x,frame.origin.y,frame.size.width,frame.size.height,visible.origin.x,visible.origin.y,visible.size.width,visible.size.height,screen.backingScaleFactor()])
        }).collect();
        let workspace=NSWorkspace::sharedWorkspace();
        let appearance=self.ghost.effectiveAppearance().bestMatchFromAppearancesWithNames(&objc2_foundation::NSArray::from_slice(&[unsafe{NSAppearanceNameAqua},unsafe{NSAppearanceNameDarkAqua}])).map(|name|name.to_string());
        serde_json::json!([screens,appearance,workspace.accessibilityDisplayShouldIncreaseContrast(),workspace.accessibilityDisplayShouldReduceTransparency()])
    }
    fn label(&mut self,docs:bool) {
        let label=if docs{"BroTypist Google Docs accessibility help"}else{"BroTypist menu"};
        let tip=if docs{"Google Docs accessibility help — open the menu for setup. Completion support is not verified. Option-click sends a normal Tab."}else{"BroTypist — typing settings and pause controls. Option-click sends a normal Tab."};
        unsafe{let _:()=msg_send![&*self.button,setAccessibilityLabel:&*NSString::from_str(label)];}self.button.setToolTip(Some(&NSString::from_str(tip)));
    }
    fn place_accessory(&mut self) {
        let anchor=self.docs.or_else(||self.field.or(self.caret).map(|field|{let line=self.caret.unwrap_or(field);[field[0],line[1],field[2],line[3]]}));
        if self.show_accessory {if let Some(frame)=anchor.and_then(|r|position(self.mtm,r,24.,24.,true)){self.accessory.setFrame_display(frame,true);self.accessory.orderFrontRegardless();return;}}
        self.accessory.orderOut(None);
    }
    fn clear(&mut self,hide:bool,panel:Option<&str>) {
        if panel.is_none()||panel==Some("ghost"){self.ghost.orderOut(None);self.ghost_state=None;self.ghost_event=None;}
        if panel.is_none()||panel==Some("chooser"){self.chooser.orderOut(None);self.chooser_state=None;self.chooser_event=None;self.target.ivars().enabled.borrow_mut().clear();}
        if hide {self.caret=None;self.field=None;self.docs=None;self.label(false);self.accessory.orderOut(None);}
    }
    fn ack(event:&Value,text:String,mode:&str)->Option<Command> {
        Some(Command::PresentationResult{key:serde_json::from_value::<SnapshotKey>(event.get("key")?.clone()).ok()?,authority:serde_json::from_value::<Authority>(event.get("authority")?.clone()).ok()?,revision:event.get("revision")?.as_u64()?,visible_text:text,mode:mode.into()})
    }
    pub fn handle(&mut self,event:&Value)->Result<Vec<Command>> {
        if self.closed{return Ok(Vec::new());} let mut commands=Vec::new();
        let environment=self.environment_signature();
        if environment!=self.environment {
            self.environment=environment;
            self.ghost.orderOut(None);self.chooser.orderOut(None);self.accessory.orderOut(None);
        }
        match event["type"].as_str().unwrap_or("") {
            "clear"=>self.clear(event["hide_accessory"].as_bool().unwrap_or(false),event["panel"].as_str()),
            "caret"=>{self.docs=None;self.label(false);self.caret=finite_rect(&event["rect"]);self.field=finite_rect(&event["field_rect"]);if self.caret.is_none()&&self.field.is_none(){self.clear(true,None);}else{self.place_accessory();}},
            "docs_help"=>{if event["hint"].is_null(){if self.docs.is_some(){self.clear(true,None);}}else{self.clear(true,None);self.docs=finite_rect(&event["hint"]["anchor"]);self.label(true);self.place_accessory();}},
            "suggestion"=>{
                let mode=event["mode"].as_str().unwrap_or("completion");let choices:Vec<String>=event["choices"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|s|!s.is_empty()).take(9).map(str::to_owned).collect();
                let choice_mode=if matches!(mode,"completion"|"correction"){event["choices_mode"].as_str().unwrap_or(mode)}else{mode};
                if finite_rect(&event["rect"]).is_none(){self.clear(true,None);if let Some(c)=Self::ack(event,String::new(),mode){commands.push(c);}if !choices.is_empty()&&choice_mode!=mode{if let Some(c)=Self::ack(event,String::new(),choice_mode){commands.push(c);}}return Ok(commands);}
                self.docs=None;self.label(false);self.caret=finite_rect(&event["rect"]);self.field=finite_rect(&event["field_rect"]);self.place_accessory();
                if matches!(mode,"completion"|"correction") {
                    let identity=serde_json::json!([event["text"],mode,event["rect"],event["render"],event["inline"],event["advance"],event["correction"],self.environment_signature()]);
                    if !cached(&identity,self.ghost_state.as_ref(),visible(&self.ghost)){let text=self.present_ghost(event,mode);self.ghost_state=Some((identity,text));}
                    self.ghost_event=Some(event.clone());if let Some(c)=Self::ack(event,self.ghost_state.as_ref().map(|s|s.1.clone()).unwrap_or_default(),mode){commands.push(c);}
                    if !choices.is_empty(){self.present_chooser(event,&choices,choice_mode,&mut commands);}
                }else{let rows=if choices.is_empty(){vec![event["text"].as_str().unwrap_or("").to_owned()]}else{choices};self.present_chooser(event,&rows,mode,&mut commands);}
            },_=>{}
        }
        commands.extend(self.target.ivars().queue.take());Ok(commands)
    }
    fn present_chooser(&mut self,event:&Value,rows:&[String],mode:&str,commands:&mut Vec<Command>) {
        let rows:Vec<String>=rows.iter().filter(|s|!s.is_empty()).take(9).cloned().collect();
        let identity=serde_json::json!([rows,mode,event["rect"],event["field_anchor"],self.environment_signature()]);
        if !cached(&identity,self.chooser_state.as_ref(),visible(&self.chooser)) {
            let text=if rows.is_empty(){self.chooser.orderOut(None);String::new()}else{
                let w=(rows.iter().map(|r|chip_text(r).chars().count()).max().unwrap_or(0)as f64*7.+60.).clamp(180.,480.);let h=rows.len()as f64*30.+8.;
                let surface=NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(self.mtm),rect([0.,0.,w,h]));surface.setMaterial(NSVisualEffectMaterial::Popover);surface.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
                let workspace=NSWorkspace::sharedWorkspace();surface.setState(if workspace.accessibilityDisplayShouldReduceTransparency(){NSVisualEffectState::Inactive}else{NSVisualEffectState::Active});
                surface.setWantsLayer(true);unsafe{if let Some(layer)=surface.layer(){let _:()=msg_send![&*layer,setCornerRadius:8.0f64];let _:()=msg_send![&*layer,setMasksToBounds:true];let border:f64=if workspace.accessibilityDisplayShouldIncreaseContrast(){2.}else{1.};let _:()=msg_send![&*layer,setBorderWidth:border];let color:*const c_void=msg_send![&*NSColor::separatorColor(),CGColor];let _:()=msg_send![&*layer,setBorderColor:color];if workspace.accessibilityDisplayShouldReduceTransparency(){let bg:*const c_void=msg_send![&*NSColor::windowBackgroundColor(),CGColor];let _:()=msg_send![&*layer,setBackgroundColor:bg];}}}
                let font=NSFont::systemFontOfSize(13.);let count=visible_choices(&rows,w-40.,|s|width(&font,s));self.target.ivars().enabled.replace((1..=count).collect());
                for (i,row)in rows.iter().enumerate(){let button:Retained<PassiveButton>=unsafe{msg_send![PassiveButton::alloc(self.mtm),initWithFrame:rect([10.,h-(i+1)as f64*30.-4.,w-20.,30.])]};button.setTitle(&NSString::from_str(&format!("{}   {}",i+1,chip_text(row))));button.setFont(Some(&font));button.setContentTintColor(Some(&NSColor::labelColor()));button.setAlignment(NSTextAlignment::Left);button.setBordered(false);button.setEnabled(i<count);button.setTag((i+1)as isize);button.setIdentifier(Some(&NSString::from_str(&format!("inklow.suggestion.choice.{}",i+1))));button.setToolTip(Some(&NSString::from_str(row)));unsafe{button.setTarget(Some(&self.target));button.setAction(Some(sel!(pressed:)));let _:()=msg_send![&*button,setAccessibilityIdentifier:&*NSString::from_str(&format!("inklow.suggestion.choice.{}",i+1))];let _:()=msg_send![&*button,setAccessibilityLabel:&*NSString::from_str(&format!("Accept choice {}: {}",i+1,row))];if let Some(cell)=button.cell(){let _:()=msg_send![&*cell,setWraps:false];let _:()=msg_send![&*cell,setLineBreakMode:4usize];}}surface.addSubview(&button);}
                self.chooser.setContentView(Some(&surface));if let Some(mut anchor)=finite_rect(&event["rect"]){if event["field_anchor"].as_bool()==Some(true){anchor[2]=0.;}if let Some(frame)=position(self.mtm,anchor,w,h,false){self.chooser.setFrame_display(frame,true);self.chooser.orderFrontRegardless();self.chooser.displayIfNeeded();}}
                if visible(&self.chooser){rows[..count].join("\n")}else{String::new()}
            };self.chooser_state=Some((identity,text));
        }
        self.chooser_event=Some(event.clone());if let Some(c)=Self::ack(event,self.chooser_state.as_ref().map(|s|s.1.clone()).unwrap_or_default(),mode){commands.push(c);}
    }
    fn present_ghost(&mut self,event:&Value,mode:&str)->String {
        let result=(||->Option<String>{
            let caret=finite_rect(&event["rect"])?;let render:InlineRender=serde_json::from_value(event["render"].clone()).ok()?;
            if !render.valid(Some(caret))||(mode=="completion"&&event["inline"].as_bool()!=Some(true)){return None;}
            let font=resolve_font(&render)?;let text=event["text"].as_str().unwrap_or("").split(['\n','\r']).next()?;let advance=event["advance"].as_str().unwrap_or("");if text.is_empty()||advance.contains(['\r','\n']){return None;}
            let x=caret[0]+width(&font,advance);let [cx,cy,cw,ch]=render.clip_rect;let available=cx+cw-x-render.suffix_width;let mut left=x;let mut gap=0.;let mut strike=None;
            let drawn=if mode=="correction"{let r=finite_rect(&event["correction"]["rect"])?;if r[2]<=0.||!advance.is_empty()||!(r[1]<render.baseline&&render.baseline<=r[1]+r[3]+1.)||r[0]<cx||r[0]+r[2]>x+1.5{return None;}if(r[0]+r[2]-x).abs()<=1.5{gap=width(&font," ");}if width(&font,text)+gap>available{return None;}left=r[0];strike=Some(r);text}else{fit_text(text,available,|s|width(&font,s))};if drawn.is_empty(){return None;}
            let ghost_width=width(&font,drawn);let right=x+gap+ghost_width+render.suffix_width;let displaced=!render.suffix.trim().is_empty();let mut top=render.baseline-render.ascent;let mut bottom=render.baseline+render.descent;if displaced{top=top.min(caret[1]);bottom=bottom.max(caret[1]+caret[3]);}
            if left<cx||right>cx+cw+0.5||top<cy||bottom>cy+ch+1.{return None;}
            let screens=NSScreen::screens(self.mtm);let first=screens.firstObject()?;let origin_top=first.frame().origin.y+first.frame().size.height;let cocoa_bottom=origin_top-bottom;let screen=screens.iter().find(|s|contains_point(s.frame(),left,cocoa_bottom))?;let scale=screen.backingScaleFactor();if !scale.is_finite()||scale<=0.{return None;}
            let fx=(left*scale).floor()/scale;let fy=(cocoa_bottom*scale).floor()/scale;let mut dx=left-fx;let mut dy=cocoa_bottom-fy;let frame=rect([fx,fy,((right-left+dx)*scale).ceil()/scale,((bottom-top+dy)*scale).ceil()/scale]);if !screens.iter().any(|s|contains_rect(s.frame(),frame)){return None;}
            self.ghost.setFrame_display(frame,false);
            // NSWindow can quantize origins. Correct the owned drawing origin,
            // never host geometry; strict readback bounds the subpoint residual.
            let actual=self.ghost.frame();let ex=fx-actual.origin.x;let ey=fy-actual.origin.y;if ex.abs()>1./scale+0.001||ey.abs()>1./scale+0.001{return None;}dx+=ex;dy+=ey;
            if dx<0.||dy<0.||right-left+dx>actual.size.width+0.001||bottom-top+dy>actual.size.height+0.001{return None;}
            let workspace=NSWorkspace::sharedWorkspace();let contrast=workspace.accessibilityDisplayShouldIncreaseContrast();let color=if let(Some(ink),Some(bg))=(render.text_rgb,render.background_rgb){let share=if contrast{0.6}else{0.42};rgb(std::array::from_fn(|i|bg[i]+(ink[i]-bg[i])*share))}else{let dark=self.ghost.effectiveAppearance().bestMatchFromAppearancesWithNames(&objc2_foundation::NSArray::from_slice(&[unsafe{NSAppearanceNameAqua},unsafe{NSAppearanceNameDarkAqua}])).is_some_and(|n|n.to_string()==unsafe{NSAppearanceNameDarkAqua}.to_string());rgb(match(dark,contrast){(true,true)=>[154./255.,156./255.,163./255.],(false,true)=>[110./255.,112./255.,120./255.],(true,false)=>[108./255.,111./255.,119./255.],_=>[162./255.,164./255.,170./255.]})};
            let baseline=dy+bottom-render.baseline;let gx=dx+x-left+gap;let mut runs=vec![DrawRun{text:drawn.into(),x:gx,color:color.clone()}];let mut fills=Vec::new();if displaced{fills.push((rect([dx+x-left,dy,right-x,bottom-top]),rgb(render.background_rgb?)));runs.push(DrawRun{text:render.suffix.clone(),x:gx+ghost_width,color:rgb(render.text_rgb?)});}
            if let Some(strike)=strike{let thickness=font.underlineThickness().max(1.);let middle=baseline+font.xHeight()/2.;fills.push((rect([dx,middle-thickness/2.,strike[2],thickness]),color));}
            let surface=GhostSurface::new(self.mtm,actual.size,GhostIvars{font,baseline,runs,fills});self.ghost.setContentView(Some(&surface));self.ghost.orderFrontRegardless();self.ghost.displayIfNeeded();visible(&self.ghost).then(||drawn.to_owned())
        })();if let Some(text)=result{text}else{self.ghost.orderOut(None);String::new()}
    }
    pub fn tick(&mut self)->Result<Vec<Command>> {
        if self.closed{return Ok(Vec::new());}let mut commands=self.target.ivars().queue.take();
        let environment=self.environment_signature();
        if environment!=self.environment {
            self.environment=environment;
            self.ghost.orderOut(None);self.chooser.orderOut(None);self.accessory.orderOut(None);
        }
        // Revoke stale authority immediately when native visibility/Space is lost.
        // A subsequent same-content event retries rendering rather than caching refusal.
        if !visible(&self.ghost){if let Some((_,text))=self.ghost_state.as_mut(){if !text.is_empty(){text.clear();if let Some(event)=&self.ghost_event{if let Some(c)=Self::ack(event,String::new(),event["mode"].as_str().unwrap_or("completion")){commands.push(c);}}}}}
        if !visible(&self.chooser){if let Some((_,text))=self.chooser_state.as_mut(){if !text.is_empty(){text.clear();self.target.ivars().enabled.borrow_mut().clear();if let Some(event)=&self.chooser_event{let mode=event["mode"].as_str().unwrap_or("completion");let mode=if matches!(mode,"completion"|"correction"){event["choices_mode"].as_str().unwrap_or(mode)}else{mode};if let Some(c)=Self::ack(event,String::new(),mode){commands.push(c);}}}}}
        Ok(commands)
    }
    pub fn close(&mut self)->Result<()> {if !self.closed{self.clear(true,None);self.target.ivars().menu.take();self.target.ivars().queue.borrow_mut().clear();unsafe{self.button.setTarget(None);self.button.setAction(None);}self.ghost.setContentView(None);self.chooser.setContentView(None);self.accessory.setContentView(None);self.ghost.close();self.chooser.close();self.accessory.close();self.closed=true;}Ok(())}
}
impl Drop for OverlayOwner {fn drop(&mut self){let _=self.close();}}

#[cfg(test)]mod tests {
    use super::*;
    #[test]fn whole_word_clipping(){let measure=|s:&str|s.chars().count()as f64;assert_eq!(fit_text(" see you tomorrow",9.,measure)," see you");assert_eq!(fit_text(" see",3.,measure),"");assert_eq!(fit_text("  amanhã depois",9.,measure),"  amanhã");assert_eq!(fit_text("abc",f64::NAN,measure),"");}
    #[test]fn exact_chooser_prefix(){let rows=vec!["tomorrow".into(),"a deliberately long choice that exceeds the display limit".into(),"later".into()];assert_eq!(visible_choices(&rows,440.,|s|s.chars().count()as f64*7.),1);assert_eq!(visible_choices(&["W".repeat(43),"later".into()],320.,|s|s.chars().count()as f64*12.),0);}
    #[test]fn invalid_rectangles(){assert!(finite_rect(&serde_json::json!([1,2,0,16])).is_some());for v in [serde_json::json!([1,2,-1,16]),serde_json::json!([1,2,1,0]),serde_json::json!([1,2,3]),Value::Null]{assert!(finite_rect(&v).is_none());}}
    #[test]fn hidden_or_refused_content_retries(){let id=serde_json::json!(["text",1]);let old=(id.clone(),"text".into());assert!(cached(&id,Some(&old),true));assert!(!cached(&id,Some(&old),false));assert!(!cached(&id,Some(&(id.clone(),String::new())),true));}
    #[test]fn acknowledgment_requires_all_source_authority(){let snapshot=crate::core::Snapshot::default();let event=serde_json::json!({"key":snapshot.key(),"authority":snapshot.authority(),"revision":17});assert!(SelfTest::ack(&event));let mut missing=event.clone();missing.as_object_mut().unwrap().remove("authority");assert!(!SelfTest::ack(&missing));}
    struct SelfTest;impl SelfTest{fn ack(v:&Value)->bool{OverlayOwner::ack(v," visible".into(),"completion").is_some()}}
}
