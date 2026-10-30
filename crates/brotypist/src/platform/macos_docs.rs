//! Metadata-only Docs hints and explicit, one-use native accessibility setup.
use super::{CaptureServices,MacSession,ax::{AxElement,AxValue,Raw},input::InputSourceKind};
use crate::{core::Rect,platform::policy::is_browser};
use rand::{RngCore,rngs::OsRng};
use serde::Serialize;
use sha2::{Sha256,Digest};
use std::time::{Duration,Instant};
pub const HELP_URL:&str="https://support.google.com/docs/answer/6282736?hl=en#Turn_on_screen_reader_support";
const TOKEN_SECONDS:Duration=Duration::from_secs(30);
const UI_TIMEOUT:Duration=Duration::from_millis(600);
type Check<T>=std::result::Result<T,&'static str>;
#[derive(Clone,Debug,Serialize)]pub struct DocsHint{pub token:String,pub app_id:String,pub pid:i32,pub window_id:String,pub document_id:String,pub anchor:Rect,pub support:&'static str,pub help_url:&'static str}
#[derive(Clone,Debug,Serialize)]pub struct DocsOutcome{pub status:&'static str,pub diagnostic:&'static str,pub help_url:&'static str}
struct Target{token:String,created:Instant,session:MacSession,document_id:String,window_id:String,window:AxElement,web:AxElement,focus:AxElement,anchor:Rect,tools:Option<AxElement>}
struct Session{session:MacSession,document_id:String,window:AxElement,web:AxElement,focus:AxElement,ancestors:Vec<AxElement>}
pub struct DocsHelper{salt:[u8;32],target:Option<Target>,pub diagnostic:&'static str}
impl Default for DocsHelper{fn default()->Self{let mut salt=[0;32];OsRng.fill_bytes(&mut salt);Self{salt,target:None,diagnostic:"docs_not_observed"}}}
fn random()->String{let mut bytes=[0u8;24];OsRng.fill_bytes(&mut bytes);bytes.iter().map(|b|format!("{b:02x}")).collect()}
#[link(name="CoreFoundation",kind="framework")]unsafe extern "C"{fn CFNumberIsFloatType(value:Raw)->u8;}
fn native_bool(value:&AxValue)->Option<bool>{
 if let Some(value)=value.as_bool(){return Some(value)}
 let number=value.as_number()?;if unsafe{CFNumberIsFloatType(value.as_raw())}!=0{return None}
 match number{0.0=>Some(false),1.0=>Some(true),_=>None}
}
fn boolean_attribute(node:&AxElement,name:&str)->Option<bool>{native_bool(&node.attribute(name).ok()??)}
fn token_valid(expected:&str,provided:&str,elapsed:Duration)->bool{elapsed<TOKEN_SECONDS&&equal_secret(expected,provided)}
fn equal_secret(a:&str,b:&str)->bool{let mut difference=a.len()^b.len();for (x,y) in a.bytes().zip(b.bytes()){difference|=(x^y)as usize;}difference==0}
fn document_url(value:&str)->Check<&str>{
 if value.chars().any(|c|c as u32<=32){return Err("docs_url_unverified")}
 // Parse only after exact authority validation: URL canonicalization must not
 // turn a spoofed host, credential, percent-encoded ID or default port into proof.
 let rest=value.strip_prefix("https://").ok_or("docs_url_unverified")?;
 let (authority,path)=rest.split_once('/').ok_or("docs_url_unverified")?;
 if !["docs.google.com","docs.google.com:443"].contains(&authority){return Err("docs_url_unverified")}
 let path=path.split(['?','#']).next().unwrap_or("");let mut parts=path.split('/');
 if parts.next()!=Some("document"){return Err("docs_url_unverified")}
 let next=parts.next();if next==Some("u"){let user=parts.next().ok_or("docs_url_unverified")?;if user.is_empty()||!user.bytes().all(|c|c.is_ascii_digit())||parts.next()!=Some("d"){return Err("docs_url_unverified")}}else if next!=Some("d"){return Err("docs_url_unverified")}
 let id=parts.next().ok_or("docs_url_unverified")?;
 if id.is_empty()||!id.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'_'||c==b'-')||parts.next()!=Some("edit")||parts.next().is_some(){return Err("docs_url_unverified")}
 Ok(id)
}
fn safe(node:&AxElement,secure_description:&str,hidden:bool,disabled:bool)->Check<()>{
 let protected=node.attribute("AXProtectedContent").map_err(|_|"native_control_unavailable")?;
 if protected.as_ref().is_some_and(|v|v.as_bool().or_else(||v.as_number().map(|n|n!=0.0)).unwrap_or(true))||node.string("AXSubrole").as_deref()==Some("AXSecureTextField"){return Err("native_control_unavailable")}
 let enabled=node.attribute("AXEnabled").map_err(|_|"native_control_unavailable")?;
 if enabled.as_ref().is_some_and(|v|native_bool(v).is_none_or(|b|!disabled&&!b)){return Err("native_control_unavailable")}
 if !secure_description.is_empty()&&node.string("AXRoleDescription").as_deref()==Some(secure_description){return Err("native_control_protected")}
 let visibility=node.attribute("AXHidden").map_err(|_|"native_control_hidden")?;
 if !hidden&&visibility.as_ref().is_some_and(|v|v.as_bool().or_else(||v.as_number().map(|n|n!=0.0)).unwrap_or(true)){return Err("native_control_hidden")}
 Ok(())
}
fn ancestry(node:&AxElement,description:&str,hidden:bool)->Check<Vec<AxElement>>{
 let mut result=Vec::new();let mut next=Some(node.clone());
 for _ in 0..32{let Some(node)=next else{return Ok(result)};if result.contains(&node){return Err("native_ancestry_ambiguous")}safe(&node,description,hidden,false)?;next=node.element("AXParent");result.push(node);}
 if next.is_some(){Err("native_ancestry_unbounded")}else{Ok(result)}
}
fn nodes(root:&AxElement)->Check<Vec<AxElement>>{let mut queue=vec![root.clone()];let mut result=Vec::new();while let Some(node)=queue.pop(){if result.contains(&node){return Err("docs_controls_ambiguous")}if result.len()>=2048{return Err("docs_controls_unbounded")}queue.extend(node.children().unwrap_or_default());result.push(node);}Ok(result)}
fn owned(node:&AxElement,pid:i32)->Check<()>{if node.pid()!=Some(pid){Err("native_owner_unverified")}else{Ok(())}}
fn checked_value(node:&AxElement)->Check<bool>{boolean_attribute(node,"AXValue").ok_or("docs_checkbox_state_unknown")}
impl DocsHelper{
 pub fn new()->Self{Self::default()}
 pub fn reset(&mut self){self.target=None;}
 fn document(&self,node:&AxElement,attribute:&str)->Check<String>{let value=node.string(attribute).ok_or("docs_url_unverified")?;let id=document_url(&value)?;
 // HMAC-SHA256, implemented over SHA-256's fixed 64-byte block. Neither literal
 // document IDs nor URLs leave this helper or become globally joinable hashes.
 let mut inner=[0x36;64];let mut outer=[0x5c;64];for i in 0..32{inner[i]^=self.salt[i];outer[i]^=self.salt[i];}let mut digest=Sha256::new();digest.update(inner);digest.update(id.as_bytes());let body=digest.finalize();let mut digest=Sha256::new();digest.update(outer);digest.update(body);Ok(format!("{:x}",digest.finalize()))}
 fn session(&self,services:&mut CaptureServices<'_>,authorized:&dyn Fn()->bool,target:Option<&mut Target>,scopes:&[AxElement],committed:bool,hidden:bool)->Check<Session>{
 if !authorized()||!super::ax::trusted()||services.secure_input()!=Some(false)||services.dead_key()||services.source_kind()!=InputSourceKind::Layout{return Err("docs_security_unavailable")}
 let mut native=services.session().map_err(|_|"docs_foreground_unavailable")?.ok_or("docs_foreground_unavailable")?;
 if !is_browser(&[&native.foreground.app_id]){return Err("docs_browser_unverified")}
 if !services.allowed(&native,true){return Err("docs_policy_denied")}
 let pid=native.foreground.window_pid;let window=native.app.element("AXFocusedWindow").ok_or("docs_window_unverified")?;owned(&window,pid)?;safe(&window,services.secure_description(),false,false)?;
 let document_id=self.document(&window,"AXDocument")?;native.foreground.domain="docs.google.com".into();if !services.allowed(&native,true){return Err("docs_policy_denied")}
 if let Some(t)=target.as_deref(){if t.session.foreground.window_pid!=pid||t.session.foreground.app_id!=native.foreground.app_id||t.window!=window||t.document_id!=document_id{return Err("docs_target_changed")}}
 let focus=native.app.element("AXFocusedUIElement").ok_or("docs_focus_unverified")?;if focus.bool("AXFocused")==Some(false){return Err("docs_focus_unverified")}owned(&focus,pid)?;
 if focus.element("AXWindow").is_some_and(|w|w!=window){return Err("docs_focus_outside_document")}
 let role=focus.string("AXRole");let editor=role.as_deref()==Some("AXTextArea")||(role.as_deref()==Some("AXGroup")&&focus.string("AXDOMIdentifier").as_deref()==Some("docs-texteventtarget-descendant"));
 if target.is_none()&&!editor{return Err("docs_focus_unverified")}
 if role.as_deref()==Some("AXTextArea")&&focus.attribute("AXMarkedTextRange").map_err(|_|"docs_security_unavailable")?.is_some_and(|v|v.as_range().is_none_or(|r|r.length!=0)){return Err("docs_security_unavailable")}
 let ancestors=ancestry(&focus,services.secure_description(),hidden)?;for node in &ancestors{owned(node,pid)?}if !ancestors.contains(&window){return Err("docs_focus_outside_document")}
 let areas:Vec<_>=ancestors.iter().filter(|n|n.string("AXRole").as_deref()==Some("AXWebArea")).cloned().collect();let web=areas.last().cloned().ok_or("docs_focus_outside_document")?;
 if areas.len()!=1{let frame=&areas[0];let parent=frame.element("AXParent").ok_or("docs_focus_outside_document")?;if areas.len()!=2||!editor||frame.string("AXURL").as_deref()!=Some("about:blank")||parent.string("AXRole").as_deref()!=Some("AXGroup")||!parent.strings("AXDOMClassList").ok().flatten().unwrap_or_default().iter().any(|s|s=="docs-texteventtarget-iframe")||self.document(&web,"AXURL")?!=document_id{return Err("docs_focus_outside_document")}}
 for attribute in ["AXDocument","AXURL"]{if web.attribute(attribute).map_err(|_|"docs_document_ambiguous")?.is_some()&&self.document(&web,attribute)?!=document_id{return Err("docs_document_ambiguous")}}
 if let Some(t)=target{if t.web!=web{return Err("docs_target_changed")}if focus!=t.focus&&!scopes.iter().any(|scope|ancestors.contains(scope)){if !committed||(!editor&&t.tools.as_ref()!=Some(&focus)){return Err("docs_focus_changed")}t.focus=focus.clone();}}
 Ok(Session{session:native,document_id,window,web,focus,ancestors})
 }
 fn anchor(&self,session:&Session,description:&str)->Check<Rect>{
 session.focus.frame().ok_or("docs_anchor_unverified")?;
 let position=session.ancestors.iter().position(|n|n==&session.web).ok_or("docs_anchor_unverified")?;let mut clip:Option<Rect>=None;
 for node in &session.ancestors[position..]{let role=node.string("AXRole");let bounds=node.frame();if bounds.is_none()&&!matches!(role.as_deref(),Some("AXWebArea"|"AXWindow")){continue}let bounds=bounds.ok_or("docs_anchor_unverified")?;if matches!(role.as_deref(),Some("AXWebArea"|"AXWindow"))&&(bounds[2]<80.0||bounds[3]<80.0){return Err("docs_anchor_unverified")}
 clip=Some(match clip{None=>bounds,Some(c)=>{let x=c[0].max(bounds[0]);let y=c[1].max(bounds[1]);[x,y,(c[0]+c[2]).min(bounds[0]+bounds[2])-x,(c[1]+c[3]).min(bounds[1]+bounds[3])-y]}});if node==&session.window{break}}
 let clip=clip.ok_or("docs_anchor_unverified")?;if clip[2]<80.0||clip[3]<80.0{return Err("docs_anchor_unverified")}
 let mut editors=Vec::new();let mut pages=Vec::new();for node in nodes(&session.web)?{if node.string("AXRole").as_deref()!=Some("AXGroup")||node.bool("AXHidden")==Some(true){continue}let classes=node.strings("AXDOMClassList").map_err(|_|"docs_anchor_unverified")?.unwrap_or_default();if classes.iter().any(|s|s=="kix-appview-editor"){editors.push(node.clone())}if classes.iter().any(|s|s=="kix-page-paginated")&&classes.iter().any(|s|s=="canvas-first-page"){pages.push(node)}}
 if editors.len()!=1||pages.len()!=1{return Err("docs_anchor_unverified")}let editor=&editors[0];let page=&pages[0];for node in [editor,page]{let chain=ancestry(node,description,false)?;for parent in &chain{owned(parent,session.session.foreground.window_pid)?}if !chain.contains(&session.web)||!chain.contains(&session.window)||(node==page&&!chain.contains(editor)){return Err("docs_anchor_unverified")}}
 let e=editor.frame().ok_or("docs_anchor_unverified")?;let p=page.frame().ok_or("docs_anchor_unverified")?;let left=clip[0].max(e[0]);let top=clip[1].max(e[1]).max(p[1]);let right=(clip[0]+clip[2]).min(e[0]+e[2]);let bottom=(clip[1]+clip[3]).min(e[1]+e[3]).min(p[1]+p[3]);if p[0]-left<34.0||p[0]>right||top+40.0>bottom{return Err("docs_anchor_unverified")}Ok([p[0],top+40.0,1.0,1.0])
 }
 pub fn hint(&mut self,services:&mut CaptureServices<'_>,authorized:&dyn Fn()->bool)->Option<DocsHint>{
 let result=(||->Check<DocsHint>{let session=self.session(services,authorized,None,&[],false,false)?;let anchor=self.anchor(&session,services.secure_description())?;
 let mut target=match self.target.take(){Some(t)if t.created.elapsed()<TOKEN_SECONDS&&t.session.foreground.app_id==session.session.foreground.app_id&&t.session.foreground.window_pid==session.session.foreground.window_pid&&t.document_id==session.document_id&&t.window==session.window&&t.web==session.web&&t.focus==session.focus=>t,_=>Target{token:random(),created:Instant::now(),session:session.session.clone(),document_id:session.document_id,window_id:random(),window:session.window,web:session.web,focus:session.focus,anchor,tools:None}};
 target.anchor=anchor;self.session(services,authorized,Some(&mut target),&[],false,false)?;
 let hint=DocsHint{token:target.token.clone(),app_id:target.session.foreground.app_id.clone(),pid:target.session.foreground.window_pid,document_id:target.document_id.clone(),window_id:target.window_id.clone(),anchor,support:"unknown",help_url:HELP_URL};self.target=Some(target);Ok(hint)})();
 match result{Ok(hint)=>{self.diagnostic="docs_preference_unknown";Some(hint)},Err(reason)=>{self.diagnostic=reason;self.reset();None}}
 }
 fn named(&self,root:&AxElement,roles:&[&str],label:&str,description:&str,hidden:bool,disabled:bool)->Check<AxElement>{let mut found=Vec::new();for node in nodes(root)?{let role=node.string("AXRole").unwrap_or_default();if !roles.contains(&role.as_str())||(role=="AXGroup"&&node.string("AXSubrole").as_deref()!=Some("AXApplicationDialog")){continue}let names=[node.string("AXTitle"),node.string("AXDescription")];if names.iter().any(|s|s.as_deref()==Some(label)){if names.iter().flatten().any(|s|!s.is_empty()&&s!=label){return Err("docs_control_name_ambiguous")}found.push(node)}}if found.len()>1{return Err("docs_controls_ambiguous")}let node=found.pop().ok_or("docs_control_unverified")?;safe(&node,description,hidden,disabled)?;Ok(node)}
 fn wait_named(&self,services:&mut CaptureServices<'_>,authorized:&dyn Fn()->bool,target:&mut Target,root:&AxElement,roles:&[&str],label:&str,scopes:&[AxElement])->Check<AxElement>{let deadline=Instant::now()+UI_TIMEOUT;loop{let result=(||{let control=self.named(root,roles,label,services.secure_description(),true,false)?;let mut owned_scopes=scopes.to_vec();owned_scopes.push(control.clone());if roles==["AXMenuItem"]{let menus:Vec<_>=ancestry(&control,services.secure_description(),true)?.into_iter().filter(|n|n.string("AXRole").as_deref()==Some("AXMenu")).collect();if menus.len()!=1{return Err("docs_menu_unverified")}owned_scopes=scopes.iter().cloned().chain(menus).collect();}
 self.session(services,authorized,Some(target),&owned_scopes,false,true)?;let chain=ancestry(&control,services.secure_description(),true)?;for node in &chain{owned(node,target.session.foreground.window_pid)?}if !chain.contains(&target.web){return Err("docs_control_outside_document")}ancestry(&control,services.secure_description(),false)?;self.session(services,authorized,Some(target),&owned_scopes,false,false)?;Ok(control)})();match result{Ok(v)=>return Ok(v),Err(reason)if (reason=="native_control_hidden"||reason=="docs_control_unverified")&&Instant::now()<deadline=>{self.session(services,authorized,Some(target),scopes,false,true)?;std::thread::sleep(Duration::from_millis(30));},Err(e)=>return Err(e)}}}
 fn checked(&self,services:&mut CaptureServices<'_>,authorized:&dyn Fn()->bool,target:&mut Target,dialog:&AxElement,checkbox:&AxElement)->Check<bool>{self.session(services,authorized,Some(target),std::slice::from_ref(dialog),false,false)?;owned(checkbox,target.session.foreground.window_pid)?;safe(checkbox,services.secure_description(),false,false)?;if boolean_attribute(checkbox,"AXEnabled")!=Some(true){return Err("native_control_unavailable")}if !ancestry(checkbox,services.secure_description(),false)?.contains(dialog){return Err("docs_checkbox_detached")}checked_value(checkbox)}
 fn press(&self,services:&mut CaptureServices<'_>,authorized:&dyn Fn()->bool,target:&mut Target,control:&AxElement,scopes:&[AxElement],unchecked:bool)->Check<()>{self.session(services,authorized,Some(target),scopes,false,false)?;owned(control,target.session.foreground.window_pid)?;if !ancestry(control,services.secure_description(),false)?.contains(&target.web){return Err("docs_control_outside_document")}if !control.actions().map_err(|_|"docs_native_action_unavailable")?.iter().any(|s|s=="AXPress"){return Err("docs_native_action_unavailable")}
 self.session(services,authorized,Some(target),scopes,false,false)?;safe(control,services.secure_description(),false,false)?;owned(control,target.session.foreground.window_pid)?;if !ancestry(control,services.secure_description(),false)?.contains(&target.web){return Err("docs_control_outside_document")}if unchecked&&self.checked(services,authorized,target,&scopes[0],control)?{return Err("docs_checkbox_changed")}control.perform_action("AXPress").map_err(|_|"docs_native_action_failed")}
 fn open_settings(&self,services:&mut CaptureServices<'_>,authorized:&dyn Fn()->bool,target:&mut Target)->Check<(AxElement,AxElement,AxElement,AxElement)>{let deadline=Instant::now()+UI_TIMEOUT;let (portuguese,tools)=loop{self.session(services,authorized,Some(target),&[],false,false)?;let mut candidates=Vec::new();for label in ["Tools","Ferramentas"]{match self.named(&target.web,&["AXMenuBarItem","AXMenuItem"],label,services.secure_description(),false,false){Ok(node)=>candidates.push((label=="Ferramentas",node)),Err("docs_control_unverified")=>{},Err(reason)=>return Err(reason)}}if candidates.len()==1{break candidates.pop().unwrap()}if candidates.len()>1||Instant::now()>=deadline{return Err("docs_control_unverified")}std::thread::sleep(Duration::from_millis(30));};
 if portuguese&&tools.string("AXDOMIdentifier").as_deref()!=Some("docs-tools-menu"){return Err("docs_control_unverified")}
 self.press(services,authorized,target,&tools,&[],false)?;let web=target.web.clone();let item=self.wait_named(services,authorized,target,&web,&["AXMenuItem"],if portuguese{"Acessibilidade c"}else{"Accessibility settings"},std::slice::from_ref(&tools))?;
 let menus:Vec<_>=ancestry(&item,services.secure_description(),false)?.into_iter().filter(|n|n.string("AXRole").as_deref()==Some("AXMenu")).collect();if menus.len()!=1||!ancestry(&menus[0],services.secure_description(),false)?.contains(&target.web){return Err("docs_menu_unverified")}let menu=menus[0].clone();self.press(services,authorized,target,&item,&[tools.clone(),menu.clone()],false)?;
 let dialog=self.wait_named(services,authorized,target,&web,&["AXDialog","AXGroup"],if portuguese{"Configurações de acessibilidade"}else{"Accessibility settings"},&[menu,tools.clone()])?;
 let session=self.session(services,authorized,Some(target),&[dialog.clone(),tools.clone()],false,false)?;target.tools=Some(tools.clone());if session.focus==tools{target.focus=tools;}
 self.session(services,authorized,Some(target),std::slice::from_ref(&dialog),false,false)?;
 let checkbox=self.named(&dialog,&["AXCheckBox"],if portuguese{"Ativar a compatibilidade com o leitor de tela"}else{"Turn on screen reader support"},services.secure_description(),false,false)?;
 let braille=self.named(&dialog,&["AXCheckBox"],if portuguese{"Ativar suporte a braille"}else{"Turn on braille support"},services.secure_description(),false,true)?;
 let default=self.named(&dialog,&["AXButton"],"OK",services.secure_description(),false,false)?;if dialog.element("AXDefaultButton").is_some_and(|v|v!=default){return Err("docs_commit_control_unverified")}Ok((dialog,checkbox,braille,default))
 }
 fn enable_checkbox(&self,services:&mut CaptureServices<'_>,authorized:&dyn Fn()->bool,target:&mut Target,dialog:&AxElement,checkbox:&AxElement)->Check<bool>{if self.checked(services,authorized,target,dialog,checkbox)?{return Ok(false)}self.press(services,authorized,target,checkbox,std::slice::from_ref(dialog),true)?;let deadline=Instant::now()+UI_TIMEOUT;while !self.checked(services,authorized,target,dialog,checkbox)?{if Instant::now()>=deadline{return Err("docs_checkbox_readback_failed")}std::thread::sleep(Duration::from_millis(30));}Ok(true)}
 pub fn enable(&mut self,services:&mut CaptureServices<'_>,token:&str,authorized:&dyn Fn()->bool)->DocsOutcome{
 let target=self.target.take();let result=(||->Check<&'static str>{let mut target=target.ok_or("docs_token_invalid")?;if !token_valid(&target.token,token,target.created.elapsed()){return Err("docs_token_invalid")}
 self.session(services,authorized,Some(&mut target),&[],false,false)?;if target.session.foreground.app_id.eq_ignore_ascii_case("at.studio.asidebrowser"){return Err("docs_native_actions_unverified")}
 let (dialog,reader,braille,default)=self.open_settings(services,authorized,&mut target)?;let changed_reader=self.enable_checkbox(services,authorized,&mut target,&dialog,&reader)?;
 if changed_reader{let deadline=Instant::now()+UI_TIMEOUT;loop{self.session(services,authorized,Some(&mut target),std::slice::from_ref(&dialog),false,false)?;owned(&braille,target.session.foreground.window_pid)?;safe(&braille,services.secure_description(),false,true)?;let parent=braille.element("AXParent").ok_or("docs_checkbox_detached")?;if !ancestry(&parent,services.secure_description(),false)?.contains(&dialog){return Err("docs_checkbox_detached")}match boolean_attribute(&braille,"AXEnabled"){Some(true)=>break,Some(false)=>{},None=>return Err("native_control_unavailable")}if Instant::now()>=deadline{return Err("native_control_unavailable")}std::thread::sleep(Duration::from_millis(30));}}
 let changed_braille=self.enable_checkbox(services,authorized,&mut target,&dialog,&braille)?;self.press(services,authorized,&mut target,&default,std::slice::from_ref(&dialog),false)?;
 let deadline=Instant::now()+UI_TIMEOUT;loop{match self.session(services,authorized,Some(&mut target),&[],true,false){Ok(_)=>break,Err("docs_focus_changed")if Instant::now()<deadline=>std::thread::sleep(Duration::from_millis(30)),Err(reason)=>return Err(reason)}}
 let (dialog,reader,braille,default)=self.open_settings(services,authorized,&mut target)?;if !self.checked(services,authorized,&mut target,&dialog,&reader)?||!self.checked(services,authorized,&mut target,&dialog,&braille)?{return Err("docs_committed_readback_failed")}self.press(services,authorized,&mut target,&default,std::slice::from_ref(&dialog),false)?;
 Ok(if changed_reader||changed_braille{"enabled"}else{"already_enabled"})})();
 match result{Ok(status)=>{self.diagnostic="docs_checkbox_verified_checked";DocsOutcome{status,diagnostic:self.diagnostic,help_url:HELP_URL}},Err(reason)=>{self.diagnostic=reason;DocsOutcome{status:"unknown",diagnostic:reason,help_url:HELP_URL}}}
 }
}
#[cfg(test)]mod tests{use super::*;
 #[test]fn url_identity_is_exact_and_not_canonicalized(){for s in ["http://docs.google.com/document/d/id/edit","https://docs.google.com.evil/document/d/id/edit","https://user@docs.google.com/document/d/id/edit","https://docs.google.com:444/document/d/id/edit","https://docs.google.com/document/d/id%2Fspoof/edit","https://docs.google.com/document/d/id/edit/extra","https://docs.google.com/document/d//edit"]{assert!(document_url(s).is_err())}assert_eq!(document_url("https://docs.google.com/document/u/1/d/abc-_12/edit?x=1"),Ok("abc-_12"));}
 #[test]fn capability_tokens_expire_and_compare_exactly(){assert!(token_valid("abc","abc",Duration::from_secs(29)));assert!(!token_valid("abc","abc",TOKEN_SECONDS));assert!(!token_valid("abc","abcx",Duration::ZERO));assert!(!token_valid("abc","abd",Duration::ZERO));}
 #[test]fn string_and_float_checkbox_states_are_never_coerced(){assert_eq!(native_bool(&AxValue::string("1").unwrap()),None);unsafe{unsafe extern "C"{fn CFNumberCreate(allocator:Raw,kind:isize,value:*const std::ffi::c_void)->Raw;}let number=1.0f64;let number=AxValue::from_owned(CFNumberCreate(std::ptr::null(),13,(&number as *const f64).cast())).unwrap();assert_eq!(native_bool(&number),None);}}
}
