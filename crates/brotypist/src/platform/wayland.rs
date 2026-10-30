//! KWin-launched input-method-v1 bridge. Never opens an ordinary/public display.
//! The private socket, all protocol objects and their pump belong to the constructing thread.
#[path="wayland_protocol.rs"] mod protocol;
#[path="wayland_panel.rs"] mod panel;
#[path="wayland_wire.rs"] mod wire;
use super::{linux::LinuxBridge,policy::ObservationPolicy,ContextRead,EditOutcome,Foreground,NativeBridge,Permission,ShortcutHandler};
use crate::{config::Config,core::{Rect,Snapshot}};
use anyhow::{Result,bail,Context};
use std::{os::fd::{AsRawFd,FromRawFd,OwnedFd},sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}},thread::{self,JoinHandle,ThreadId},time::{Duration,Instant}};
static PREPARED:Mutex<Option<OwnedFd>>=Mutex::new(None);
/// The caller transfers an inherited private socket, never a WAYLAND_DISPLAY name.
/// Call before any GUI toolkit; the launch owner removes WAYLAND_SOCKET immediately afterward.
/// A second preparation is rejected without consuming the caller's descriptor.
pub fn prepare_input_method(fd:i32)->Result<()>{prepare_slot(&PREPARED,fd,std::env::var_os("WAYLAND_SOCKET").as_deref())}
fn prepare_slot(slot:&Mutex<Option<OwnedFd>>,fd:i32,advertised:Option<&std::ffi::OsStr>)->Result<()>{
 if fd<0{bail!("A private input-method socket descriptor is required");}
 let mut prepared=slot.lock().unwrap_or_else(|e|e.into_inner());
 if prepared.is_some(){bail!("A private input-method socket is already prepared");}
 // From this point the descriptor is consumed on every path, including validation failure.
 let owned=unsafe{OwnedFd::from_raw_fd(fd)};
 if let Some(advertised)=advertised{
  if advertised.to_str().and_then(|s|s.parse::<i32>().ok())!=Some(fd){bail!("WAYLAND_SOCKET does not identify this private descriptor");}
 }
 validate_private_socket(&owned)?;*prepared=Some(owned);Ok(())
}
fn validate_private_socket(fd:&OwnedFd)->Result<()>{unsafe{
 let fd=fd.as_raw_fd();let mut ty:libc::c_int=0;let mut ty_len=std::mem::size_of_val(&ty) as libc::socklen_t;
 let mut peer:libc::sockaddr_storage=std::mem::zeroed();let mut peer_len=std::mem::size_of_val(&peer) as libc::socklen_t;
 if libc::getsockopt(fd,libc::SOL_SOCKET,libc::SO_TYPE,(&mut ty as *mut libc::c_int).cast(),&mut ty_len)!=0||ty!=libc::SOCK_STREAM
  ||libc::getpeername(fd,(&mut peer as *mut libc::sockaddr_storage).cast(),&mut peer_len)!=0||peer.ss_family as i32!=libc::AF_UNIX{bail!("A connected private Unix stream socket is required");}
 let flags=libc::fcntl(fd,libc::F_GETFD);if flags<0||libc::fcntl(fd,libc::F_SETFD,flags|libc::FD_CLOEXEC)<0{return Err(std::io::Error::last_os_error().into());}
 Ok(())
}}
pub fn is_prepared()->bool{PREPARED.lock().unwrap_or_else(|e|e.into_inner()).is_some()}
pub fn cleanup_prepared()->Result<()>{PREPARED.lock().unwrap_or_else(|e|e.into_inner()).take();Ok(())}
fn claim_prepared(slot:&Mutex<Option<OwnedFd>>)->Result<OwnedFd>{slot.lock().unwrap_or_else(|e|e.into_inner()).take().context("Wayland requires an explicit KWin input-method launch with its private socket")}
fn manual_offer_allowed(anchor:&str,text:&str)->bool{!anchor.is_empty()&&text.is_empty()}

