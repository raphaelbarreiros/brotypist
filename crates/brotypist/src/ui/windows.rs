//! Win32 resident shell. All UI/controller/clipboard work stays on this thread;
//! model evaluation belongs to the isolated runtime and preview processes.
#[path="windows_settings.rs"] mod settings;
#[path="windows_overlay.rs"] mod overlay;
#[path="windows_accessory.rs"] mod accessory;
#[path="windows_preview.rs"] mod owned_preview;
#[cfg(feature="native-smoke")]
#[path="windows_smoke.rs"] mod native_smoke;
#[cfg(feature="native-smoke")]
pub fn smoke(output:&std::path::Path)->Result<i32>{native_smoke::run(output)}
use crate::{config::Config, runtime::{Controller,Command}, ui::{LaunchOptions,settings::SettingsDraft}, preview::PreviewSession, platform::Action};
use anyhow::{Result,ensure};
use serde_json::{Value,json};
use std::{cell::RefCell,collections::VecDeque};
use windows::{core::{PCWSTR,w},Win32::{Foundation::*,Graphics::Gdi::*,System::LibraryLoader::GetModuleHandleW,UI::{WindowsAndMessaging::*,Controls::*,Controls::Dialogs::*,Input::KeyboardAndMouse::*,Shell::*,HiDpi::*}}};
use settings::{Kind,FIELDS,PANES,RULE_FLAGS,WelcomeState};
const TRAY:u32=WM_APP+14;
const ROOT_PANE:usize=10; const SAVE:usize=11;const REVERT:usize=12;const QUIT:usize=13;const WELCOME:usize=14;
const MODEL_LIST:usize=20;const MODEL_SELECT:usize=21;const MODEL_DOWNLOAD:usize=22;const MODEL_CANCEL:usize=23;const MODEL_IMPORT:usize=24;const MODEL_REVEAL:usize=25;const MODEL_LICENSE:usize=26;
const MODEL_PROGRESS:usize=27;const UPDATE_PROGRESS:usize=28;const SETTINGS_FOLDER:usize=79;
const RULE_LIST:usize=40;const RULE_KIND:usize=41;const RULE_ID:usize=42;const RULE_INSTRUCTIONS:usize=43;const RULE_USE_INSTRUCTIONS:usize=44;const RULE_APPLY:usize=45;const RULE_REMOVE:usize=46;const RULE_NEW:usize=47;
const COUNT:usize=60;const DELETE_ALL:usize=61;const DELETE_SCOPE:usize=62;const STATS:usize=63;const CLEAR_STATS:usize=64;const PERIOD:usize=65;const GROUP:usize=66;const RESULTS:usize=67;
const UPDATE_CHECK:usize=70;const UPDATE_DOWNLOAD:usize=71;const UPDATE_INSTALL:usize=72;const UPDATE_CANCEL:usize=73;const UPDATE_STATUS:usize=74;const PERMISSIONS:usize=75;const DIAGNOSTICS:usize=76;const SOURCE:usize=77;
const W_BACK:usize=80;const W_NEXT:usize=81;const W_TEXT:usize=82;const W_EDIT:usize=83;const W_SUGGESTION:usize=84;const W_WORD:usize=85;const W_ALL:usize=86;const W_LANGUAGE:usize=87;
const FIELD_BASE:usize=1000;const SHORTCUT_BASE:usize=2000;const RULE_FLAG_BASE:usize=2100;
const ACTIONS:&[(&str,&str)]=&[("word","Accept next word"),("all","Accept all"),("force","Request completion"),("app_toggle","Toggle current app"),("toggle","Toggle predictions"),("alternatives","Alternative words"),("synonyms","Selected-text synonyms")];
thread_local! {static EVENTS:RefCell<VecDeque<Event>>=const{RefCell::new(VecDeque::new())};}
#[derive(Clone,Copy)]enum Event{Command(usize,u16),Close,Tick,Resize,Scroll(i32),ScrollTo(i32),Display,Tray(u32),Accessory}
fn queue(event:Event){EVENTS.with(|q|q.borrow_mut().push_back(event));}
pub(super) fn wide(s:&str)->Vec<u16>{s.encode_utf16().chain(Some(0)).collect()}
unsafe extern "system" fn procedure(hwnd:HWND,msg:u32,wp:WPARAM,lp:LPARAM)->LRESULT{
    match msg{
        WM_COMMAND=>queue(Event::Command(wp.0&0xffff,((wp.0>>16)&0xffff)as u16)),
        WM_CLOSE=>{queue(Event::Close);return LRESULT(0)},
        WM_TIMER=>queue(Event::Tick),WM_SIZE=>queue(Event::Resize),
        WM_DISPLAYCHANGE|WM_DPICHANGED|WM_SETTINGCHANGE=>queue(Event::Display),
        WM_MOUSEWHEEL=>queue(Event::Scroll(-((wp.0>>16)as u16 as i16 as i32)/3)),
        WM_VSCROLL=>{let kind=wp.0&0xffff;if kind==4||kind==5{queue(Event::ScrollTo((wp.0>>16)as i32));}else{queue(Event::Scroll(match kind{0=>-32,1=>32,2=>-400,3=>400,_=>0}));}},
        TRAY=>queue(Event::Tray(lp.0 as u32)),
        WM_DESTROY=>{unsafe{PostQuitMessage(0);}return LRESULT(0)},_=>{}
    }
    unsafe{DefWindowProcW(hwnd,msg,wp,lp)}
}
struct Control{hwnd:HWND,id:usize,pane:usize,y:i32,height:i32,x:i32,width:i32}
struct Shell{
    window:HWND,instance:HINSTANCE,controller:Controller,draft:SettingsDraft,controls:Vec<Control>,pane:usize,scroll:i32,loading:bool,last_config:Config,acknowledged:bool,
    tray:bool,tray_requested:bool,tray_icon:HICON,taskbar_created:u32,overlay:overlay::Overlay,chooser:ChoiceWindow,accessory:accessory::Accessory,
    models:Vec<Value>,rules:Vec<String>,editing_rule:Option<String>,recording:Option<usize>,
    rule_original:Value,
    welcome:Option<WelcomeState>,preview:Option<PreviewSession>,preview_closing:bool,ime:bool,preview_navigation:Option<bool>,close_pending:bool,
    preview_ghost:Option<owned_preview::PreviewGhost>,
    closing:bool,updates:crate::updates::UpdateService,update_intent:Value,status:Value,
    power:crate::system::PowerStatus,
}
impl Shell{
    fn new(config:Config,options:&LaunchOptions,instance:HINSTANCE)->Result<Self>{unsafe{
        let window=CreateWindowExW(WS_EX_CONTROLPARENT,w!("BroTypist.Settings"),w!("BroTypist — Settings"),WS_OVERLAPPEDWINDOW|WS_VSCROLL, CW_USEDEFAULT,CW_USEDEFAULT,920,760,None,None,Some(instance),None)?;
        let icon_path=std::env::current_exe()?.parent().unwrap_or(std::path::Path::new(".")).join("assets/app.ico");
        let icon=LoadImageW(None,PCWSTR(wide(&icon_path.to_string_lossy()).as_ptr()),IMAGE_ICON,32,32,LR_LOADFROMFILE).map(|h|HICON(h.0)).or_else(|_|LoadIconW(None,IDI_APPLICATION))?;
        SendMessageW(window,WM_SETICON,Some(WPARAM(ICON_BIG as usize)),Some(LPARAM(icon.0 as isize)));
        let draft=SettingsDraft::new(&config)?;let last_config=config.clone();let controller=Controller::start(config)?;
        let mut s=Self{window,instance,controller,draft,last_config,acknowledged:false,controls:vec![],pane:options.pane.as_deref().and_then(|p|PANES.iter().position(|v|v.to_lowercase().replace(' ',"_")==p || p=="models"&&*v=="AI Models")).unwrap_or(0),scroll:0,loading:false,tray:false,tray_requested:!options.no_tray,tray_icon:icon,taskbar_created:RegisterWindowMessageW(w!("TaskbarCreated")),overlay:overlay::Overlay::new(instance)?,chooser:ChoiceWindow::new(instance)?,accessory:accessory::Accessory::new(instance)?,models:vec![],rules:vec![],editing_rule:None,rule_original:Value::Null,recording:None,welcome:None,preview:None,preview_ghost:None,preview_closing:false,ime:false,preview_navigation:None,close_pending:false,closing:false,updates:crate::updates::UpdateService::new()?,update_intent:Value::Null,status:Value::Null,power:Default::default()};
        s.build()?;s.preview_ghost=Some(owned_preview::PreviewGhost::new(s.h(W_EDIT))?);s.new_rule();s.refresh_controls()?;if !options.no_tray{s.tray=s.install_tray();}
        if !s.controller.config().setup_completed&&!options.login_startup{s.show_welcome()?;}else if !options.background||!s.tray{let _=ShowWindow(window,SW_SHOW);}
        if s.last_config.check_updates_automatically{let _=s.updates.start_automatic_check();}
        s.controller.send(Command::Models)?;ensure!(SetTimer(Some(window),1,30,None)!=0,"The Windows event timer could not start.");Ok(s)
    }}
    fn control(&mut self,id:usize,pane:usize,class:&str,text:&str,style:WINDOW_STYLE,x:i32,y:i32,width:i32,height:i32)->Result<HWND>{unsafe{
        let h=CreateWindowExW(WINDOW_EX_STYLE(0),PCWSTR(wide(class).as_ptr()),PCWSTR(wide(text).as_ptr()),WS_CHILD|style,x,y,width,height,Some(self.window),Some(HMENU(id as *mut _)),Some(self.instance),None)?;
        SendMessageW(h,WM_SETFONT,Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),Some(LPARAM(1)));
        self.controls.push(Control{hwnd:h,id,pane,y,height,x,width});Ok(h)
    }}
    fn button(&mut self,id:usize,pane:usize,title:&str,y:i32)->Result<HWND>{self.control(id,pane,"BUTTON",title,WS_TABSTOP|WINDOW_STYLE(BS_PUSHBUTTON as u32),32,y,350,30)}
    fn label(&mut self,pane:usize,title:&str,y:i32,height:i32)->Result<HWND>{self.control(0,pane,"STATIC",title,WINDOW_STYLE(0),32,y,790,height)}
    fn combo(&mut self,id:usize,pane:usize,label:&str,values:&[&str],y:i32)->Result<HWND>{self.label(pane,label,y,22)?;let h=self.control(id,pane,"COMBOBOX", "",WS_TABSTOP|WS_VSCROLL|WINDOW_STYLE(CBS_DROPDOWNLIST as u32),32,y+24,720,260)?;for value in values{unsafe{SendMessageW(h,CB_ADDSTRING,None,Some(LPARAM(wide(value).as_ptr() as isize)));}}set_selection(h,0);Ok(h)}
    fn build(&mut self)->Result<()>{
        self.combo(ROOT_PANE,usize::MAX,"Settings",PANES,8)?;
        self.button(SAVE,usize::MAX,"&Save",0)?;self.button(REVERT,usize::MAX,"&Revert",0)?;self.button(QUIT,usize::MAX,"&Quit BroTypist",0)?;
        let mut y=[100;6];
        for (i,f) in FIELDS.iter().enumerate(){let yy=y[f.pane];match f.kind{
            Kind::Check=>{self.control(FIELD_BASE+i,f.pane,"BUTTON",f.label,WS_TABSTOP|WINDOW_STYLE(BS_AUTOCHECKBOX as u32),32,yy,790,30)?;y[f.pane]+=38;},
            Kind::Choice(values)=>{self.combo(FIELD_BASE+i,f.pane,f.label,values,yy)?;y[f.pane]+=64;},
            Kind::Number=>{
                self.label(f.pane,f.label,yy,22)?;
                let edit=self.control(FIELD_BASE+i,f.pane,"EDIT","",WS_TABSTOP|WS_BORDER|WINDOW_STYLE((ES_AUTOHSCROLL|ES_NUMBER)as u32),32,yy+24,710,26)?;
                let spin=self.control(3000+i,f.pane,"msctls_updown32","",WINDOW_STYLE(UDS_SETBUDDYINT|UDS_ARROWKEYS|UDS_NOTHOUSANDS),744,yy+24,36,26)?;
                let(low,high)=number_bounds(f.key);
                unsafe{
                    SendMessageW(spin,UDM_SETBUDDY,Some(WPARAM(edit.0 as usize)),None);
                    SendMessageW(spin,UDM_SETRANGE32,Some(WPARAM(low as usize)),Some(LPARAM(high as isize)));
                }
                y[f.pane]+=64;
            },
            _=>{self.label(f.pane,f.label,yy,22)?;let multiline=matches!(f.kind,Kind::Multiline);self.control(FIELD_BASE+i,f.pane,"EDIT","",WS_TABSTOP|WS_BORDER|WINDOW_STYLE(ES_AUTOHSCROLL as u32)|if multiline{WS_VSCROLL|WINDOW_STYLE((ES_MULTILINE|ES_AUTOVSCROLL|ES_WANTRETURN) as u32)}else{WINDOW_STYLE(0)},32,yy+24,750,if multiline{96}else{26})?;y[f.pane]+=if multiline{132}else{64};}
        }}
        let mut yy=y[2]+12;self.label(2,"Shortcuts: select a button, then press a chord. Escape cancels; Backspace clears. Above-Tab uses the physical key. System-reserved shortcuts remain system-owned.",yy,48)?;yy+=56;
        for(i,(_,label))in ACTIONS.iter().enumerate(){self.label(2,label,yy,22)?;self.button(SHORTCUT_BASE+i,2,"Record shortcut",yy+24)?;yy+=64;}
        let mut yy=y[1]+12;self.label(1,"Catalog models retain their licenses. Capacity is advisory; imports reference user-owned files. Selecting or downloading never predicts in Welcome until the selected model is actually ready.",yy,48)?;yy+=56;
        self.combo(MODEL_LIST,1,"Local model catalog",&[],yy)?;yy+=68;self.label(1,"",yy,90)?;self.control(DIAGNOSTICS,1,"STATIC","",WINDOW_STYLE(0),32,yy,790,100)?;yy+=110;
        self.control(MODEL_PROGRESS,1,"msctls_progress32","Model download progress",WINDOW_STYLE(0),32,yy,750,20)?;yy+=28;
        for(id,title)in[(MODEL_SELECT,"Use selected installed model"),(MODEL_DOWNLOAD,"Download selected model…"),(MODEL_CANCEL,"Cancel download"),(MODEL_IMPORT,"Import GGUF file…"),(MODEL_LICENSE,"Read selected model license…"),(MODEL_REVEAL,"Reveal model files")]{self.button(id,1,title,yy)?;yy+=38;}
        let mut yy=y[4]+12;self.label(4,"Website URLs are unavailable on Windows. Website exclusions fail closed for recognized browsers. Exact app IDs and inherited rules stay separate. History is OS-encrypted; there is no plaintext fallback.",yy,52)?;yy+=60;
        self.control(RULE_LIST,4,"LISTBOX","",WS_TABSTOP|WS_BORDER|WS_VSCROLL|WINDOW_STYLE(LBS_NOTIFY as u32),32,yy,750,120)?;yy+=130;
        self.combo(RULE_KIND,4,"Rule type",&["Application","Website"],yy)?;yy+=64;self.label(4,"Exact executable/app ID or hostname",yy,22)?;self.control(RULE_ID,4,"EDIT","",WS_TABSTOP|WS_BORDER|WINDOW_STYLE(ES_AUTOHSCROLL as u32),32,yy+24,750,26)?;yy+=64;
        for(i,(_,label))in RULE_FLAGS.iter().enumerate(){self.combo(RULE_FLAG_BASE+i,4,label,&["Inherit","Enabled","Disabled"],yy)?;yy+=64;}
        self.control(RULE_USE_INSTRUCTIONS,4,"BUTTON","Override instructions (unchecked inherits; empty override is meaningful)",WS_TABSTOP|WINDOW_STYLE(BS_AUTOCHECKBOX as u32),32,yy,790,30)?;yy+=38;
        self.control(RULE_INSTRUCTIONS,4,"EDIT","",WS_TABSTOP|WS_BORDER|WS_VSCROLL|WINDOW_STYLE((ES_MULTILINE|ES_AUTOVSCROLL|ES_WANTRETURN)as u32),32,yy,750,96)?;yy+=106;
        for(id,title)in[(RULE_APPLY,"Keep rule in settings draft"),(RULE_NEW,"New inherited rule"),(RULE_REMOVE,"Remove selected rule from draft"),(COUNT,"Count encrypted history (explicit unlock)…"),(DELETE_SCOPE,"Forget selected app / website history…"),(DELETE_ALL,"Forget all learned writing…"),(PERMISSIONS,"Open system access information")]{self.button(id,4,title,yy)?;yy+=38;}
        let mut yy=y[0]+10;
        self.control(78,0,"EDIT","",WS_BORDER|WS_VSCROLL|WINDOW_STYLE((ES_MULTILINE|ES_AUTOVSCROLL|ES_READONLY)as u32),32,yy,750,120)?;yy+=132;
        self.combo(PERIOD,0,"Statistics period",&["day","week","month","all"],yy)?;yy+=64;set_selection(self.h(PERIOD),1);self.combo(GROUP,0,"Statistics grouping",&["day","app"],yy)?;yy+=64;self.button(STATS,0,"Refresh aggregate statistics",yy)?;yy+=38;self.button(CLEAR_STATS,0,"Clear aggregate statistics only…",yy)?;yy+=38;
        self.control(RESULTS,usize::MAX-1,"EDIT","",WS_BORDER|WS_VSCROLL|WINDOW_STYLE((ES_MULTILINE|ES_AUTOVSCROLL|ES_READONLY)as u32),32,yy,750,200)?;
        let mut yy=y[5]+10;self.label(5,&format!("BroTypist {} · Windows\r\nIndependent local typing assistance. No account, cloud sync or main writing editor. Unsupported protected, hidden, IME or unverified fields remain unchanged. Windows publication and OTA installation are held pending native evidence.",env!("CARGO_PKG_VERSION")),yy,94)?;yy+=102;
        self.control(UPDATE_STATUS,5,"STATIC","",WINDOW_STYLE(0),32,yy,790,100)?;yy+=110;
        self.control(UPDATE_PROGRESS,5,"msctls_progress32","Software update download progress",WINDOW_STYLE(0),32,yy,750,20)?;yy+=28;
        for(id,title)in[(UPDATE_CHECK,"Check signed update manifest"),(UPDATE_DOWNLOAD,"Download signed update"),(UPDATE_INSTALL,"Install update"),(UPDATE_CANCEL,"Cancel update operation"),(SOURCE,"Open source repository"),(SETTINGS_FOLDER,"Open settings folder"),(WELCOME,"Show six-step Welcome")]{self.button(id,5,title,yy)?;yy+=38;}
        self.label(3,"Language quality comes from the selected local model. Instructions can request English, Portuguese or another supported language; there is no separate quality selector. Native Windows correction depends on an available dictionary and replaceable control.",y[3]+10,80)?;
        self.label(6,"",100,180)?;self.control(W_TEXT,6,"STATIC","",WINDOW_STYLE(0),32,100,790,180)?;
        self.combo(W_LANGUAGE,6,"Owned preview language",&["English","Português"],300)?;
        self.control(88,6,"BUTTON","I have reviewed system access and its limitations",WS_TABSTOP|WINDOW_STYLE(BS_AUTOCHECKBOX as u32),32,320,790,30)?;
        self.control(W_EDIT,6,"EDIT","",WS_TABSTOP|WS_BORDER|WS_VSCROLL|WINDOW_STYLE((ES_MULTILINE|ES_AUTOVSCROLL|ES_WANTRETURN)as u32),32,370,750,110)?;
        self.control(W_SUGGESTION,6,"STATIC","",WINDOW_STYLE(0),32,490,750,64)?;
        self.button(W_WORD,6,"Accept next word in owned preview",562)?;self.button(W_ALL,6,"Accept full owned preview",600)?;
        self.button(W_BACK,6,"&Back",650)?;self.button(W_NEXT,6,"&Continue",688)?;
        Ok(())
    }
    fn h(&self,id:usize)->HWND{self.controls.iter().find(|c|c.id==id).map(|c|c.hwnd).unwrap_or_default()}
    fn layout(&mut self){unsafe{
        let mut r=RECT::default();let _=GetClientRect(self.window,&mut r);let height=(r.bottom-100).max(220);let width=(r.right-64).max(200);
        let active=self.welcome.as_ref().map_or(self.pane,|_|6);
        let base_max=self.controls.iter().filter(|c|c.pane==active).map(|c|c.y+c.height.min(100)).max().unwrap_or(height);
        let results=self.controls.iter().find(|c|c.id==RESULTS).map_or(0,|c|c.y+c.height);
        let max=if active==4{base_max+230}else if active==0{base_max.max(results)}else{base_max};
        self.scroll=self.scroll.clamp(0,(max-height).max(0));
        for c in &self.controls{
            let mut visible=c.pane==active;let mut y=c.y-self.scroll;let mut x=c.x;let mut w=c.width.min(width);let mut h=c.height;
            if (3000..3000+FIELDS.len()).contains(&c.id){x=(r.right-76).max(220);w=36;}
            if c.pane==usize::MAX{visible=self.welcome.is_none();if c.id==ROOT_PANE||c.id==0{y=c.y;w=if c.id==ROOT_PANE{width}else{c.width};}else{y=r.bottom-46;x=match c.id{SAVE=>32,REVERT=>180,_=>340};w=if c.id==QUIT{200}else{135};}}
            if c.id==RESULTS{visible=self.welcome.is_none()&&[0,4].contains(&active);y=if active==0{c.y-self.scroll}else{base_max+20-self.scroll};}
            if let Some(welcome)=&self.welcome {
                let step=welcome.step;
                if [W_EDIT,W_SUGGESTION,W_WORD,W_ALL,W_LANGUAGE].contains(&c.id){visible=step==4;}
                if c.id==88 {visible=step==2;}
                if c.id==PERMISSIONS {visible=step==2;y=380-self.scroll;}
                if [MODEL_LIST,MODEL_SELECT,MODEL_DOWNLOAD,MODEL_CANCEL,MODEL_IMPORT,MODEL_LICENSE,DIAGNOSTICS,MODEL_PROGRESS].contains(&c.id){visible=step==3;y=match c.id{MODEL_LIST=>300,DIAGNOSTICS=>350,MODEL_PROGRESS=>434,MODEL_SELECT=>460,MODEL_DOWNLOAD=>498,MODEL_CANCEL=>536,MODEL_IMPORT=>574,_=>612}-self.scroll;}
                if c.id==W_BACK{y=r.bottom-50;x=32;w=135;}
                if c.pane==6&&c.id==0{visible=false;}
                if c.id==QUIT{visible=true;x=(r.right/2-90).max(180);w=180;y=r.bottom-50;}
                if c.id==W_NEXT{y=r.bottom-50;x=r.right-200;w=168;}
            }
            if visible&&c.pane!=usize::MAX&&c.id!=W_BACK&&c.id!=W_NEXT{visible=y>=86&&y<r.bottom-60;}
            let _=SetWindowPos(c.hwnd,None,x,y,w,h,SWP_NOZORDER|SWP_NOACTIVATE);let _=ShowWindow(c.hwnd,if visible{SW_SHOW}else{SW_HIDE});
        }
        SetScrollRange(self.window,SB_VERT,0,(max-height).max(0),false);SetScrollPos(self.window,SB_VERT,self.scroll,true);
    }}
    fn refresh_controls(&mut self)->Result<()>{
        self.loading=true;
        for(i,f)in FIELDS.iter().enumerate(){
            let Some(v)=self.draft.value(f.key) else{continue}; let h=self.h(FIELD_BASE+i);
            match f.kind{
                Kind::Check=>check(h,v.as_bool().unwrap_or(false)),
                Kind::Choice(values)=>set_selection(h,values.iter().position(|s|v.as_str()==Some(*s)).unwrap_or(0)),
                _=>{
                    let value=if f.key=="excluded_apps"{v.as_array().map(|a|a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\r\n")).unwrap_or_default()}else{v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string())};
                    // Unchanged text must not reset an instruction editor's selection/undo state.
                    if text(h)!=value {set_text(h,&value);}
                }
            }
        }
        for(i,(action,label))in ACTIONS.iter().enumerate(){
            let chord=self.draft.value("shortcuts").and_then(|v|v.get(*action)).and_then(Value::as_str).unwrap_or("");
            set_text(self.h(SHORTCUT_BASE+i),&format!("{label}: {}",if chord.is_empty(){"Not assigned"}else{chord}));
        }
        self.refresh_rules();set_selection(self.h(ROOT_PANE),self.pane);self.loading=false;self.layout();Ok(())
    }
    fn refresh_rules(&mut self){self.rules=self.draft.value("app_rules").and_then(Value::as_object).map(|v|v.keys().cloned().collect()).unwrap_or_default();reset_list(self.h(RULE_LIST));for key in &self.rules{list_add(self.h(RULE_LIST),key);}}
    fn changed(&mut self,index:usize)->Result<()>{if self.loading{return Ok(())}let f=FIELDS[index];let h=self.h(FIELD_BASE+index);let v=settings::control_value(f,&text(h),is_checked(h),selection(h))?;if matches!(f.key,"clipboard_context"|"screen_context"|"screenshot_context"|"screenshot_appearance"|"record_history"|"terminal_completion")&&v==true&&self.draft.value(f.key)!=Some(&v){ensure!(self.confirm("Optional local data access","Enable this independent consent? Eligible focused content may be read locally. History requires OS-encrypted storage. You can revoke it without enabling any other context source."),"Consent not granted.");}self.draft.set(f.key,v)?;Ok(())}
    fn save(&mut self)->Result<()>{
        for i in 0..FIELDS.len(){self.changed(i)?;}
        if self.rule_dirty(){self.apply_rule()?;}
        let current=self.controller.config();let c=self.draft.apply_to(&current)?;
        let login_changed=c.launch_at_login!=current.launch_at_login;
        if login_changed{crate::system::set_launch_at_login(c.launch_at_login)?;}
        let result=(||->Result<()>{
            self.controller.send(Command::ApplyConfig{config:c.clone(),persist:true})?;
            for event in self.controller.poll(){self.runtime_event(event)?;}
            ensure!(self.controller.config()==c,"Settings could not be saved; your draft remains available.");
            Ok(())
        })();
        if result.is_err()&&login_changed{let _=crate::system::set_launch_at_login(current.launch_at_login);}
        result?;self.last_config=c.clone();self.draft.reset(&c)?;self.refresh_controls()
    }
    fn dirty_close(&mut self)->Result<bool>{
        let pending_number=FIELDS.iter().enumerate().any(|(i,f)|matches!(f.kind,Kind::Number)&&text(self.h(FIELD_BASE+i)).trim().parse::<u32>().is_err());
        if !self.draft.is_dirty()&&!self.rule_dirty()&&!pending_number{return Ok(true)}
        let answer=unsafe{MessageBoxW(Some(self.window),w!("Save your settings and rule editor before leaving?\nYes: Save · No: Discard · Cancel: Keep editing"),w!("Unsaved settings"),MB_YESNOCANCEL|MB_ICONQUESTION)};
        match answer{IDYES=>{self.save()?;Ok(true)},IDNO=>{self.draft.reset(&self.controller.config())?;self.new_rule();self.refresh_controls()?;Ok(true)},_=>Ok(false)}
    }
    fn confirm(&self,title:&str,message:&str)->bool{unsafe{MessageBoxW(Some(self.window),PCWSTR(wide(message).as_ptr()),PCWSTR(wide(title).as_ptr()),MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2)==IDYES}}
    fn error(&self,error:impl std::fmt::Display){unsafe{MessageBoxW(Some(self.window),PCWSTR(wide(&error.to_string()).as_ptr()),w!("BroTypist"),MB_OK|MB_ICONWARNING);}}
    fn show(&mut self){unsafe{let _=ShowWindow(self.window,SW_RESTORE);let _=SetForegroundWindow(self.window);}self.layout();}
    fn install_tray(&self)->bool{let mut n=NOTIFYICONDATAW{cbSize:std::mem::size_of::<NOTIFYICONDATAW>()as u32,hWnd:self.window,uID:1,uFlags:NIF_MESSAGE|NIF_ICON|NIF_TIP,uCallbackMessage:TRAY,hIcon:self.tray_icon,..Default::default()};let tip=wide("BroTypist — local typing assistance");n.szTip[..tip.len()].copy_from_slice(&tip);unsafe{Shell_NotifyIconW(NIM_ADD,&n).as_bool()}}
    fn tray_menu(&mut self)->Result<()>{
        self.controller.suspend_shortcuts(true)?;self.disarm();
        let status=self.controller.status();let app=status["current_app"].as_str().unwrap_or("").to_owned();let domain=status["current_domain"].as_str().unwrap_or("").to_owned();
        let foreground=unsafe{GetForegroundWindow()};
        let result=(||->Result<usize>{unsafe{
            let menu=NativeMenu(CreatePopupMenu()?);
            let selected_model=self.controller.config().model_path;
            AppendMenuW(menu.0,MF_STRING|MF_GRAYED,1,PCWSTR(wide(&format!("BroTypist — {}",status["message"].as_str().unwrap_or("Local typing assistance"))).as_ptr()))?;
            AppendMenuW(menu.0,MF_STRING|MF_GRAYED,2,PCWSTR(wide(&format!("Model: {selected_model}")).as_ptr()))?;
            let rows=[
                (0,String::new()),(108,format!("Predictions: {}",if self.controller.config().global_enabled{"On"}else{"Off"})),
                (109,"Resume now".into()),(110,"Pause for 5 minutes".into()),(102,"Pause for 15 minutes".into()),
                (111,"Pause for 1 hour".into()),(112,"Pause until tomorrow".into()),
                (103,format!("Toggle current app: {}",if app.is_empty(){"unavailable"}else{&app})),
                (0,String::new()),(104,"Request completion".into()),(113,"Alternative words".into()),(114,"Selected-text synonyms".into()),
                (0,String::new()),(100,"Settings…".into()),(101,"Welcome…".into()),(105,"Models…".into()),(106,"Privacy and learning…".into()),(115,"Check for Updates…".into()),
                (0,String::new()),(107,"Quit BroTypist".into())
            ];
            for(id,title)in rows{AppendMenuW(menu.0,if id==0{MF_SEPARATOR}else{MF_STRING},id,PCWSTR(wide(&title.replace('&',"&&")).as_ptr()))?;}
            let mut point=POINT::default();GetCursorPos(&mut point)?;let _=SetForegroundWindow(self.window);
            Ok(TrackPopupMenu(menu.0,TPM_RETURNCMD|TPM_RIGHTBUTTON,point.x,point.y,None,self.window,None).0 as usize)
        }})();
        unsafe{if GetForegroundWindow()==self.window&&IsWindow(Some(foreground)).as_bool(){let _=SetForegroundWindow(foreground);}}
        for event in self.controller.poll(){self.runtime_event(event)?;}
        self.controller.suspend_shortcuts(false)?;
        match result?{
            100=>self.show(),101=>self.show_welcome()?,
            102=>self.controller.send(Command::Pause{minutes:15})?,
            103 if !app.is_empty()=>self.controller.send(Command::ToggleApp{app,domain})?,
            104=>self.controller.send(Command::Action(Action::Force))?,
            105=>{self.pane=1;self.scroll=0;self.show();},
            106=>{self.pane=4;self.scroll=0;self.show();},
            107=>self.quit()?,
            108=>{let mut c=self.controller.config();c.global_enabled=!c.global_enabled;let enabled=c.global_enabled;self.controller.send(Command::ApplyConfig{config:c,persist:true})?;if enabled{self.controller.send(Command::Resume)?;}},
            109=>self.controller.send(Command::Resume)?,
            110=>self.controller.send(Command::Pause{minutes:5})?,
            111=>self.controller.send(Command::Pause{minutes:60})?,
            112=>self.controller.send(Command::Pause{minutes:tomorrow_minutes()?})?,
            113=>self.controller.send(Command::Action(Action::Alternatives))?,
            114=>self.controller.send(Command::Action(Action::Synonyms))?,
            115=>{self.updates.start_check()?;self.pane=5;self.scroll=0;self.show();},
            _=>{}
        }Ok(())
    }
    fn field_menu(&mut self)->Result<()>{
        let Some((app,domain))=self.accessory.target.clone()else{return Ok(())};
        self.controller.suspend_shortcuts(true)?;self.disarm();
        let paused=self.controller.status()["paused_remaining"].as_f64().is_some_and(|v|v>0.0);
        let result=(||->Result<usize>{unsafe{
            let menu=NativeMenu(CreatePopupMenu()?);
            for(id,title)in [(1,format!("Toggle predictions for {app}")),(2,if paused{"Resume predictions".into()}else{"Pause for 1 hour".into()}),(3,"Open Settings".into())]{
                AppendMenuW(menu.0,MF_STRING,id,PCWSTR(wide(&title.replace('&',"&&")).as_ptr()))?;
            }
            let mut point=POINT::default();GetCursorPos(&mut point)?;
            let action=TrackPopupMenu(menu.0,TPM_RETURNCMD|TPM_RIGHTBUTTON,point.x,point.y,None,self.window,None).0 as usize;
            Ok(action)
        }})();
        for event in self.controller.poll(){self.runtime_event(event)?;}
        self.controller.suspend_shortcuts(false)?;
        let current=self.controller.status();
        match result?{
            1 if current["current_app"]==app&&current["current_domain"]==domain=>self.controller.send(Command::ToggleApp{app,domain})?,
            2=>self.controller.send(if paused{Command::Resume}else{Command::Pause{minutes:60}})?,
            3=>self.show(),_=>{}
        }Ok(())
    }
    fn disarm(&mut self){
        let presented=self.overlay.current.is_some()||self.chooser.current.is_some();
        if presented{let _=self.controller.suspend_shortcuts(true);}
        if let Some(c)=self.overlay.hide(){let _=self.controller.send(c);}
        if let Some(c)=self.chooser.hide(){let _=self.controller.send(c);}
        // Cache interception is off before pixels disappear. Rearming is queued
        // after the empty receipts, never ahead of them.
        if presented&&self.recording.is_none()&&!self.closing{
            let _=self.controller.send(Command::SuspendShortcuts{suspended:false});
        }
    }
    fn quit(&mut self)->Result<()>{if !self.dirty_close()?{return Ok(())}self.closing=true;self.disarm();if let Some(p)=&mut self.preview{let _=p.close();}self.preview_closing=self.preview.is_some();self.updates.cancel();self.show();set_text(self.h(W_TEXT),"Stopping native hooks, background reads and inference processes…");Ok(())}
    fn selected_model(&self)->Result<Value>{self.models.get(selection(self.h(MODEL_LIST))).cloned().ok_or_else(||anyhow::anyhow!("Choose a catalog model."))}
    fn use_model(&mut self)->Result<()>{let m=self.selected_model()?;ensure!(m["installed"]==true,"Download the selected model first.");let c=self.controller.config();let mut pending=self.draft.apply_to(&c)?;pending.backend="gguf".into();pending.model_path=m["path"].as_str().unwrap_or("").into();pending.model_id=m["id"].as_str().unwrap_or("").into();pending.prompt_style=m["prompt_style"].as_str().unwrap_or("continuation").into();
        if let Some(w)=&mut self.welcome{w.select(pending.model_id.clone());w.ready=pending.model_id.clone();self.draft.set("backend",json!("gguf"))?;self.draft.set("model_path",json!(pending.model_path))?;self.draft.set("model_id",json!(pending.model_id))?;self.draft.set("prompt_style",json!(pending.prompt_style))?;}else{self.controller.send(Command::SetModel{path:pending.model_path,prompt_style:pending.prompt_style,model_id:Some(pending.model_id)})?;}
        self.refresh_controls()?;self.welcome_text();Ok(())}
    fn download(&mut self)->Result<()>{let m=self.selected_model()?;let accept=m["requires_license_acceptance"]!=true||self.confirm("Model license",&format!("{}\n{}\n\nAccept this model's license and download {} bytes?",m["license_name"].as_str().unwrap_or(""),m["license_url"].as_str().unwrap_or(""),m["size_bytes"]));ensure!(accept,"License was not accepted.");self.controller.send(Command::DownloadModel{id:m["id"].as_str().unwrap_or("").into(),accept_license:accept})}
    fn import(&mut self)->Result<()>{let Some(path)=choose_gguf(self.window)?else{return Ok(())};ensure!(std::path::Path::new(&path).is_file(),"Choose a readable GGUF file.");if let Some(w)=&mut self.welcome{w.select(path.clone());w.ready=path.clone();self.draft.set("model_path",json!(path))?;self.draft.set("backend",json!("gguf"))?;self.draft.set("model_id",json!(""))?;}else{self.controller.send(Command::SetModel{path,prompt_style:self.draft.value("prompt_style").and_then(Value::as_str).unwrap_or("continuation").into(),model_id:None})?;}self.refresh_controls()?;self.welcome_text();Ok(())}
    fn model_details(&mut self){if let Ok(m)=self.selected_model(){set_text(self.h(DIAGNOSTICS),&format!("{}\r\n{}\r\n{} bytes · {}\r\nLicense: {}\r\n{}",m["label"].as_str().unwrap_or(""),m["description"].as_str().unwrap_or(""),m["size_bytes"],if m["installed"]==true{"Installed"}else{"Not installed"},m["license_name"].as_str().unwrap_or(""),human_report(&m["availability"])));}}
    fn rule_snapshot(&self)->Value{
        json!({"kind":selection(self.h(RULE_KIND)),"identity":text(self.h(RULE_ID)),
            "flags":(0..RULE_FLAGS.len()).map(|i|selection(self.h(RULE_FLAG_BASE+i))).collect::<Vec<_>>(),
            "override":is_checked(self.h(RULE_USE_INSTRUCTIONS)),"instructions":text(self.h(RULE_INSTRUCTIONS))})
    }
    fn rule_dirty(&self)->bool{self.rule_snapshot()!=self.rule_original}
    fn resolve_rule_editor(&mut self)->Result<bool>{
        if !self.rule_dirty(){return Ok(true)}
        let answer=unsafe{MessageBoxW(Some(self.window),w!("Keep the current rule editor in the settings draft?\nYes: Keep · No: Discard editor · Cancel: Continue editing"),w!("Unsaved rule"),MB_YESNOCANCEL|MB_ICONQUESTION)};
        match answer{IDYES=>{self.apply_rule()?;Ok(true)},IDNO=>Ok(true),_=>Ok(false)}
    }
    fn load_rule(&mut self)->Result<()>{
        let i=unsafe{SendMessageW(self.h(RULE_LIST),LB_GETCURSEL,None,None).0};
        let Some(key)=usize::try_from(i).ok().and_then(|i|self.rules.get(i)).cloned()else{return Ok(())};
        if !self.resolve_rule_editor()?{return Ok(())}
        self.editing_rule=Some(key.clone());let(kind,identity)=key.split_once(':').unwrap_or(("app",&key));
        set_selection(self.h(RULE_KIND),usize::from(kind=="domain"));set_text(self.h(RULE_ID),identity);
        if let Some(r)=self.draft.value("app_rules").and_then(|r|r.get(&key)){
            for(i,(name,_))in RULE_FLAGS.iter().enumerate(){set_selection(self.h(RULE_FLAG_BASE+i),match r.get(*name).and_then(Value::as_bool){Some(true)=>1,Some(false)=>2,None=>0});}
            check(self.h(RULE_USE_INSTRUCTIONS),r.get("custom_instructions").is_some());set_text(self.h(RULE_INSTRUCTIONS),r["custom_instructions"].as_str().unwrap_or(""));
        }
        self.rule_original=self.rule_snapshot();Ok(())
    }
    fn new_rule(&mut self){
        self.editing_rule=None;set_text(self.h(RULE_ID),"");set_text(self.h(RULE_INSTRUCTIONS),"");check(self.h(RULE_USE_INSTRUCTIONS),false);
        for i in 0..RULE_FLAGS.len(){set_selection(self.h(RULE_FLAG_BASE+i),0);}
        self.rule_original=self.rule_snapshot();
    }
    fn apply_rule(&mut self)->Result<()>{
        let values=(0..RULE_FLAGS.len()).map(|i|selection(self.h(RULE_FLAG_BASE+i))).collect::<Vec<_>>();
        let instructions=text(self.h(RULE_INSTRUCTIONS));
        let key=settings::put_rule(&mut self.draft,self.editing_rule.as_deref(),if selection(self.h(RULE_KIND))==0{"app"}else{"domain"},&text(self.h(RULE_ID)),&values,is_checked(self.h(RULE_USE_INSTRUCTIONS)).then_some(instructions.as_str()))?;
        self.editing_rule=Some(key);self.rule_original=self.rule_snapshot();self.refresh_rules();Ok(())
    }
    fn show_welcome(&mut self)->Result<()>{
        if !self.dirty_close()?{return Ok(())}
        self.disarm();self.accessory.hide();self.controller.send(Command::SuspendForPreview)?;
        self.welcome=Some(WelcomeState::default());let c=self.controller.config();
        let id=if c.backend!="gguf"{format!("{}:{}",c.backend,c.model)}else if c.model_id.is_empty(){c.model_path.clone()}else{c.model_id.clone()};
        if !id.is_empty(){let ready=if c.backend=="gguf"{std::path::Path::new(&c.model_path).is_file()}else{!c.model.trim().is_empty()};let w=self.welcome.as_mut().unwrap();w.selected=id.clone();if ready{w.ready=id;}}
        self.scroll=0;self.show();self.welcome_text();Ok(())
    }
    fn welcome_text(&mut self){let Some(w)=&self.welcome else{return};let titles=[
        "1 of 6 — Welcome to BroTypist\r\nLocal typing assistance, not a writing editor. No account or cloud subscription. Predictions stay on your computer. Existing user documents are never used as this preview.",
        "2 of 6 — How it works\r\nEligible text before your cursor → selected local model → grey same-line suggestion. Protected controls, selections, IME composition and unverified geometry remain unchanged. Your Word shortcut accepts a word; your All shortcut accepts the full visible completion. Customize shortcuts in Autocomplete.",
        "3 of 6 — Review system access\r\nWindows uses native UI Automation/Edit controls and a paired keyboard hook. Website URLs are unavailable. A settings page opening is not proof of access. No permission or registration is granted by continuing. Explicitly review these limitations below.",
        "4 of 6 — Choose your local model\r\nSelect an installed catalog model, explicitly accept and download its license, or import your own GGUF. Continue is tied to that selected model, never another model's readiness. Local server availability is checked by Try it.",
        "5 of 6 — Try your selected model\r\nType in this owned ephemeral field. Predictions use only this field and your pending model, never clipboard, window, screenshot or history context. Acceptance is counted here only after exact widget readback. Use your configured Word and All shortcuts, or the explicit acceptance buttons.",
        "6 of 6 — Ready when you are\r\nClose Settings keeps BroTypist resident only when the tray is available. The tray opens Settings, pause and actual Quit. Review Privacy for independent context and encrypted learning consent. Completing Welcome saves your selected draft, not preview text."
    ];
        let text=if w.step==3{format!("{}\r\n\r\nPending selected model: {}",titles[w.step],if w.selected.is_empty(){"None"}else{&w.selected})}
            else if w.step==4{
                let shortcuts=self.draft.value("shortcuts");
                format!("{}\r\nWord: {} · All: {}",titles[w.step],shortcuts.and_then(|v|v["word"].as_str()).unwrap_or("Not assigned"),shortcuts.and_then(|v|v["all"].as_str()).unwrap_or("Not assigned"))
            }else{titles[w.step].into()};
        set_text(self.h(W_TEXT),&text);unsafe{let _=EnableWindow(self.h(W_BACK),w.step>0);let _=EnableWindow(self.h(W_NEXT),w.can_continue());}
        set_text(self.h(W_NEXT),if w.step==5{"Finish setup"}else{"Continue"});self.layout();
    }
    fn next_welcome(&mut self,back:bool)->Result<()>{
        let Some(w)=&self.welcome else{return Ok(())};
        if !back{ensure!(w.can_continue(),"Review access and choose the selected installed model first.");}
        if w.step==4{self.stop_preview()?;if self.preview.is_some(){self.preview_navigation=Some(back);return Ok(())}}
        let step=self.welcome.as_ref().unwrap().step;
        if !back&&step==5{
            self.draft.set("setup_completed",json!(true))?;
            self.refresh_controls()?;self.save()?;self.welcome=None;
            self.controller.send(Command::ResumeFromPreview)?;self.layout();return Ok(())
        }
        self.welcome.as_mut().unwrap().step=if back{step.saturating_sub(1)}else{step+1};
        self.scroll=0;
        if self.welcome.as_ref().unwrap().step==4{
            self.preview=Some(PreviewSession::new(settings::preview_config(&self.draft,&self.controller.config())?)?);
            self.preview_closing=false;self.seed_preview();
        }
        self.welcome_text();
        if self.welcome.as_ref().is_some_and(|w|w.step==4){unsafe{let _=SetFocus(Some(self.h(W_EDIT)));}}
        Ok(())
    }
    fn seed_preview(&mut self){if let Some(p)=&mut self.preview{p.dismiss();}set_text(self.h(W_EDIT),if selection(self.h(W_LANGUAGE))==1{"Amanhã vamos nos encontrar para"}else{"Tomorrow we will meet to"});unsafe{SendMessageW(self.h(W_EDIT),EM_SETSEL,Some(WPARAM(usize::MAX)),Some(LPARAM(-1)));let _=SetFocus(Some(self.h(W_EDIT)));}}
    fn stop_preview(&mut self)->Result<()>{
        if let Some(ghost)=&self.preview_ghost{ghost.hide();}
        if let Some(p)=&mut self.preview{p.set_active(false);if p.close()?{self.preview=None;self.preview_closing=false;}else{self.preview_closing=true;}}
        Ok(())
    }
    fn accept_preview(&mut self,all:bool)->Result<()>{
        if self.ime||self.preview_closing{return Ok(())}
        unsafe{let _=SetFocus(Some(self.h(W_EDIT)));}
        self.preview_update()?;
        let Some(p)=&mut self.preview else{return Ok(())};
        let proposal=p.poll()["suggestion"].as_str().unwrap_or("").to_owned();
        let visible=if let Some(ghost)=&self.preview_ghost{ghost.show(&proposal);ghost.visible()}else{String::new()};
        let insertion=p.accept_visible(all,&visible);
        if !insertion.is_empty(){
            unsafe{SendMessageW(self.h(W_EDIT),EM_REPLACESEL,Some(WPARAM(1)),Some(LPARAM(wide(&insertion).as_ptr() as isize)));}
            if let Some(ghost)=&self.preview_ghost{ghost.hide();}
            self.preview_update()?;
        }Ok(())
    }
    fn preview_update(&mut self)->Result<()>{
        let h=self.h(W_EDIT);let value=text(h);let(mut start,mut end)=(0u32,0u32);
        unsafe{SendMessageW(h,EM_GETSEL,Some(WPARAM((&mut start as *mut u32)as usize)),Some(LPARAM((&mut end as *mut u32)as isize)));}
        let caret=utf16_scalar(&value,start as usize);let end=utf16_scalar(&value,end as usize);
        let active=unsafe{GetFocus()==h}&&!self.ime&&!self.preview_closing;
        let suspended=self.status["preview_suspended"]==true;
        if let Some(p)=&mut self.preview{
            p.set_owner_released(suspended);p.set_active(active);
            if let(Some(caret),Some(end))=(caret,end){p.update(&value,caret,end.saturating_sub(caret))?;}else{p.set_active(false);}
        }Ok(())
    }
    fn command(&mut self,id:usize,notification:u16)->Result<()>{
        if (FIELD_BASE..FIELD_BASE+FIELDS.len()).contains(&id){
            if [EN_CHANGE as u16,BN_CLICKED as u16,CBN_SELCHANGE as u16].contains(&notification){
                let index=id-FIELD_BASE;
                // Native numeric edits may be temporarily empty while replacing a value.
                // Save validates the complete control; no coercion or default reset is used.
                if matches!(FIELDS[index].kind,Kind::Number)&&text(self.h(id)).trim().parse::<u32>().is_err(){return Ok(())}
                if let Err(e)=self.changed(index){self.refresh_controls()?;return Err(e)}
            }return Ok(())
        }
        if (SHORTCUT_BASE..SHORTCUT_BASE+ACTIONS.len()).contains(&id){
            self.recording=Some(id-SHORTCUT_BASE);self.controller.suspend_shortcuts(true)?;
            set_text(self.h(id),"Press a chord · Escape cancels · Backspace clears");return Ok(())
        }
        match id{
            ROOT_PANE if notification==CBN_SELCHANGE as u16=>{self.pane=selection(self.h(ROOT_PANE));self.scroll=0;self.layout();},
            SAVE=>self.save()?,REVERT=>{self.draft.reset(&self.controller.config())?;self.new_rule();self.refresh_controls()?;},
            QUIT=>self.quit()?,WELCOME=>self.show_welcome()?,
            MODEL_LIST if notification==CBN_SELCHANGE as u16=>{
                let selected=self.models.get(selection(self.h(MODEL_LIST))).and_then(|m|m["id"].as_str()).map(str::to_owned);
                if let (Some(w),Some(id))=(&mut self.welcome,selected){w.select(id);}
                self.model_details();self.welcome_text();
            },
            88=>{let reviewed=is_checked(self.h(88));if let Some(w)=&mut self.welcome{w.access_reviewed=reviewed;}self.welcome_text();},
            MODEL_SELECT=>self.use_model()?,MODEL_DOWNLOAD=>self.download()?,MODEL_CANCEL=>self.controller.send(Command::CancelDownload)?,MODEL_IMPORT=>self.import()?,MODEL_REVEAL=>crate::system::reveal_models()?,MODEL_LICENSE=>{let m=self.selected_model()?;open_url(m["license_url"].as_str().unwrap_or(""))?;},
            RULE_LIST if notification==LBN_SELCHANGE as u16=>self.load_rule()?,
            RULE_NEW=>{if self.resolve_rule_editor()?{self.new_rule();}},
            RULE_APPLY=>self.apply_rule()?,
            RULE_REMOVE=>{if let Some(key)=self.editing_rule.take(){settings::remove_rule(&mut self.draft,&key)?;self.refresh_rules();self.new_rule();}},
            COUNT=>{if self.confirm("Unlock encrypted history","Use your OS-backed encryption key to count local records? No writing text will be shown."){self.controller.send(Command::HistoryCounts{unlock:true})?;}},
            DELETE_ALL=>{if self.confirm("Forget learned writing","Permanently delete all encrypted writing and derived learning? Aggregate statistics are separate and will remain."){self.controller.send(Command::DeleteHistory{app:None,domain:None})?;}},
            DELETE_SCOPE=>{let key=self.editing_rule.clone().ok_or_else(||anyhow::anyhow!("Select a saved draft rule first."))?;if self.confirm("Forget this scope",&format!("Permanently forget writing and derived learning for {key}?")){let(kind,id)=key.split_once(':').unwrap();self.controller.send(Command::DeleteHistory{app:(kind=="app").then(||id.into()),domain:(kind=="domain").then(||id.into())})?;}},
            STATS|PERIOD|GROUP=>self.controller.send(Command::Statistics{period:combo_text(self.h(PERIOD)),group_by:combo_text(self.h(GROUP))})?,CLEAR_STATS=>{if self.confirm("Clear aggregate statistics","Delete aggregate counts only? Encrypted writing history is separate and will remain."){self.controller.send(Command::ClearStatistics)?;}},
            PERMISSIONS=>crate::system::open_permissions("accessibility")?,SOURCE=>open_url("https://github.com/renan-brasilio/brotypist")?,
            SETTINGS_FOLDER=>open_settings_folder()?,
            UPDATE_CHECK=>self.updates.start_check()?,UPDATE_DOWNLOAD=>{let now=self.updates.snapshot();ensure!(now==self.update_intent,"Update identity changed. Review its status before requesting Download again.");self.updates.start_download()?;},UPDATE_INSTALL=>{let now=self.updates.snapshot();ensure!(now==self.update_intent,"Update identity changed.");anyhow::bail!("Windows OTA installation is held. No package is installed or launched. Use a separately verified Windows release when publication is approved.");},UPDATE_CANCEL=>self.updates.cancel(),
            W_BACK=>self.next_welcome(true)?,W_NEXT=>self.next_welcome(false)?,W_LANGUAGE=>self.seed_preview(),W_WORD=>self.accept_preview(false)?,W_ALL=>self.accept_preview(true)?,_=>{}
        }Ok(())
    }
    fn runtime_event(&mut self,event:Value)->Result<()>{match event["type"].as_str().unwrap_or(""){
        "suggestion"=>{
            if self.welcome.is_some()||self.closing{
                if let Ok(p)=serde_json::from_value::<overlay::Presentation>(event){self.controller.send(p.ack(String::new()))?;}
            }else if event["mode"]=="completion"||event["mode"]=="correction"{
                if let Some(c)=self.chooser.hide(){self.controller.send(c)?;}
                self.controller.send(self.overlay.show(&event)?)?;
                if event["choices_mode"]=="alternatives" {
                    let mut choices=event.clone();choices["mode"]=json!("alternatives");
                    self.controller.send(self.chooser.show(&choices)?)?;
                }
            }else{
                if let Some(c)=self.overlay.hide(){self.controller.send(c)?;}
                self.controller.send(self.chooser.show(&event)?)?;
            }
        },
        "settings"=>{self.pane=0;self.scroll=0;self.show();},
        "quit"=>self.quit()?,
        "caret"=>{
            if self.overlay.current.as_ref().is_some_and(|p|serde_json::to_value(p.rect).ok().as_ref()!=Some(&event["rect"])) {self.disarm();}
            if self.last_config.show_accessory&&self.welcome.is_none()&&!self.closing{
                let _=self.accessory.show(&event,&self.controller.status());
            }else{self.accessory.hide();}
        },
        "config"=>{
            let config:Config=serde_json::from_value(event["config"].clone())?;
            let automatic=config.check_updates_automatically&&!self.last_config.check_updates_automatically;
            self.draft.refresh(&config)?;self.last_config=config;self.refresh_controls()?;
            if automatic{let _=self.updates.start_automatic_check();}
        },
        "clear"=>{self.disarm();if event["hide_accessory"]==true{self.accessory.hide();}},
        "models"=>{let old=self.models.get(selection(self.h(MODEL_LIST))).and_then(|m|m["id"].as_str()).map(str::to_owned);self.models=event["models"].as_array().cloned().unwrap_or_default();unsafe{SendMessageW(self.h(MODEL_LIST),CB_RESETCONTENT,None,None);}for m in &self.models{let label=format!("{} · {}",m["label"].as_str().unwrap_or(""),if m["installed"]==true{"Installed"}else{"Download"});unsafe{SendMessageW(self.h(MODEL_LIST),CB_ADDSTRING,None,Some(LPARAM(wide(&label).as_ptr()as isize)));}}set_selection(self.h(MODEL_LIST),old.and_then(|id|self.models.iter().position(|m|m["id"]==id)).unwrap_or(0));self.model_details();},
        "download"=>{
            set_text(self.h(DIAGNOSTICS),&format!("{}: {} / {} bytes\r\n{}",event["state"].as_str().unwrap_or(""),event["received"],event["total"],event["error"].as_str().unwrap_or("")));
            progress(self.h(MODEL_PROGRESS),&event);
            if event["state"]=="complete"{self.controller.send(Command::Models)?;}
        },
        "history_counts"|"statistics"|"history_info"=>set_text(self.h(RESULTS),&human_report(&event["result"])),"history_deleted"=>set_text(self.h(RESULTS),&format!("Forgot {} encrypted records and their derived learning. Statistics were not deleted.",event["count"])),"statistics_cleared"=>set_text(self.h(RESULTS),"Aggregate statistics cleared. Encrypted writing history was not deleted."),"error"=>{set_text(self.h(RESULTS),event["message"].as_str().unwrap_or("Operation failed."));},_=>{}
        }Ok(())}
    fn tick(&mut self)->Result<bool>{
        if !self.overlay.valid()||!self.chooser.valid(){self.disarm();}
        for event in self.controller.poll(){self.runtime_event(event)?;}
        self.accessory.revalidate();if self.welcome.is_some()||self.closing||!self.last_config.show_accessory{self.accessory.hide();}
        for choice in self.chooser.take_actions(){self.controller.send(Command::Action(Action::Choice(choice)))?;}
        if self.preview_closing{self.stop_preview()?;}else if self.preview.is_some(){
            self.preview_update()?;
            let v=self.preview.as_mut().unwrap().poll();
            if let Some(ghost)=&self.preview_ghost{ghost.show(v["suggestion"].as_str().unwrap_or(""));}
            set_text(self.h(W_SUGGESTION),&format!("{}\r\n{} · Accepted here: {}",v["suggestion"].as_str().unwrap_or(""),v["message"].as_str().unwrap_or(""),v["accepted"]));
        }else if let Some(ghost)=&self.preview_ghost{ghost.hide();}
        if self.preview.is_none(){
            if self.close_pending{self.close_pending=false;self.preview_navigation=None;self.welcome=None;self.controller.send(Command::ResumeFromPreview)?;unsafe{let _=ShowWindow(self.window,SW_HIDE);}}
            else if let Some(back)=self.preview_navigation.take(){self.next_welcome(back)?;}
        }
        if !self.acknowledged{crate::update_install::acknowledge_startup()?;self.acknowledged=true;}
        let status=self.controller.status();
        let power=crate::system::power_status();
        if status!=self.status||power!=self.power{
            self.status=status.clone();self.power=power;
            let title=if self.closing{"BroTypist — Stopping hooks and background processes…".into()}else{format!("BroTypist — {}{}",status["message"].as_str().unwrap_or("Local typing assistance"),if self.draft.is_dirty()||self.rule_dirty(){" · Unsaved settings"}else{""})};
            set_text(self.window,&title);
            let power=if !self.power.available{"Power unavailable — battery limits are not inferred.".into()}
                else if !self.power.has_battery{"External power — no battery reported.".into()}
                else{format!("{} · {} · {}",if self.power.on_battery{"On battery"}else{"External power"},self.power.percent.map_or("charge unknown".into(),|p|format!("{p}%")),match self.power.charging{Some(true)=>"Charging",Some(false)=>"Not charging",None=>"Charging state unknown"})};
            set_text(self.h(78),&format!("Current application: {}\r\n{}\r\nContext: {}\r\nPower: {}\r\nNative diagnostics:\r\n{}",status["current_app"].as_str().unwrap_or("Unavailable"),status["inline_message"].as_str().unwrap_or(""),status["context_message"].as_str().unwrap_or(""),power,human_report(&status["native_status"])));
        }
        let update=self.updates.snapshot();if update!=self.update_intent{self.update_intent=update.clone();set_text(self.h(UPDATE_STATUS),&format!("{}\r\n{}\r\n{} / {} bytes\r\nWindows OTA held: no install is authorized.",update["state"].as_str().unwrap_or(""),update["message"].as_str().unwrap_or(""),update["received"],update["total"]));unsafe{let _=EnableWindow(self.h(UPDATE_DOWNLOAD),["available","error","cancelled"].contains(&update["state"].as_str().unwrap_or(""))&&update["installable"]==true);let _=EnableWindow(self.h(UPDATE_INSTALL),update["state"]=="ready");}}
        progress(self.h(UPDATE_PROGRESS),&update);
        if self.closing{let preview_stopped=self.preview.is_none();let controller_stopped=self.controller.shutdown().unwrap_or(false);let update_stopped=self.updates.close().is_ok();if settings::cleanup_complete(controller_stopped,preview_stopped,update_stopped){return Ok(true)}}Ok(false)
    }
    fn key(&mut self,msg:&MSG)->Result<bool>{if msg.message==WM_IME_STARTCOMPOSITION{self.ime=true;if let Some(p)=&mut self.preview{p.set_active(false);}}if msg.message==WM_IME_ENDCOMPOSITION{self.ime=false;}if msg.message!=WM_KEYDOWN&&msg.message!=WM_SYSKEYDOWN{return Ok(false)}
        if let Some(index)=self.recording{let key=msg.wParam.0 as u16;if [VK_SHIFT.0,VK_CONTROL.0,VK_MENU.0,VK_LWIN.0,VK_RWIN.0].contains(&key){return Ok(true)}self.recording=None;self.controller.suspend_shortcuts(false)?;if key!=VK_ESCAPE.0{let chord=if key==VK_BACK.0{String::new()}else{record_chord(msg)?};let mut mapping=self.draft.value("shortcuts").and_then(Value::as_object).cloned().unwrap_or_default();mapping.insert(ACTIONS[index].0.into(),json!(chord));self.draft.set("shortcuts",Value::Object(mapping))?;}self.refresh_controls()?;return Ok(true)}
        if msg.hwnd==self.h(W_EDIT)&&self.preview.is_some()&&!self.ime{
            if msg.wParam.0==VK_ESCAPE.0 as usize{
                if let Some(p)=&mut self.preview{p.dismiss();}
                if let Some(ghost)=&self.preview_ghost{ghost.hide();}
                return Ok(true)
            }
            let chord=record_chord(msg).unwrap_or_default();
            let visible=self.preview_ghost.as_ref().is_some_and(|g|!g.visible().is_empty());
            let mapping=self.draft.value("shortcuts");
            let word=!chord.is_empty()&&mapping.and_then(|v|v["word"].as_str())==Some(chord.as_str());
            let all=!chord.is_empty()&&mapping.and_then(|v|v["all"].as_str())==Some(chord.as_str());
            if visible&&(word||all){self.accept_preview(all)?;return Ok(true)}
        }
        let native_text=unsafe{
            let mut class=[0u16;64];let n=GetClassNameW(msg.hwnd,&mut class);
            ["edit","combobox","listbox"].contains(&String::from_utf16_lossy(&class[..n.max(0)as usize]).to_ascii_lowercase().as_str())
        };
        if !native_text{
            if msg.wParam.0==VK_NEXT.0 as usize{self.scroll+=400;self.layout();return Ok(true)}
            if msg.wParam.0==VK_PRIOR.0 as usize{self.scroll-=400;self.layout();return Ok(true)}
        }
        Ok(false)
    }
}
impl Drop for Shell{fn drop(&mut self){unsafe{if self.tray{let n=NOTIFYICONDATAW{cbSize:std::mem::size_of::<NOTIFYICONDATAW>()as u32,hWnd:self.window,uID:1,..Default::default()};let _=Shell_NotifyIconW(NIM_DELETE,&n);}let _=KillTimer(Some(self.window),1);let _=DestroyWindow(self.window);let _=DestroyIcon(self.tray_icon);}}}

struct NativeEnvironment{instance:HINSTANCE,previous_dpi:DPI_AWARENESS_CONTEXT}
impl NativeEnvironment{
    fn new()->Result<Self>{unsafe{
        let previous_dpi=SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        ensure!(!previous_dpi.0.is_null(),"Per-monitor native pixel coordinates are unavailable.");
        let instance:HINSTANCE=GetModuleHandleW(None)?.into();
        let class=RegisterClassW(&WNDCLASSW{lpfnWndProc:Some(procedure),hInstance:instance,lpszClassName:w!("BroTypist.Settings"),hCursor:LoadCursorW(None,IDC_ARROW)?,hbrBackground:HBRUSH((COLOR_WINDOW.0+1)as *mut _),..Default::default()});
        ensure!(class!=0||GetLastError()==ERROR_CLASS_ALREADY_EXISTS,"The native Settings class could not register.");
        ensure!(InitCommonControlsEx(&INITCOMMONCONTROLSEX{dwSize:std::mem::size_of::<INITCOMMONCONTROLSEX>()as u32,dwICC:ICC_UPDOWN_CLASS|ICC_PROGRESS_CLASS}).as_bool(),"Native controls could not initialize.");
        Ok(Self{instance,previous_dpi})
    }}
}
impl Drop for NativeEnvironment{fn drop(&mut self){unsafe{SetThreadDpiAwarenessContext(self.previous_dpi);}}}

fn dispatch_message(shell:&mut Shell,msg:&MSG)->Result<()>{unsafe{
    if shell.taskbar_created!=0&&msg.message==shell.taskbar_created&&shell.tray_requested{
        shell.tray=shell.install_tray();if !shell.tray{shell.show();}
    }
    if !shell.key(msg)?&&!IsDialogMessageW(shell.window,msg).as_bool(){let _=TranslateMessage(msg);DispatchMessageW(msg);}
    Ok(())
}}
fn dispatch_event(shell:&mut Shell,event:Event)->Result<bool>{
    match event{
        Event::Tick=>return shell.tick(),
        Event::Command(id,n)=>shell.command(id,n)?,
        Event::Resize=>shell.layout(),
        Event::ScrollTo(position)=>{shell.scroll=position;shell.layout();},
        Event::Scroll(delta)=>{shell.scroll+=delta;shell.layout();},
        Event::Display=>{shell.disarm();shell.accessory.hide();shell.layout();},
        Event::Accessory=>shell.field_menu()?,
        Event::Tray(kind)=>{
            if kind==WM_RBUTTONUP||kind==WM_CONTEXTMENU{shell.tray_menu()?;}
            else if kind==WM_LBUTTONUP||kind==WM_LBUTTONDBLCLK{shell.show();}
        },
        Event::Close=>{
            if shell.dirty_close()?{
                if shell.tray{shell.close_pending=true;shell.preview_navigation=None;shell.stop_preview()?;}
                else if shell.confirm("No system tray","Quit BroTypist and stop typing assistance? Cancel keeps this recovery window available."){shell.quit()?;}
            }
        }
    }Ok(false)
}
pub fn launch(config:Config,options:LaunchOptions)->Result<i32>{
    let native=NativeEnvironment::new()?;
    let mut power=crate::power::PowerSampler::start()?;
    let clipboard=crate::system::ClipboardService::install()?;
    let mut shell=Shell::new(config,&options,native.instance)?;
    let mut msg=MSG::default();
    loop{
        let result=unsafe{GetMessageW(&mut msg,None,0,0).0};
        if result==0{break}
        if result<0{shell.error("The Windows message loop failed; stopping native owners.");break}
        if let Err(error)=dispatch_message(&mut shell,&msg){shell.error(error);}
        let _=clipboard.poll();
        let events=EVENTS.with(|q|q.borrow_mut().drain(..).collect::<Vec<_>>());let mut done=false;
        for event in events{match dispatch_event(&mut shell,event){Ok(stopped)=>done|=stopped,Err(error)=>shell.error(error)}}
        if done{break}
    }
    loop{
        let controller=shell.controller.shutdown().unwrap_or(false);
        let preview=shell.preview.as_mut().is_none_or(|p|p.close().unwrap_or(false));
        let updates=shell.updates.close().is_ok();
        if settings::cleanup_complete(controller,preview,updates){break}
        let _=clipboard.poll();std::thread::sleep(std::time::Duration::from_millis(15));
    }
    drop(shell);drop(clipboard);power.stop()?;Ok(0)
}
fn text(h:HWND)->String{unsafe{let length=GetWindowTextLengthW(h).clamp(0,65536)as usize;let mut units=vec![0u16;length+1];let n=GetWindowTextW(h,&mut units);String::from_utf16_lossy(&units[..n.max(0)as usize])}}
fn set_text(h:HWND,s:&str){unsafe{let _=SetWindowTextW(h,PCWSTR(wide(s).as_ptr()));}}
fn check(h:HWND,b:bool){unsafe{SendMessageW(h,BM_SETCHECK,Some(WPARAM(if b{BST_CHECKED.0 as usize}else{BST_UNCHECKED.0 as usize})),None);}}
fn is_checked(h:HWND)->bool{unsafe{SendMessageW(h,BM_GETCHECK,None,None).0==BST_CHECKED.0 as isize}}
fn selection(h:HWND)->usize{unsafe{SendMessageW(h,CB_GETCURSEL,None,None).0.max(0)as usize}}
fn set_selection(h:HWND,index:usize){unsafe{SendMessageW(h,CB_SETCURSEL,Some(WPARAM(index)),None);}}
fn combo_text(h:HWND)->String{let index=selection(h);unsafe{let len=SendMessageW(h,CB_GETLBTEXTLEN,Some(WPARAM(index)),None).0;if !(0..65536).contains(&len){return String::new()}let mut units=vec![0u16;len as usize+1];let n=SendMessageW(h,CB_GETLBTEXT,Some(WPARAM(index)),Some(LPARAM(units.as_mut_ptr()as isize))).0;String::from_utf16_lossy(&units[..n.max(0)as usize])}}
fn reset_list(h:HWND){unsafe{SendMessageW(h,LB_RESETCONTENT,None,None);}}
fn list_add(h:HWND,s:&str){unsafe{SendMessageW(h,LB_ADDSTRING,None,Some(LPARAM(wide(s).as_ptr()as isize)));}}
fn utf16_scalar(s:&str,offset:usize)->Option<usize>{let mut u=0;for(i,c)in s.chars().enumerate(){if u==offset{return Some(i)}u+=c.len_utf16();if u>offset{return None}}(u==offset).then(||s.chars().count())}
fn human_report(v:&Value)->String{fn flatten(v:&Value,prefix:&str,out:&mut String){match v{Value::Object(o)=>for(k,v)in o{flatten(v,&if prefix.is_empty(){k.clone()}else{format!("{prefix} · {k}")},out)},Value::Array(a)=>for(i,v)in a.iter().enumerate(){flatten(v,&format!("{prefix} {}",i+1),out)},_=>{out.push_str(&format!("{prefix}: {}\r\n",v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string())));}}}let mut out=String::new();flatten(v,"",&mut out);out}
fn choose_gguf(owner:HWND)->Result<Option<String>>{unsafe{
    let mut file=vec![0u16;32768];let filter=wide("GGUF models\0*.gguf\0All files\0*.*\0");
    let initial=wide(&crate::paths::model_dir()?.to_string_lossy());
    let mut dialog=OPENFILENAMEW{
        lStructSize:std::mem::size_of::<OPENFILENAMEW>()as u32,hwndOwner:owner,
        lpstrTitle:w!("Import GGUF model"),lpstrInitialDir:PCWSTR(initial.as_ptr()),
        lpstrFilter:PCWSTR(filter.as_ptr()),lpstrFile:windows::core::PWSTR(file.as_mut_ptr()),nMaxFile:file.len()as u32,
        Flags:OFN_FILEMUSTEXIST|OFN_PATHMUSTEXIST|OFN_NOCHANGEDIR|OFN_EXPLORER|OFN_DONTADDTORECENT,..Default::default()
    };
    if GetOpenFileNameW(&mut dialog).as_bool(){
        let end=file.iter().position(|c|*c==0).unwrap_or(file.len());Ok(Some(String::from_utf16(&file[..end])?))
    }else{ensure!(CommDlgExtendedError()==COMMON_DLG_ERRORS(0),"The model picker failed.");Ok(None)}
}}
fn open_url(url:&str)->Result<()>{ensure!(url.starts_with("https://")&&!url.contains(['\r','\n','\0']),"Only an explicit HTTPS destination can be opened.");unsafe{let result=ShellExecuteW(None,w!("open"),PCWSTR(wide(url).as_ptr()),None,None,SW_SHOWNORMAL);ensure!(result.0 as isize>32,"The system could not open this link.");}Ok(())}
fn record_chord(msg:&MSG)->Result<String>{let key=msg.wParam.0 as u16;let scan=((msg.lParam.0>>16)&0xff)as u32;let mut parts=Vec::new();unsafe{if GetKeyState(VK_CONTROL.0 as i32)<0{parts.push("Ctrl".to_owned())}if GetKeyState(VK_MENU.0 as i32)<0{parts.push("Alt".to_owned())}if GetKeyState(VK_SHIFT.0 as i32)<0{parts.push("Shift".to_owned())}if GetKeyState(VK_LWIN.0 as i32)<0||GetKeyState(VK_RWIN.0 as i32)<0{parts.push("Super".to_owned())}}
    let name=crate::shortcuts::windows_key(key as u32,scan);
    ensure!(!name.is_empty(),"Choose a supported letter, number, F1–F12, Space, Enter, Tab or physical Above-Tab.");
    parts.push(name);crate::shortcuts::normalize_shortcut(&parts.join("+"))
}
fn number_bounds(key:&str)->(u32,u32){
    match key{
        "debounce_ms"=>(100,5000),"min_chars"=>(1,1000),"max_tokens"=>(1,128),"context_chars"=>(64,8000),
        "idle_release_seconds"=>(0,3600),"escape_pause_seconds"=>(1,3600),
        "personalization_strength"=>(0,100),"emoji_skin_tone"=>(0,5),
        "alternatives_delay_ms"|"synonyms_delay_ms"=>(100,10000),
        _=>unreachable!("Every native numeric field has an explicit Config bound"),
    }
}

