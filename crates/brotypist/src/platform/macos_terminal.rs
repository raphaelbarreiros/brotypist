//! Source-bound signed Codex 0.162.1 composer. Never reads AXValue/scrollback.
use anyhow::{Result,anyhow};
use crate::{config::Config,core::{Snapshot,Rect},platform::EditOutcome};
use super::{CaptureServices,MacSession,TerminalIntegration,ax::{AxElement,AxRange,AxValue,Raw},input::InputSourceKind};
use super::ax::ffi::{CGPreflightPostEventAccess,CGEventSourceFlagsState};
use objc2::{msg_send,rc::Retained,runtime::{AnyClass,AnyObject}};
use objc2_foundation::NSString;
use sha2::{Sha256,Digest};
use std::{ffi::{c_void,CStr},sync::LazyLock,time::{Duration,Instant},os::unix::fs::{FileTypeExt,MetadataExt}};
use unicode_general_category::{get_general_category,GeneralCategory as G};
pub const TERMINAL:&str="com.apple.Terminal";
pub const ITERM:&str="com.googlecode.iterm2";
pub const CODEX_REQUIREMENT:&str="anchor apple generic and identifier \"codex\" and certificate leaf[subject.OU] = \"2DC432GLL2\" and cdhash H\"9ddc61acceef79aa2aad437ba1fe94239f2e0423\"";
const READ_OPTIONS:usize=0x00021093;
fn code(s:&str)->u32{u32::from_be_bytes(s.as_bytes().try_into().expect("fixed AppleEvent code"))}
fn fail<T>(reason:&'static str)->Result<T>{Err(anyhow!(reason))}
#[derive(Clone,Debug,PartialEq,Eq)] pub struct ProcessIdentity{pub pid:i32,pub ppid:u32,pub pgid:u32,pub started:(u64,u64),pub executable:String}
#[derive(Clone,Debug,PartialEq)] pub struct TerminalMetadata{pub app_id:String,pub host_pid:i32,pub window_id:u32,pub tab_id:String,pub session_id:String,pub tty:String,pub columns:usize,pub rows:usize,pub app_version:String,pub font_name:String,pub font_size:f64,pub at_shell_prompt:Option<bool>,pub tmux_role:String,pub pane_id:String,pub process:Option<ProcessIdentity>}
impl TerminalMetadata{pub fn candidate(&self)->&'static str{let name=self.process.as_ref().and_then(|p|p.executable.rsplit('/').next()).unwrap_or("");if !self.tmux_role.is_empty()||["tmux","zellij"].contains(&name){"multiplexer"}else if ["claude","codex","opencode","pi","kilo"].contains(&name){"agent"}else if ["zsh","bash","fish","sh","dash","ksh","tcsh"].contains(&name){"shell"}else{"unknown"}}}
#[repr(C,packed(2))] struct AeDesc{kind:u32,handle:*mut c_void}
#[link(name="CoreServices",kind="framework")] unsafe extern "C"{
 fn AECreateDesc(kind:u32,data:*const c_void,size:isize,out:*mut AeDesc)->i16;
 fn AEDisposeDesc(desc:*mut AeDesc)->i16;
 fn AEDeterminePermissionToAutomateTarget(desc:*const AeDesc,class:u32,id:u32,ask:bool)->i32;
}
fn running(app_id:&str)->Vec<i32>{unsafe{
 let Some(class)=AnyClass::get(c"NSRunningApplication")else{return Vec::new()};let bundle=NSString::from_str(app_id);
 let apps:*mut AnyObject=msg_send![class,runningApplicationsWithBundleIdentifier:&*bundle];if apps.is_null(){return Vec::new()}
 let count:usize=msg_send![apps,count];if count>64{return Vec::new()}
 (0..count).filter_map(|i|{let app:*mut AnyObject=msg_send![apps,objectAtIndex:i];let terminated:bool=msg_send![app,isTerminated];let pid:i32=msg_send![app,processIdentifier];(!terminated&&pid>0).then_some(pid)}).collect()
}}
unsafe extern "C"{fn pthread_main_np()->i32;}
pub fn permission_state(app_id:&str,request:bool)->&'static str{
 if ![TERMINAL,ITERM].contains(&app_id){return "unsupported"}if unsafe{pthread_main_np()}==0{return "unavailable"}let pids=running(app_id);if pids.is_empty(){return "not-running"}if pids.len()!=1{return "unavailable"}
 permission_for_pid(pids[0],request)
}
fn permission_for_pid(pid:i32,request:bool)->&'static str{
 if pid<=0{return "not-running"}
 let mut target=AeDesc{kind:0,handle:std::ptr::null_mut()};
 unsafe{if AECreateDesc(code("kpid"),(&pid as *const i32).cast(),4,&mut target)!=0{return "unavailable"}let result=AEDeterminePermissionToAutomateTarget(&target,code("core"),code("getd"),request);AEDisposeDesc(&mut target);match result{0=>"granted",-1744=>"not-determined",-1743=>"denied",-600=>"not-running",_=>"unavailable"}}
}
struct Event(Retained<AnyObject>);
impl Event{
 fn owned(raw:*mut AnyObject)->Result<Self>{unsafe{Retained::retain(raw)}.map(Self).ok_or_else(||anyhow!("metadata-unavailable"))}
 fn class()->Result<&'static AnyClass>{AnyClass::get(c"NSAppleEventDescriptor").ok_or_else(||anyhow!("metadata-unavailable"))}
 fn null()->Result<Self>{Self::owned(unsafe{msg_send![Self::class()?,nullDescriptor]})}
 fn int(n:i32)->Result<Self>{Self::owned(unsafe{msg_send![Self::class()?,descriptorWithInt32:n]})}
 fn text(s:&str)->Result<Self>{let s=NSString::from_str(s);Self::owned(unsafe{msg_send![Self::class()?,descriptorWithString:&*s]})}
 fn spec(container:&Self,want:&str,form:&str,selector:&Self)->Result<Self>{unsafe{
 let record=Self::owned(msg_send![Self::class()?,recordDescriptor])?;
 let wanted=Self::owned(msg_send![Self::class()?,descriptorWithTypeCode:code(want)])?;
 let form=Self::owned(msg_send![Self::class()?,descriptorWithEnumCode:code(form)])?;
 for (key,value) in [("want",&wanted),("from",container),("form",&form),("seld",selector)]{let _:()=msg_send![&*record.0,setDescriptor:&*value.0,forKeyword:code(key)];}
 Self::owned(msg_send![&*record.0,coerceToDescriptorType:code("obj ")])
 }}
 fn property(container:&Self,name:&str)->Result<Self>{let selector=Self::owned(unsafe{msg_send![Self::class()?,descriptorWithTypeCode:code(name)]})?;Self::spec(container,"prop","prop",&selector)}
 fn string(&self)->Result<String>{let s:*mut NSString=unsafe{msg_send![&*self.0,stringValue]};if s.is_null(){fail("metadata-unavailable")}else{Ok(unsafe{&*s}.to_string())}}
 fn i32(&self)->i32{unsafe{msg_send![&*self.0,int32Value]}}
 fn bool(&self)->bool{unsafe{msg_send![&*self.0,booleanValue]}}
 fn double(&self)->f64{unsafe{msg_send![&*self.0,doubleValue]}}
}
struct Automation{target:Event,deadline:Instant}
impl Automation{
 fn new(pid:i32)->Result<Self>{let target=Event::owned(unsafe{msg_send![Event::class()?,descriptorWithDescriptorType:code("kpid"),bytes:(&pid as *const i32).cast::<c_void>(),length:4usize]})?;Ok(Self{target,deadline:Instant::now()+Duration::from_millis(600)})}
 fn send(&self,class:&str,id:&str,direct:&Event,parameters:&[(&str,&Event)])->Result<Event>{
 let remaining=self.deadline.saturating_duration_since(Instant::now()).as_secs_f64();if remaining<=0.0{return fail("metadata-timeout")}
 unsafe{let event=Event::owned(msg_send![Event::class()?,appleEventWithEventClass:code(class),eventID:code(id),targetDescriptor:&*self.target.0,returnID:-1i16,transactionID:0i32])?;
 let _:()=msg_send![&*event.0,setParamDescriptor:&*direct.0,forKeyword:code("----")];
 for (name,value) in parameters{let _:()=msg_send![&*event.0,setParamDescriptor:&*value.0,forKeyword:code(name)];}
 let mut error:*mut AnyObject=std::ptr::null_mut();let reply:*mut AnyObject=msg_send![&*event.0,sendEventWithOptions:READ_OPTIONS,timeout:remaining.min(0.15),error:&mut error];
 if !error.is_null()||reply.is_null(){return fail("metadata-unavailable")}
 let reply=Event::owned(reply)?;let failure:*mut AnyObject=msg_send![&*reply.0,paramDescriptorForKeyword:code("errn")];
 if !failure.is_null()&&Event::owned(failure)?.i32()!=0{return fail("metadata-unavailable")}
 Event::owned(msg_send![&*reply.0,paramDescriptorForKeyword:code("----")])}
 }
 fn get(&self,container:&Event,key:&str)->Result<Event>{self.send("core","getd",&Event::property(container,key)?,&[])}
}
fn metadata(pid:i32,app_id:&str,host_alive:&dyn Fn()->bool)->Result<TerminalMetadata>{
 if !host_alive(){return fail("not-running")}
 let ae=Automation::new(pid)?;let root=Event::null()?;let version=ae.get(&root,"vers")?.string()?;
 let (window,session,tab_id,session_id,tty,columns,rows,font_name,font_size,at_shell_prompt,tmux_role,pane_id,job_pid)=if app_id==TERMINAL{
 let window=Event::spec(&root,"cwin","indx",&Event::int(1)?)?;let session=Event::property(&window,"tcnt")?;let settings=Event::property(&session,"tcst")?;
 let tty=ae.get(&session,"ttty")?.string()?;if !ae.get(&session,"tbsl")?.bool(){return fail("inactive-session")}
 let cols=ae.get(&session,"ccol")?.i32();let rows=ae.get(&session,"crow")?.i32();let font=ae.get(&settings,"font")?.string()?;let size=ae.get(&settings,"ptsz")?.double();
 (window,session,tty.clone(),tty.clone(),tty,cols,rows,font,size,None,String::new(),String::new(),String::new())
 }else{
 let window=Event::property(&root,"Crwn")?;let tab=Event::property(&window,"Crtb")?;let session=Event::property(&tab,"Wcsn")?;
 let variable=|name:&str|ae.send("Itrm","varb",&session,&[("Namd",&Event::text(name)?)])?.string();
 let tty=ae.get(&session,"Stty")?.string()?;let tab_id=ae.get(&tab,"Indx")?.i32().to_string();let session_id=ae.get(&session,"ID  ")?.string()?;
 let cols=ae.get(&session,"Cols")?.i32();let rows=ae.get(&session,"Rows")?.i32();let prompt=ae.get(&session,"Iaps")?.bool();let role=variable("tmuxRole")?;let pane=variable("tmuxWindowPane")?;let job=variable("jobPid")?;
 (window,session,tab_id,session_id,tty,cols,rows,String::new(),0.0,Some(prompt),role,pane,job)
 };
 let window_id=ae.get(&window,"ID  ")?.i32();
 if !valid_tty(&tty)||session_id.is_empty()||session_id.len()>256||window_id<=0||!(1..=10000).contains(&columns)||!(1..=10000).contains(&rows){return fail("invalid-session-metadata")}
 if !font_size.is_finite()||!(0.0..=300.0).contains(&font_size)||font_name.len()>256{return fail("invalid-font-metadata")}
 if !["","client","gateway"].contains(&tmux_role.as_str())||pane_id.len()>32{return fail("invalid-pane-metadata")}
 let job=if job_pid.is_empty(){0}else{if !job_pid.bytes().all(|c|c.is_ascii_digit()){return fail("invalid-process-metadata")}job_pid.parse::<i32>().map_err(|_|anyhow!("invalid-process-metadata"))?};
 let process=foreground_process(&tty,job);
 if ae.get(&window,"ID  ")?.i32()!=window_id||ae.get(&session,if app_id==TERMINAL{"ttty"}else{"Stty"})?.string()?!=tty||!host_alive(){return fail("session-changed")}
 if app_id==ITERM&&ae.get(&session,"ID  ")?.string()?!=session_id{return fail("session-changed")}
 Ok(TerminalMetadata{app_id:app_id.into(),host_pid:pid,window_id:window_id as u32,tab_id,session_id,tty,columns:columns as usize,rows:rows as usize,app_version:version,font_name,font_size,at_shell_prompt,tmux_role,pane_id,process})
}
#[repr(C)] #[derive(Default,PartialEq,Eq)] struct BsdInfo{flags:u32,status:u32,xstatus:u32,pid:u32,ppid:u32,uid:u32,gid:u32,ruid:u32,rgid:u32,svuid:u32,svgid:u32,reserved:u32,comm:[u8;16],name:[u8;32],nfiles:u32,pgid:u32,jobc:u32,ttydev:u32,tpgid:u32,nice:i32,start_sec:u64,start_usec:u64}
#[link(name="proc")] unsafe extern "C"{fn proc_listpids(kind:u32,info:u32,buffer:*mut c_void,size:i32)->i32;fn proc_pidinfo(pid:i32,flavor:i32,arg:u64,buffer:*mut c_void,size:i32)->i32;fn proc_pidpath(pid:i32,buffer:*mut c_void,size:u32)->i32;}
fn valid_tty(tty:&str)->bool{tty.strip_prefix("/dev/tty").is_some_and(|s|!s.is_empty()&&s.bytes().all(|c|c.is_ascii_alphanumeric()))}
fn foreground_process(tty:&str,job:i32)->Option<ProcessIdentity>{
 if !valid_tty(tty){return None}let device=std::fs::metadata(tty).ok()?;if !device.file_type().is_char_device(){return None}let dev=device.rdev() as u32;
 let mut pids=[0i32;256];let size=unsafe{proc_listpids(3,dev,pids.as_mut_ptr().cast(),std::mem::size_of_val(&pids) as i32)};if size<=0||size as usize>=std::mem::size_of_val(&pids)||size%4!=0{return None}
 let read=|pid|{let mut info=BsdInfo::default();let size=std::mem::size_of::<BsdInfo>() as i32;(unsafe{proc_pidinfo(pid,3,0,(&mut info as *mut BsdInfo).cast(),size)}==size).then_some(info)};
 let mut candidates=Vec::new();
 for &pid in &pids[..size as usize/4]{if pid<=0||(job!=0&&pid!=job){continue}let Some(before)=read(pid)else{continue};if before.pid!=pid as u32||before.uid!=unsafe{libc::getuid()}||before.start_sec==0||before.ttydev!=dev||before.tpgid==0||before.pgid!=before.tpgid{continue}
 let mut path=[0u8;4096];if unsafe{proc_pidpath(pid,path.as_mut_ptr().cast(),4096)}<=0{continue}let Some(after)=read(pid)else{continue};if (before.pid,before.ppid,before.pgid,before.uid,before.ttydev,before.tpgid,before.start_sec,before.start_usec,before.name,before.comm)!=(after.pid,after.ppid,after.pgid,after.uid,after.ttydev,after.tpgid,after.start_sec,after.start_usec,after.name,after.comm){continue}
 let executable=CStr::from_bytes_until_nul(&path).ok()?.to_str().ok()?.to_owned();if !executable.starts_with('/'){continue}candidates.push(ProcessIdentity{pid,ppid:before.ppid,pgid:before.pgid,started:(before.start_sec,before.start_usec),executable});}
 (candidates.len()==1).then(||candidates.remove(0))
}
trait TerminalAccess{
 fn session(&mut self)->Result<Option<MacSession>>;
 fn allowed(&self,session:&MacSession,editor_probe:bool)->bool;
 fn secure_input(&self)->Option<bool>;
 fn dead_key(&self)->bool;
 fn source_kind(&self)->InputSourceKind;
 fn permission(&self,session:&MacSession)->&'static str;
 fn host_alive(&self,session:&MacSession)->bool;
}
impl TerminalAccess for CaptureServices<'_>{
 fn session(&mut self)->Result<Option<MacSession>>{CaptureServices::session(self)}
 fn allowed(&self,session:&MacSession,editor_probe:bool)->bool{CaptureServices::allowed(self,session,editor_probe)}
 fn secure_input(&self)->Option<bool>{CaptureServices::secure_input(self)}
 fn dead_key(&self)->bool{CaptureServices::dead_key(self)}
 fn source_kind(&self)->InputSourceKind{CaptureServices::source_kind(self)}
 fn permission(&self,session:&MacSession)->&'static str{permission_state(&session.foreground.app_id,false)}
 fn host_alive(&self,session:&MacSession)->bool{running(&session.foreground.app_id).contains(&session.foreground.window_pid)}
}
#[derive(Clone,Debug,PartialEq,Eq)]struct HostIdentity{pid:i32,started:(u64,u64),executable:String}
fn host_identity(pid:i32)->Option<HostIdentity>{
 let read=||{let mut info=BsdInfo::default();let size=std::mem::size_of::<BsdInfo>()as i32;(unsafe{proc_pidinfo(pid,3,0,(&mut info as *mut BsdInfo).cast(),size)}==size).then_some(info)};
 let before=read()?;if before.pid!=pid as u32||before.start_sec==0||before.uid!=unsafe{libc::getuid()}{return None}
 let mut path=[0u8;4096];if unsafe{proc_pidpath(pid,path.as_mut_ptr().cast(),4096)}<=0{return None}
 let after=read()?;if before.pid!=after.pid||before.start_sec!=after.start_sec||before.start_usec!=after.start_usec||before.uid!=after.uid||before.name!=after.name||before.comm!=after.comm{return None}
 Some(HostIdentity{pid,started:(before.start_sec,before.start_usec),executable:CStr::from_bytes_until_nul(&path).ok()?.to_str().ok()?.into()})
}
struct DetachedAccess{lease:super::NativeReadLease,host:HostIdentity}
impl TerminalAccess for DetachedAccess{
 fn session(&mut self)->Result<Option<MacSession>>{
  if !self.lease.valid()||host_identity(self.host.pid).as_ref()!=Some(&self.host){return Ok(None)}
  let Some(system)=AxElement::system_wide()else{return Ok(None)};system.set_timeout(0.25)?;
  if system.element("AXFocusedApplication").and_then(|a|a.pid())!=Some(self.host.pid){return Ok(None)}
  let Some(app)=AxElement::application(self.host.pid)else{return Ok(None)};app.set_timeout(0.25)?;let window=app.element("AXFocusedWindow");
  Ok(Some(MacSession{foreground:self.lease.foreground.clone(),name:self.lease.name.clone(),app,window,document_id:String::new()}))
 }
 fn allowed(&self,_:&MacSession,_:bool)->bool{self.lease.valid()}
 fn secure_input(&self)->Option<bool>{self.lease.valid().then_some(false)}
 fn dead_key(&self)->bool{!self.lease.valid()}
 fn source_kind(&self)->InputSourceKind{self.lease.source_kind()}
 fn permission(&self,_:&MacSession)->&'static str{if self.lease.valid()&&host_identity(self.host.pid).as_ref()==Some(&self.host){permission_for_pid(self.host.pid,false)}else{"unavailable"}}
 fn host_alive(&self,_:&MacSession)->bool{self.lease.valid()&&host_identity(self.host.pid).as_ref()==Some(&self.host)}
}
pub struct TerminalContextTicket{lease:super::NativeReadLease,host:HostIdentity}
impl TerminalContextTicket{
 fn observe(&self)->Result<bool>{
  if !self.lease.valid(){return Ok(false)}let mut access=DetachedAccess{lease:self.lease.clone(),host:self.host.clone()};let Some(session)=access.session()?else{return Ok(false)};
  let mut adapter=TerminalAdapter::new();adapter.enabled=true;
  let result=adapter.read(&mut access,&session);
  Ok(result.is_ok_and(|(snapshot,_,_,_,_)|self.lease.valid()&&self.lease.matches_snapshot(&snapshot)))
 }
}
impl crate::platform::DetachedContextRead for TerminalContextTicket{
 fn read(&self,_:&dyn Fn()->bool,_:usize)->Result<crate::platform::ContextRead>{
  // Terminal opt-in covers only unsent input. Neither AX context nor OCR may
  // acquire transcript pixels/text, even with general screenshot consent.
  Ok(crate::platform::ContextRead::blocked("accessibility"))
 }
 fn matches(&self)->Result<bool>{self.observe()}
 fn matches_snapshot(&self,snapshot:&Snapshot)->bool{self.lease.matches_snapshot(snapshot)}
}
#[link(name="Security",kind="framework")] unsafe extern "C"{static kSecGuestAttributePid:Raw;fn SecRequirementCreateWithString(text:Raw,flags:u32,result:*mut Raw)->i32;fn SecCodeCopyGuestWithAttributes(host:Raw,attrs:Raw,flags:u32,result:*mut Raw)->i32;fn SecCodeCheckValidity(code:Raw,flags:u32,requirement:Raw)->i32;}
#[link(name="CoreFoundation",kind="framework")] unsafe extern "C"{fn CFNumberCreate(allocator:Raw,kind:isize,value:*const c_void)->Raw;fn CFDictionaryCreate(allocator:Raw,keys:*const Raw,values:*const Raw,count:isize,key_callbacks:Raw,value_callbacks:Raw)->Raw;}
struct CodeIdentity{requirement:Option<AxValue>}
impl CodeIdentity{
 fn accepts(&mut self,process:&ProcessIdentity)->bool{if process.pid<=0||process.pid as u32!=process.pgid{return false}unsafe{
 if self.requirement.is_none(){let Some(text)=AxValue::string(CODEX_REQUIREMENT)else{return false};let mut value=std::ptr::null();let status=SecRequirementCreateWithString(text.as_raw(),0,&mut value);let value=AxValue::from_owned(value);if status!=0{return false}self.requirement=value;}
 let Some(requirement)=&self.requirement else{return false};let Some(number)=AxValue::from_owned(CFNumberCreate(std::ptr::null(),3,(&process.pid as *const i32).cast()))else{return false};
 let Some(attrs)=AxValue::from_owned(CFDictionaryCreate(std::ptr::null(),&kSecGuestAttributePid,&number.as_raw(),1,std::ptr::null(),std::ptr::null()))else{return false};
 let mut code=std::ptr::null();let status=SecCodeCopyGuestWithAttributes(std::ptr::null(),attrs.as_raw(),0,&mut code);let code=AxValue::from_owned(code);status==0&&code.is_some_and(|code|SecCodeCheckValidity(code.as_raw(),0,requirement.as_raw())==0)
 }}
}
fn printable(text:&str)->bool{text.chars().all(|c|!matches!(get_general_category(c),G::Control|G::Format|G::Surrogate|G::PrivateUse|G::Unassigned|G::LineSeparator|G::ParagraphSeparator))}
fn plain_draft(text:&str)->bool{!text.is_empty()&&printable(text)&&!text.chars().any(|c|matches!(unicode_bidi::bidi_class(c),unicode_bidi::BidiClass::R|unicode_bidi::BidiClass::AL|unicode_bidi::BidiClass::AN))&&!text.starts_with(['/','!','›','»'])&&!text.split_whitespace().any(|s|s.starts_with(['@','$']))&&!hidden(text)}
static HIDDEN:LazyLock<regex::Regex>=LazyLock::new(||regex::Regex::new(r"\[(?:Pasted Content [0-9]+ chars|Image #[0-9]+)\]").unwrap());
static FOOTER:LazyLock<regex::Regex>=LazyLock::new(||regex::Regex::new(r"^  [^\s·]+(?: [^\s·]+)* (?:default|minimal|low|medium|high|xhigh|max|ultra|persistent) · [^\r\n]+$").unwrap());
fn hidden(text:&str)->bool{HIDDEN.is_match(text)}
fn cells(text:&str)->usize{text.chars().map(|c|if matches!(get_general_category(c),G::NonspacingMark|G::SpacingMark|G::EnclosingMark){0}else if crate::providers::east_asian_wide(c){2}else{1}).sum()}
fn lease_matches(lease:Option<&Snapshot>,snapshot:&Snapshot)->bool{lease.is_some_and(|v|v.same_key(snapshot)&&v.same_authority(snapshot))}
fn consume_lease(lease:&mut Option<Snapshot>,snapshot:&Snapshot)->bool{if !lease_matches(lease.as_ref(),snapshot){return false}lease.take();true}
fn valid_rect(rect:Rect)->bool{rect.iter().all(|v|v.is_finite())&&rect[2]>0.0&&rect[3]>0.0}
fn contains(a:Rect,b:Rect)->bool{b[0]>=a[0]&&b[1]>=a[1]&&b[0]+b[2]<=a[0]+a[2]+0.5&&b[1]+b[3]<=a[1]+a[3]+0.5}
#[derive(Debug,PartialEq)] struct Row{start:usize,length:usize,text:String}
fn row(field:&AxElement,line:usize,columns:usize)->Result<Row>{let range=field.range_for_line(line)?.ok_or_else(||anyhow!("native-input-range-unavailable"))?;if range.end().is_none()||range.length==0||range.length>4096.min(columns*2+1){return fail("native-row-unbounded")}
 let text=field.string_for_range(range)?.ok_or_else(||anyhow!("native-input-range-unavailable"))?;
 if text.encode_utf16().count()!=range.length{return fail("native-row-inconsistent")}
 if !text.ends_with('\n')||text[..text.len()-1].contains(['\n','\r']){return fail("wrapped-or-incomplete-input-row")}
 Ok(Row{start:range.location,length:range.length,text:text[..text.len()-1].into()})}