struct Bootstrap{task:JoinHandle<()>}
fn bootstrap_kwin(token:Arc<AtomicU64>,expected:u64)->Result<()>{
 use zbus::{blocking::{connection::Builder,Proxy},zvariant::{OwnedValue,Value}};
 let current=||token.load(Ordering::Acquire)==expected;
 if !current(){return Ok(());}
 let connection=Builder::session()?.method_timeout(Duration::from_millis(200)).build()?;
 let proxy=Proxy::new(&connection,"org.kde.KWin","/VirtualKeyboard","org.freedesktop.DBus.Properties")?;
 let active:OwnedValue=proxy.call("Get",&("org.kde.kwin.VirtualKeyboard","active"))?;
 if !current()||bool::try_from(active).ok()!=Some(true){return Ok(());}
 let supports:OwnedValue=proxy.call("Get",&("org.kde.kwin.VirtualKeyboard","activeClientSupportsTextInput"))?;
 if !current()||bool::try_from(supports).ok()!=Some(true){return Ok(());}
 // Adopt the current context only; never enable an inactive input method or alter visibility/purpose.
 let _:()=proxy.call("Set",&("org.kde.kwin.VirtualKeyboard","active",Value::from(true)))?;Ok(())
}

pub struct WaylandBridge{accessibility:LinuxBridge,wire:Box<wire::Wire>,owner:ThreadId,bootstrap:Vec<Bootstrap>}
impl WaylandBridge{
 pub fn new()->Result<Self>{
  let fd=claim_prepared(&PREPARED)?;
  let accessibility=LinuxBridge::accessibility_only()?;let policy=ObservationPolicy::new(&Config::default())?;
  let wire=wire::Wire::connect(fd,policy)?;
  let mut this=Self{accessibility,wire,owner:thread::current().id(),bootstrap:Vec::new()};this.queue_bootstrap();Ok(this)
 }
 fn owner(&self)->Result<()>{if thread::current().id()!=self.owner{bail!("Private KWin connection belongs to its GUI owner thread");}Ok(())}
 fn queue_bootstrap(&mut self){
  // The D-Bus task owns only its own bus connection; it never touches the private Wayland connection or AT-SPI.
  let mut index=0;while index<self.bootstrap.len(){if self.bootstrap[index].task.is_finished(){let task=self.bootstrap.swap_remove(index);let _=task.task.join();}else{index+=1;}}
  let phase=self.wire.state.phase();let context=self.wire.state.context_id;
  if self.wire.failed||self.wire.stopped||phase==0||self.wire.last_bootstrap==(context,phase){return;}
  self.wire.last_bootstrap=(context,phase);let token=self.wire.bootstrap_token.clone();let expected=token.load(Ordering::Acquire);
  if let Ok(task)=thread::Builder::new().name("kwin-context-bootstrap".into()).spawn(move||{let _=bootstrap_kwin(token,expected);}){self.bootstrap.push(Bootstrap{task});}
 }
 fn capture_native(&mut self)->Result<Option<Snapshot>>{
  self.owner()?;if self.wire.failed||self.wire.stopped{return Ok(None);}
  let Some(before)=self.wire.state.state()else{return Ok(None)}; // Purpose must be safe before any AT-SPI Text request.
  let context_id=self.wire.state.context_id;
  let Some(mut snapshot)=self.accessibility.capture_accessibility(false,true)?else{return Ok(None)};
  if snapshot.protected||snapshot.selection_length!=0||snapshot.text.contains('\0')||!self.accessibility.accessibility_context_matches(&snapshot)?{return Ok(None);}
  let Some(cursor)=snapshot.text.index().scalar_to_utf8(crate::core::ScalarIndex(snapshot.caret)).map(|v|v.0)else{return Ok(None)};
  // Drain events received while the synchronous AT-SPI read was in progress, then check its authority.
  self.wire.pump()?;
  if context_id!=self.wire.state.context_id||!self.wire.state.matches(before,&snapshot.text,cursor){return Ok(None);}
  // A second accessibility read proves caret/count/selection and focus stayed unchanged during corroboration.
  let Some(after)=self.accessibility.capture_accessibility(false,true)?else{return Ok(None)};
  if !after.same_key(&snapshot)||!after.same_authority(&snapshot)||!self.accessibility.accessibility_context_matches(&after)?{return Ok(None);}
  self.wire.pump()?;if context_id!=self.wire.state.context_id||!self.wire.state.matches(before,&snapshot.text,cursor){return Ok(None);}
  self.wire.app.clone_from(&snapshot.app_id);self.wire.domain.clone_from(&snapshot.domain);
  snapshot.field_id=format!("{}:wl:{}",snapshot.field_id,context_id);snapshot.native_anchor=wire::anchor(before);
  snapshot.caret_rect=None;snapshot.field_rect=None;snapshot.inline_render=None;snapshot.native_inline=false;Ok(Some(snapshot))
 }
 fn confirm(&mut self,expected:&Snapshot,context_id:u64)->Result<Option<Snapshot>>{
  let deadline=Instant::now()+Duration::from_millis(250);
  while Instant::now()<deadline{
   if self.wire.pump().is_err()||self.wire.failed||!self.wire.state.active||self.wire.state.context_id!=context_id{return Ok(None);}
   // Metadata authority is checked before text reads; selection/protection changes refuse inside the helper.
   if !self.accessibility.accessibility_context_matches(expected)?||Instant::now()>=deadline{return Ok(None);}
   let Some(observed)=self.accessibility.capture_accessibility(false,true)?else{return Ok(None)};
   if Instant::now()>=deadline||!observed.same_authority(expected){return Ok(None);}
   if observed.same_key(expected){
    if !self.accessibility.accessibility_context_matches(&observed)?||Instant::now()>=deadline{return Ok(None);}
    self.wire.pump()?;if self.wire.failed||!self.wire.state.active||self.wire.state.context_id!=context_id||Instant::now()>=deadline{return Ok(None);}
    let mut confirmed=observed;confirmed.field_id=format!("{}:wl:{context_id}",confirmed.field_id);
    if let Some(state)=self.wire.state.state(){confirmed.native_anchor=wire::anchor(state);}
    confirmed.native_inline=false;confirmed.caret_rect=None;confirmed.field_rect=None;confirmed.inline_render=None;return Ok(Some(confirmed));
   }
   thread::sleep(Duration::from_millis(3));
  }Ok(None)
 }
 /// Shell may supply its current work-area, device scale and appearance; no compositor-global inference.
 pub fn set_panel_environment(&mut self,width:i32,height:i32,scale:i32,dark:bool)->Result<()>{self.owner()?;self.wire.panel.bounds=[width.max(44),height.max(80)];self.wire.panel.scale=scale.clamp(1,4);self.wire.panel.dark=dark;if self.wire.panel.snapshot.is_some()&&!self.wire.render()?{self.wire.hide();}Ok(())}
}
impl NativeBridge for WaylandBridge{
 fn configure(&mut self,config:&Config)->Result<()>{self.owner()?;let policy=ObservationPolicy::new(config)?;self.accessibility.configure(config)?;self.wire.policy=policy;self.wire.hide();if !self.wire.failed&&!self.wire.stopped{self.wire.flush()?;}Ok(())}
 fn capture(&mut self)->Result<Option<Snapshot>>{self.capture_native()}
 fn foreground(&mut self)->Result<Option<Foreground>>{self.owner()?;if self.wire.failed||self.wire.stopped{return Ok(None);}self.accessibility.foreground()}
 fn insert(&mut self,snapshot:&Snapshot,text:&str)->Result<EditOutcome>{
  self.owner()?;
  if text.is_empty()||text.len()>4079||text.contains('\0')||snapshot.protected||snapshot.selection_length!=0{return Ok(EditOutcome::Rejected);}
  let Some(current)=self.capture_native()?else{return Ok(EditOutcome::Rejected)};
  if !current.same_key(snapshot)||!current.same_authority(snapshot)||current.native_anchor!=snapshot.native_anchor{return Ok(EditOutcome::Rejected);}
  let Some(state)=self.wire.state.state()else{return Ok(EditOutcome::Rejected)};
  if wire::anchor(state)!=snapshot.native_anchor{return Ok(EditOutcome::Rejected);}
  let context=self.wire.state.context_id;let suffix=format!(":wl:{context}");let Some(base)=current.field_id.strip_suffix(&suffix)else{return Ok(EditOutcome::Rejected)};
  let mut expected=current.clone();expected.field_id=base.into();expected.native_anchor.clear();expected.text=format!("{}{}{}",current.prefix(),text,current.suffix()).into();expected.caret=current.caret+text.chars().count();
  match self.wire.commit(state,text){Ok(false)=>return Ok(EditOutcome::Rejected),Err(_)=>return Ok(EditOutcome::Indeterminate),Ok(true)=>{}}
  match self.confirm(&expected,context){Ok(Some(confirmed))=>{self.wire.hide();Ok(EditOutcome::Applied(confirmed))},_=>Ok(EditOutcome::Indeterminate)}
 }
 fn replace_range(&mut self,snapshot:&Snapshot,start:usize,length:usize,text:&str)->Result<EditOutcome>{if length!=0||start!=snapshot.caret||snapshot.selection_length!=0||snapshot.text.index().scalar_range_to_utf8(start,length).is_none(){return Ok(EditOutcome::Rejected);}self.insert(snapshot,text)}
 fn range_rect(&mut self,_snapshot:&Snapshot,_start:usize,_length:usize)->Result<Option<Rect>>{Ok(None)}
 // Baseline private-connection context workers must not inspect compositor authority off the GUI thread.
 fn focused_context(&mut self,_snapshot:&Snapshot,_authorized:&dyn Fn()->bool,_max_chars:usize)->Result<ContextRead>{Ok(ContextRead::blocked("wayland"))}
 fn context_matches(&mut self,_snapshot:&Snapshot)->Result<bool>{Ok(false)}
 fn start_shortcuts(&mut self,callback:ShortcutHandler)->Result<()>{self.owner()?;if self.wire.stopped||self.wire.failed{bail!("KWin input-method bridge unavailable");}self.wire.callback=Some(callback);Ok(())}
 fn set_shortcuts_enabled(&mut self,enabled:bool){if self.owner().is_ok(){self.wire.enabled=enabled&&!self.wire.stopped&&!self.wire.failed;self.wire.panel.pressed=None;}}
 fn set_input_callback(&mut self,callback:Option<Arc<dyn Fn()+Send+Sync>>){if self.owner().is_ok(){self.wire.input_callback=if self.wire.stopped{None}else{callback};}}
 fn finish_input(&mut self,_accepted:bool)->Result<()>{self.owner()?;Ok(())} // Grab forwards real hardware edges; no injected replay queue exists.
 fn input_revision(&self)->u64{self.wire.revision}
 fn user_input_revision(&self)->u64{self.wire.user_revision}
 fn input_timestamp(&self)->Option<Instant>{self.wire.timestamp}
 fn secure_input_enabled(&self)->Option<bool>{Some(false)} // Per-context purpose/hints and AT-SPI protection remain mandatory.
 fn request_permission(&mut self,kind:&str)->Result<Permission>{self.owner()?;Ok(match kind{"accessibility"=>self.accessibility.request_permission(kind)?,"input"|"keyboard" if !self.wire.failed&&!self.wire.stopped=>Permission::Allowed,_=>Permission::Unknown})}
 fn status(&self)->serde_json::Value{serde_json::json!({"platform":"wayland","available":!self.wire.failed&&!self.wire.stopped,"accessibility":self.accessibility.status(),"native_overlay":true,"native_inline":false,"domain_observable":false,"ready":self.wire.state.ready,"purpose_known":self.wire.state.known,"purpose_safe":self.wire.state.safe,"message":self.wire.diagnostic})}
 fn pump_events(&mut self)->Result<()>{self.owner()?;if self.wire.stopped{return Ok(());}let result=self.wire.pump();self.queue_bootstrap();result}
 fn native_event_source(&self)->Option<(i32,bool)>{self.wire.fd()}
 fn bind_native_ui(&mut self,callback:Option<Arc<dyn Fn(String)+Send+Sync>>){if self.owner().is_ok()&&!self.wire.stopped{self.wire.ui_callback=callback;}}
 fn show_native_menu(&mut self,rows:Vec<(String,String,bool)>)->Result<bool>{self.owner()?;let Some(offered)=self.wire.panel.snapshot.clone()else{return Ok(false)};let Some(current)=self.capture_native()?else{self.wire.hide();return Ok(false)};if !current.same_key(&offered)||!current.same_authority(&offered)||current.native_anchor!=offered.native_anchor{self.wire.hide();return Ok(false);}match self.wire.show_menu(rows){Ok(true)=>Ok(true),_=>{self.wire.hide();Ok(false)}}}
 fn present_native(&mut self,anchor:&str,text:&str,paused:bool)->Result<bool>{self.owner()?;if self.wire.panel.menu.is_some(){return Ok(false);}if !manual_offer_allowed(anchor,text){self.wire.hide();return Ok(false);}let Some(current)=self.capture_native()?else{self.wire.hide();return Ok(false)};if current.native_anchor!=anchor{self.wire.hide();return Ok(false);}self.wire.panel.snapshot=Some(current);self.wire.panel.paused=paused;match self.wire.render(){Ok(true)=>Ok(true),_=>{self.wire.hide();Ok(false)}}}
 fn hide_native(&mut self)->Result<()>{self.owner()?;self.wire.hide();if !self.wire.failed&&!self.wire.stopped{self.wire.flush()?;}Ok(())}
 fn stop(&mut self)->Result<()>{self.owner()?;self.wire.close();self.accessibility.stop()?;self.queue_bootstrap();if !self.bootstrap.is_empty(){bail!("KWin context bootstrap is retiring; retry shutdown after its bounded D-Bus call returns");}Ok(())}
}
impl Drop for WaylandBridge{fn drop(&mut self){self.wire.close();for task in self.bootstrap.drain(..){let _=task.task.join();}}}
#[cfg(test)]mod tests{
 use super::*;use std::{io::Read,os::{fd::IntoRawFd,unix::net::{UnixDatagram,UnixStream}}};
 #[test]fn private_socket_validation_requires_connected_unix_stream_and_sets_cloexec(){let (a,_b)=UnixStream::pair().unwrap();let a:OwnedFd=a.into();validate_private_socket(&a).unwrap();assert_ne!(unsafe{libc::fcntl(a.as_raw_fd(),libc::F_GETFD)}&libc::FD_CLOEXEC,0);let a:OwnedFd=UnixDatagram::unbound().unwrap().into();assert!(validate_private_socket(&a).is_err());let file:OwnedFd=std::fs::File::open("/dev/null").unwrap().into();assert!(validate_private_socket(&file).is_err());}
 #[test]fn no_prepared_socket_never_attempts_public_display(){
  let prepared=Mutex::new(None::<OwnedFd>);assert!(claim_prepared(&prepared).is_err());
 }
 #[test]fn preparation_is_private_cloexec_and_exactly_once_without_environment_mutation(){
  let slot=Mutex::new(None);let (a,mut peer)=UnixStream::pair().unwrap();let raw=a.into_raw_fd();
  prepare_slot(&slot,raw,Some(std::ffi::OsStr::new(&raw.to_string()))).unwrap();
  assert!(prepare_slot(&slot,raw,None).is_err());let claimed=claim_prepared(&slot).unwrap();
  assert_eq!(claimed.as_raw_fd(),raw);assert!(claim_prepared(&slot).is_err());drop(claimed);
  let mut byte=[0];assert_eq!(peer.read(&mut byte).unwrap(),0);
 }
 #[test]fn advertisement_mismatch_consumes_only_the_incoming_socket(){
  let slot=Mutex::new(None);let (a,mut peer)=UnixStream::pair().unwrap();let raw=a.into_raw_fd();
  assert!(prepare_slot(&slot,raw,Some(std::ffi::OsStr::new("-1"))).is_err());
  assert!(claim_prepared(&slot).is_err());let mut byte=[0];assert_eq!(peer.read(&mut byte).unwrap(),0);
 }
 #[test]fn ordinary_inline_preedit_and_popup_requests_are_refused_before_capture(){
  assert!(manual_offer_allowed("4:7",""));for text in [" tomorrow"," tomorrow\nnext line","\0"]{assert!(!manual_offer_allowed("4:7",text));}assert!(!manual_offer_allowed("",""));
 }
}