// Explicit alternatives/emoji chooser; it is never used for ordinary completion.
struct ChoiceWindow{window:HWND,current:Option<overlay::Presentation>,foreground:HWND,rows:usize,labels:Vec<HWND>}
thread_local!{static CHOICES:RefCell<Vec<usize>>=const{RefCell::new(Vec::new())};}
unsafe extern "system" fn choice_proc(hwnd:HWND,msg:u32,wp:WPARAM,lp:LPARAM)->LRESULT{
    if msg==WM_MOUSEACTIVATE{return LRESULT(MA_NOACTIVATE as isize)}
    if msg==WM_COMMAND && ((wp.0>>16)&0xffff)==STN_CLICKED as usize {
        let row=(wp.0&0xffff).saturating_sub(1);
        CHOICES.with(|q|q.borrow_mut().push(row));return LRESULT(0)
    }
    unsafe{DefWindowProcW(hwnd,msg,wp,lp)}
}
impl ChoiceWindow{
fn new(instance:HINSTANCE)->Result<Self>{unsafe{
    RegisterClassW(&WNDCLASSW{lpfnWndProc:Some(choice_proc),hInstance:instance,lpszClassName:w!("BroTypist.Choices"),hbrBackground:HBRUSH((COLOR_WINDOW.0+1)as *mut _),..Default::default()});
    let window=CreateWindowExW(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE|WS_EX_TOPMOST,w!("BroTypist.Choices"),w!("BroTypist choices"),WS_POPUP|WS_BORDER,0,0,1,1,None,None,Some(instance),None)?;
    let mut labels=Vec::new();
    for i in 0..5{
        let label=CreateWindowExW(WINDOW_EX_STYLE(0),w!("STATIC"),w!(""),WS_CHILD|WINDOW_STYLE(SS_NOTIFY as u32),8,i*32+6,424,26,Some(window),Some(HMENU((i+1)as usize as *mut _)),Some(instance),None)?;
        SendMessageW(label,WM_SETFONT,Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),None);labels.push(label);
    }
    Ok(Self{window,current:None,foreground:HWND::default(),rows:0,labels})
}}
fn hide(&mut self)->Option<Command>{unsafe{let _=ShowWindow(self.window,SW_HIDE);}self.rows=0;CHOICES.with(|q|q.borrow_mut().clear());self.current.take().map(|p|p.ack(String::new()))}
fn show(&mut self,event:&Value)->Result<Command>{
    let _=self.hide();let p:overlay::Presentation=serde_json::from_value(event.clone())?;
    let ack=match self.draw(&p){Ok(text)=>text,Err(_)=>{unsafe{let _=ShowWindow(self.window,SW_HIDE);}String::new()}};
    self.current=Some(p.clone());Ok(p.ack(ack))
}
fn draw(&mut self,p:&overlay::Presentation)->Result<String>{unsafe{
    ensure!(p.mode!="completion"&&p.mode!="correction","No completion popup.");
    let rect=p.rect.ok_or_else(||anyhow::anyhow!("No chooser anchor"))?;
    ensure!(rect.iter().all(|v|v.is_finite())&&!p.choices.is_empty(),"No choices");
    self.foreground=GetForegroundWindow();let x=rect[0].round()as i32;let y=(rect[1]+rect[3]+4.0).round()as i32;
    let mut work=RECT::default();SystemParametersInfoW(SPI_GETWORKAREA,0,Some((&mut work as *mut RECT).cast()),SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))?;
    let width=440;ensure!(x>=work.left&&x+width<=work.right&&y>=work.top,"Chooser cannot be placed completely on screen.");
    let count=p.choices.len().min(5).min(((work.bottom-y)/32).max(0)as usize);
    let dc=GetDC(Some(self.window));let old=SelectObject(dc,GetStockObject(DEFAULT_GUI_FONT));let mut visible=Vec::new();
    for(i,row)in p.choices.iter().take(count).enumerate(){
        if row.chars().any(char::is_control){break}
        let title=format!("{}. {}",i+1,row);let units:Vec<_>=title.encode_utf16().collect();let mut size=SIZE::default();
        if !GetTextExtentPoint32W(dc,&units,&mut size).as_bool()||size.cx>width-16||size.cy>26{break}
        set_text(self.labels[i],&title);visible.push(row.clone());
    }
    let _=SelectObject(dc,old);ReleaseDC(Some(self.window),dc);self.rows=visible.len();
    ensure!(self.rows>0&&GetForegroundWindow()==self.foreground,"No complete visible choice row");
    for(i,label)in self.labels.iter().enumerate(){let _=ShowWindow(*label,if i<self.rows{SW_SHOW}else{SW_HIDE});}
    SetWindowPos(self.window,Some(HWND_TOPMOST),x,y,width,self.rows as i32*32+2,SWP_NOACTIVATE|SWP_SHOWWINDOW)?;
    let _=UpdateWindow(self.window);
    for label in self.labels.iter().take(self.rows){let _=UpdateWindow(*label);}
    ensure!(IsWindowVisible(self.window).as_bool()&&GetForegroundWindow()==self.foreground,"Chooser refused");
    Ok(visible.join("\n"))
}}
fn valid(&self)->bool{unsafe{self.current.is_none()||(GetForegroundWindow()==self.foreground&&IsWindowVisible(self.window).as_bool())}}
fn take_actions(&self)->Vec<usize>{CHOICES.with(|q|q.borrow_mut().drain(..).filter(|i|*i<self.rows).collect())}}
impl Drop for ChoiceWindow{fn drop(&mut self){unsafe{let _=DestroyWindow(self.window);}}}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn readback_offsets_never_split_surrogate_pairs(){
        assert_eq!(utf16_scalar("a😀b",1),Some(1));assert_eq!(utf16_scalar("a😀b",2),None);
        assert_eq!(utf16_scalar("a😀b",3),Some(2));assert_eq!(utf16_scalar("a😀b",4),Some(3));assert_eq!(utf16_scalar("a😀b",5),None);
    }
    #[test]fn reports_are_readable_and_not_editable_json(){
        let v=json!({"total":{"words":12},"rows":[{"app":"owned.exe","characters":52}]});let s=human_report(&v);
        assert!(s.contains("total · words: 12"));assert!(s.contains("owned.exe"));assert!(!s.starts_with('{'));
    }
    #[test]fn tomorrow_pause_uses_upward_minute_rounding(){
        assert_eq!(pause_minutes(100,101).unwrap(),1);assert_eq!(pause_minutes(100,160).unwrap(),1);
        assert_eq!(pause_minutes(100,161).unwrap(),2);assert!(pause_minutes(100,100).is_err());
    }
}