fn hash(value:impl std::fmt::Debug)->String{format!("{:x}",Sha256::digest(format!("{value:?}").as_bytes()))}
pub struct TerminalAdapter{enabled:bool,stopped:bool,signature:CodeIdentity,lease:Option<Snapshot>,appearance:super::vision::VisionBridge,pub diagnostic:&'static str}
impl Default for TerminalAdapter{fn default()->Self{Self{enabled:false,stopped:false,signature:CodeIdentity{requirement:None},lease:None,appearance:super::vision::VisionBridge::new(),diagnostic:"terminal-unverified"}}}
impl TerminalAdapter{
 pub fn new()->Self{Self::default()}
 fn inspect_native(&mut self,services:&mut impl TerminalAccess,session:&MacSession)->Result<TerminalMetadata>{
  if ![TERMINAL,ITERM].contains(&session.foreground.app_id.as_str()){return fail("unsupported-terminal")}
  if services.secure_input()!=Some(false)||services.dead_key()||!services.allowed(session,true){return fail("privacy-or-input-policy")}
  let current=services.session()?.ok_or_else(||anyhow!("foreground-changed"))?;
  if !current.same_owner(session)||current.window!=session.window{return fail("foreground-changed")}
  let permission=services.permission(session);if permission!="granted"{return fail(match permission{"denied"=>"automation-denied","not-determined"=>"automation-not-determined","not-running"=>"automation-not-running",_=>"automation-unavailable"})}
  let meta=metadata(session.foreground.window_pid,&session.foreground.app_id,&||services.host_alive(session))?;
  let current=services.session()?.ok_or_else(||anyhow!("foreground-changed"))?;
  if !current.same_owner(session)||current.window!=session.window||services.secure_input()!=Some(false)||services.dead_key()||!services.allowed(&current,true){return fail("foreground-changed")}
  Ok(meta)
 }
 pub fn inspect(&mut self,services:&mut CaptureServices<'_>,session:&MacSession)->Result<Option<TerminalMetadata>>{
  match self.inspect_native(services,session){Ok(metadata)=>{self.diagnostic="metadata-only";Ok(Some(metadata))},Err(error)=>{let message=error.to_string();self.diagnostic=DIAGNOSTICS.iter().copied().find(|v|*v==message).unwrap_or("native-input-unavailable");Ok(None)}}
 }
 fn field(&self,services:&mut impl TerminalAccess,session:&MacSession,meta:&TerminalMetadata)->Result<AxElement>{
 let field=session.app.element("AXFocusedUIElement").ok_or_else(||anyhow!("native-input-focus-unverified"))?;
 if field.string("AXRole").as_deref()!=Some("AXTextArea")||field.string("AXSubrole").as_deref()==Some("AXSecureTextField")||field.bool("AXProtectedContent")==Some(true)||field.bool("AXHidden")==Some(true)||field.bool("AXFocused")==Some(false)||field.bool("AXEnabled")==Some(false){return fail("native-input-focus-unverified")}
 match field.attribute("AXMarkedTextRange").map_err(|_|anyhow!("composition-unobservable"))?{Some(value)=>{let range=value.as_range().ok_or_else(||anyhow!("composition-unobservable"))?;if range.length>0{return fail("composition-active")}},None if services.source_kind()!=InputSourceKind::Layout=>return fail("composition-unobservable"),None=>{}}
 if field.range("AXSelectedTextRange").is_none_or(|r|r.length!=0){return fail("terminal-selection-active")}
 let window=field.element("AXWindow").ok_or_else(||anyhow!("native-window-focus-unverified"))?;
 if session.app.element("AXFocusedWindow").as_ref()!=Some(&window)||window.frame().and_then(|r|super::vision::window_number(meta.host_pid,r))!=Some(meta.window_id){return fail("native-window-identity-unverified")}
 Ok(field)
 }
 fn read(&mut self,services:&mut impl TerminalAccess,session:&MacSession)->Result<(Snapshot,AxElement,TerminalMetadata,usize,Rect)>{
 if !self.enabled||self.stopped||!super::ax::trusted(){return fail("terminal-opt-in-or-access-required")}
 let meta=self.inspect_native(services,session)?;
 if meta.app_id==ITERM{return fail("iterm-copy-and-broadcast-state-unobservable")}
 let process=meta.process.as_ref().ok_or_else(||anyhow!("foreground-input-owner-unsupported"))?;
 if !meta.tmux_role.is_empty()||!self.signature.accepts(process){return fail("agent-renderer-version-unverified")}
 let field=self.field(services,session,&meta)?;let selection=field.range("AXSelectedTextRange").ok_or_else(||anyhow!("native-input-range-unavailable"))?;
 let line=field.number("AXInsertionPointLineNumber").filter(|v|v.is_finite()&&*v>=2.0&&v.fract()==0.0).ok_or_else(||anyhow!("native-cursor-line-unavailable"))? as usize;
 if field.line_for_index(selection.location)?!=Some(line){return fail("native-cursor-line-inconsistent")}
 let input=row(&field,line,meta.columns)?;
 let text=input.text.strip_prefix("› ").or_else(||input.text.strip_prefix("» ")).ok_or_else(||anyhow!("codex-main-composer-unrecognized"))?;
 if selection.location!=input.start+input.length-1{return fail("codex-cursor-not-at-draft-end")}
 if !plain_draft(text){return fail("codex-nonplain-or-popup-input")}
 let above=row(&field,line-1,meta.columns)?;let guard=row(&field,line-2,meta.columns)?;let below=row(&field,line+1,meta.columns)?;let footer=row(&field,line+2,meta.columns)?;
 if hidden(&guard.text){return fail("codex-hidden-attachment-row")}
 if !above.text.trim().is_empty()||!below.text.trim().is_empty()||!printable(&footer.text)||!FOOTER.is_match(&footer.text){return fail("codex-modal-or-layout-unverified")}
 if guard.start+guard.length!=above.start||above.start+above.length!=input.start||input.start+input.length!=below.start||below.start+below.length!=footer.start{return fail("native-composer-rows-inconsistent")}
 if field.number("AXNumberOfCharacters").is_none_or(|n|!n.is_finite()||n.fract()!=0.0||n<(footer.start+footer.length) as f64){return fail("native-input-extent-inconsistent")}
 let window=field.element("AXWindow").ok_or_else(||anyhow!("native-window-focus-unverified"))?;
 let field_rect=field.frame().ok_or_else(||anyhow!("native-glyph-geometry-unverified"))?;let window_rect=window.frame().ok_or_else(||anyhow!("native-glyph-geometry-unverified"))?;
 let glyph_units=text.chars().last().unwrap().len_utf16();
 let glyph=field.bounds_for_range(AxRange{location:selection.location-glyph_units,length:glyph_units}).ok_or_else(||anyhow!("native-glyph-geometry-unverified"))?;
 let whole=field.bounds_for_range(AxRange{location:input.start+2,length:selection.location-input.start-2}).ok_or_else(||anyhow!("native-glyph-geometry-unverified"))?;
 let prefix=field.bounds_for_range(AxRange{location:input.start,length:2}).ok_or_else(||anyhow!("native-glyph-geometry-unverified"))?;
 if ![field_rect,window_rect,glyph,whole,prefix].into_iter().all(valid_rect)||!contains(window_rect,field_rect)||(glyph[1]-whole[1]).abs()>0.5||(glyph[3]-whole[3]).abs()>0.5||(prefix[1]-whole[1]).abs()>0.5||(prefix[3]-whole[3]).abs()>0.5||(prefix[0]+prefix[2]-whole[0]).abs()>0.5||(glyph[0]+glyph[2]-whole[0]-whole[2]).abs()>0.5{return fail("native-glyph-geometry-unverified")}
 let caret=[glyph[0]+glyph[2],glyph[1],1.0,glyph[3]];let mut clip=field_rect;let mut ancestor=field.element("AXParent");let mut seen=vec![field.identity()];
 for _ in 0..32{let Some(node)=ancestor else{break};if seen.contains(&node.identity()){return fail("native-input-offscreen")}seen.push(node.identity());if node.bool("AXHidden")==Some(true){return fail("native-input-offscreen")}if let Some(r)=node.frame(){let right=(clip[0]+clip[2]).min(r[0]+r[2]);let bottom=(clip[1]+clip[3]).min(r[1]+r[3]);clip[0]=clip[0].max(r[0]);clip[1]=clip[1].max(r[1]);clip[2]=right-clip[0];clip[3]=bottom-clip[1];}if node==window{break}ancestor=node.element("AXParent");}
 if !valid_rect(clip)||![prefix,whole,caret].into_iter().all(|r|contains(clip,r)){return fail("native-input-offscreen")}
 let snapshot=Snapshot{app_id:meta.app_id.clone(),field_id:format!("terminal:{}",hash((&meta,field.identity(),window.identity()))),text:text.into(),caret:text.chars().count(),caret_rect:Some(caret),multiline:false,field_rect:Some(field_rect),window_id:format!("{}:{}",meta.host_pid,window.identity()),window_revision:hash((selection,line,(&input,&above,&guard,&below,&footer),(field_rect,window_rect,prefix,whole,caret,clip))),window_pid:meta.host_pid,window_bounds:Some(window_rect),can_replace:false,..Default::default()};
 if self.field(services,session,&meta)?!=field||metadata(meta.host_pid,&meta.app_id,&||services.host_alive(session))?!=meta||!self.signature.accepts(process)||field.range("AXSelectedTextRange")!=Some(selection)||row(&field,line,meta.columns)?!=input||row(&field,line-1,meta.columns)?!=above||row(&field,line-2,meta.columns)?!=guard||row(&field,line+1,meta.columns)?!=below||row(&field,line+2,meta.columns)?!=footer||self.field(services,session,&meta)?!=field||services.secure_input()!=Some(false)||services.dead_key(){return fail("terminal-input-changed")}
 let current=services.session()?.ok_or_else(||anyhow!("foreground-changed"))?;if current.foreground.window_pid!=meta.host_pid||current.window!=session.window||!services.allowed(&current,true){return fail("terminal-input-changed")}
 Ok((snapshot,field,meta,selection.location,clip))
 }
 fn read_safe(&mut self,services:&mut CaptureServices<'_>,session:&MacSession)->Option<(Snapshot,AxElement,TerminalMetadata,usize,Rect)>{match self.read(services,session){Ok(v)=>Some(v),Err(error)=>{let message=error.to_string();self.diagnostic=DIAGNOSTICS.iter().copied().find(|v|*v==message).unwrap_or("native-input-unavailable");None}}}
}
const DIAGNOSTICS:&[&str]=&["terminal-opt-in-or-access-required","unsupported-terminal","privacy-or-input-policy","foreground-changed","automation-denied","automation-not-determined","automation-not-running","automation-unavailable","invalid-session-metadata","invalid-font-metadata","invalid-pane-metadata","invalid-process-metadata","metadata-unavailable","metadata-timeout","not-running","inactive-session","session-changed","iterm-copy-and-broadcast-state-unobservable","foreground-input-owner-unsupported","agent-renderer-version-unverified","native-input-focus-unverified","composition-active","composition-unobservable","terminal-selection-active","native-window-focus-unverified","native-window-identity-unverified","native-input-range-unavailable","native-row-unbounded","native-row-inconsistent","wrapped-or-incomplete-input-row","native-cursor-line-unavailable","native-cursor-line-inconsistent","codex-main-composer-unrecognized","codex-cursor-not-at-draft-end","codex-nonplain-or-popup-input","codex-hidden-attachment-row","codex-modal-or-layout-unverified","native-composer-rows-inconsistent","native-input-extent-inconsistent","native-glyph-geometry-unverified","native-input-offscreen","terminal-input-changed"];
impl TerminalIntegration for TerminalAdapter{
 fn permission_state(&mut self,app_id:&str)->Result<String>{Ok(permission_state(app_id,false).into())}
 fn request_access(&mut self,app_id:&str)->Result<String>{Ok(permission_state(app_id,true).into())}
 fn configure(&mut self,config:&Config)->Result<()>{self.enabled=config.terminal_completion;self.stopped=false;self.lease=None;super::AppearanceIntegration::configure(&mut self.appearance,config)?;Ok(())}
 fn capture(&mut self,services:&mut CaptureServices<'_>,session:&MacSession)->Result<Option<Snapshot>>{
  let Some((mut snapshot,field,metadata,location,clip))=self.read_safe(services,session)else{self.lease=None;self.appearance.reset();return Ok(None)};
  self.lease=Some(snapshot.clone());
  register_terminal_font(&metadata);
  let span:String=snapshot.text.chars().rev().take(32).collect::<Vec<_>>().into_iter().rev().collect();let half=span.chars().count()/2;
  if half>0{
   let part:String=span.chars().skip(half).collect();let whole_units=span.encode_utf16().count();let part_units=part.encode_utf16().count();
   if !part.trim().is_empty(){if let (Some(whole),Some(part_rect))=(field.bounds_for_range(AxRange{location:location-whole_units,length:whole_units}),field.bounds_for_range(AxRange{location:location-part_units,length:part_units})){
    let count=cells(&span);let part_count=cells(&part);let advance=if count>0{whole[2]/count as f64}else{0.0};let fixed=if part_count>0&&(part_rect[2]-advance*part_count as f64).abs()<=0.5{advance}else{0.0};
    let evidence=super::geometry::AppearanceEvidence{font_name:metadata.font_name.clone(),font_family:String::new(),font_size:metadata.font_size,fixed_advance:fixed,samples:vec![super::geometry::AppearanceSample{text:span,rect:whole},super::geometry::AppearanceSample{text:part,rect:part_rect}],caret_rect:snapshot.caret_rect.unwrap(),clip_rect:clip,suffix:None,ax_render:None};
    let mut appearance=std::mem::take(&mut self.appearance);let rendered=appearance.request(&snapshot,&evidence,&mut||self.context_matches(services,&snapshot));self.appearance=appearance;
    if let Ok(Some(render))=rendered{if render.valid(snapshot.caret_rect)&&render.suffix.is_empty(){snapshot.inline_render=Some(render)}}
   }}
  }
  if !self.context_matches(services,&snapshot)?{self.lease=None;return Ok(None)}
  self.diagnostic=if snapshot.inline_safe(){"codex-native-input"}else{"codex-font-pixels-unverified"};self.lease=Some(snapshot.clone());Ok(Some(snapshot))
 }
 fn context_matches(&mut self,services:&mut CaptureServices<'_>,snapshot:&Snapshot)->Result<bool>{
  if !lease_matches(self.lease.as_ref(),snapshot){return Ok(false)}
  let Some(session)=services.session()?else{return Ok(false)};
  Ok(self.read_safe(services,&session).is_some_and(|(current,_,_,_,_)|current.same_key(snapshot)&&current.same_authority(snapshot)))
 }
 fn context_ticket(&mut self,services:&mut CaptureServices<'_>,snapshot:&Snapshot)->Result<Option<Box<dyn crate::platform::DetachedContextRead>>>{
  if !self.context_matches(services,snapshot)?{return Ok(None)}let Some(session)=services.session()?else{return Ok(None)};let Some(host)=host_identity(session.foreground.window_pid)else{return Ok(None)};
  let lease=services.detached_lease(&session,snapshot);Ok(Some(Box::new(TerminalContextTicket{lease,host})))
 }
 fn append(&mut self,services:&mut CaptureServices<'_>,snapshot:&Snapshot,text:&str)->Result<EditOutcome>{
  if text.is_empty()||!printable(text)||snapshot.protected||snapshot.selection_length!=0||snapshot.can_replace||snapshot.caret!=snapshot.text.chars().count()||!self.context_matches(services,snapshot)?{return Ok(EditOutcome::Rejected)}
  if !unsafe{CGPreflightPostEventAccess()}||unsafe{CGEventSourceFlagsState(0)}&((1<<20)|(1<<18)|(1<<19))!=0||services.preflight_unicode().is_err()||!self.context_matches(services,snapshot)?||!services.commit_input(){return Ok(EditOutcome::Rejected)}
  if !consume_lease(&mut self.lease,snapshot){return Ok(EditOutcome::Indeterminate)}
  match services.post_unicode(text){Ok(())=>Ok(EditOutcome::Dispatched),Err(_)=>Ok(EditOutcome::Indeterminate)}
 }
 fn stop(&mut self)->Result<()>{self.stopped=true;self.lease=None;super::AppearanceIntegration::stop(&mut self.appearance)?;Ok(())}
}
fn register_terminal_font(metadata:&TerminalMetadata){
 if metadata.font_name!="SFMonoTerminal-Regular"{return}
 unsafe{
 let Some(class)=AnyClass::get(c"NSFont")else{return};let name=NSString::from_str(&metadata.font_name);let font:*mut AnyObject=msg_send![class,fontWithName:&*name,size:metadata.font_size];if !font.is_null(){return}
 #[link(name="CoreFoundation",kind="framework")]unsafe extern "C"{fn CFURLCreateWithFileSystemPath(allocator:Raw,path:Raw,style:isize,directory:bool)->Raw;}
 #[link(name="CoreText",kind="framework")]unsafe extern "C"{fn CTFontManagerRegisterFontsForURL(url:Raw,scope:u32,error:*mut Raw)->bool;}
 let Some(path)=AxValue::string("/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SFMono-Terminal.ttf")else{return};
 let Some(url)=AxValue::from_owned(CFURLCreateWithFileSystemPath(std::ptr::null(),path.as_raw(),0,false))else{return};let mut error=std::ptr::null();let _=CTFontManagerRegisterFontsForURL(url.as_raw(),1,&mut error);drop(AxValue::from_owned(error));
 }
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn exact_signing_pin_is_not_version_or_path(){assert!(CODEX_REQUIREMENT.contains("9ddc61acceef79aa2aad437ba1fe94239f2e0423"));assert!(CODEX_REQUIREMENT.contains("2DC432GLL2"));assert!(!CODEX_REQUIREMENT.contains("/opt"));}
 #[test]fn reserved_input_refusals(){for s in ["/help","!ls","@file hello","use $skill","[Pasted Content 900 chars]","[Image #2]","hello\u{202e}","שלום"]{assert!(!plain_draft(s),"{s}")}assert!(plain_draft("Please explain [ordinary] 😀 e\u{301}"));}
 #[test]fn tty_paths_never_escape(){for s in ["/dev/ttys001/../../secret","/dev/tty","/dev/tty-x","/tmp/ttys001"]{assert!(!valid_tty(s))}assert!(valid_tty("/dev/ttys001"));}
 #[test]fn diagnostics_never_publish_native_text(){let text="invalid digit found in string: private content";assert!(!DIAGNOSTICS.contains(&text));}
 #[test]fn append_lease_is_exact_authority_and_one_use(){let snapshot=Snapshot{app_id:TERMINAL.into(),field_id:"terminal:synthetic".into(),text:"draft 😀e\u{301}".into(),caret:9,window_id:"window".into(),window_revision:"generation1".into(),can_replace:false,..Default::default()};let mut lease=Some(snapshot.clone());let mut stale=snapshot.clone();stale.window_revision="generation2".into();assert!(!consume_lease(&mut lease,&stale));assert!(lease.is_some());assert!(consume_lease(&mut lease,&snapshot));assert!(!consume_lease(&mut lease,&snapshot));}
 #[test]fn exact_east_asian_width_does_not_guess_emoji_or_mark_cells(){assert_eq!(cells("☰次e\u{301}"),4);assert_eq!(cells("\u{301}"),0);}
 #[test]fn applev_events_cannot_prompt_during_read(){assert_eq!(std::mem::offset_of!(AeDesc,handle),4);assert_eq!(std::mem::size_of::<AeDesc>(),4+std::mem::size_of::<*mut c_void>());assert_ne!(READ_OPTIONS&0x20000,0);assert_ne!(READ_OPTIONS&0x1000,0);assert_eq!(READ_OPTIONS&0x30,0x10);}
 #[test]fn unsigned_or_nonleader_process_rejected_before_native_lookup(){let process=ProcessIdentity{pid:11,ppid:1,pgid:12,started:(1,0),executable:"/relocated/codex".into()};assert!(!CodeIdentity{requirement:None}.accepts(&process));}
}