struct NativeMenu(HMENU);
impl Drop for NativeMenu{fn drop(&mut self){unsafe{let _=DestroyMenu(self.0);}}}
fn pause_minutes(now:i64,midnight:i64)->Result<u32>{
    ensure!(midnight>now,"The next local midnight is unavailable.");
    Ok(u32::try_from((midnight-now+59)/60)?)
}
fn tomorrow_minutes()->Result<u32>{
    use chrono::TimeZone;
    let now=chrono::Local::now();
    let midnight=now.date_naive().succ_opt().and_then(|d|d.and_hms_opt(0,0,0)).and_then(|d|chrono::Local.from_local_datetime(&d).earliest()).ok_or_else(||anyhow::anyhow!("The next local midnight is unavailable."))?;
    pause_minutes(now.timestamp(),midnight.timestamp())
}

fn open_settings_folder()->Result<()>{
    let path=crate::paths::config_dir()?;
    unsafe{let result=ShellExecuteW(None,w!("open"),PCWSTR(wide(&path.to_string_lossy()).as_ptr()),None,None,SW_SHOWNORMAL);ensure!(result.0 as isize>32,"The settings folder could not be opened.");}
    Ok(())
}
fn progress(control:HWND,snapshot:&Value){
    let received=snapshot["received"].as_u64().unwrap_or(0);let total=snapshot["total"].as_u64().unwrap_or(0);
    let percent=if total==0{0}else{((received as u128*100)/total as u128).min(100)as usize};
    unsafe{SendMessageW(control,PBM_SETRANGE32,Some(WPARAM(0)),Some(LPARAM(100)));SendMessageW(control,PBM_SETPOS,Some(WPARAM(percent)),None);}
}
