//! Consent-gated ScreenCaptureKit/Vision. Pixels never reach disk or logs.
use anyhow::{Result,anyhow};
use crate::{config::Config,core::{Snapshot,InlineRender,Rect,Authority},platform::ContextRead};
use super::{CaptureServices,AppearanceIntegration,ax::{AxValue,Raw,NativeRect,Point,Size},geometry::{AppearanceEvidence,AppearanceSample}};
use super::ax::ffi::{CGPreflightScreenCaptureAccess,CGRequestScreenCaptureAccess};
use objc2::{msg_send,rc::Retained,runtime::{AnyClass,AnyObject}};
use objc2_foundation::{NSString,NSRect};
use block2::RcBlock;
use sha2::{Sha256,Digest};
use std::{ffi::c_void,sync::{Arc,Mutex,LazyLock,atomic::{AtomicBool,Ordering}},time::{Duration,Instant},collections::VecDeque};
use unicode_general_category::{get_general_category,GeneralCategory as G};
const TIMEOUT:Duration=Duration::from_secs(2);
const TEXT_TTL:Duration=Duration::from_secs(4);
const COLOUR_TTL:Duration=Duration::from_secs(15);
const CAPTURE_INTERVAL:Duration=Duration::from_millis(750);
#[link(name="ScreenCaptureKit",kind="framework")]unsafe extern "C"{}
#[link(name="Vision",kind="framework")]unsafe extern "C"{}
#[link(name="AppKit",kind="framework")]unsafe extern "C"{static NSFontFixedAdvanceAttribute:Raw;}
unsafe extern "C"{fn pthread_main_np()->i32;}
#[link(name="Carbon",kind="framework")]unsafe extern "C"{fn IsSecureEventInputEnabled()->bool;}
#[link(name="ApplicationServices",kind="framework")]unsafe extern "C"{
 fn CGWindowListCopyWindowInfo(options:u32,relative:u32)->Raw;
 fn CGImageGetWidth(image:Raw)->usize;fn CGImageGetHeight(image:Raw)->usize;
 fn CGImageCreateWithImageInRect(image:Raw,rect:NativeRect)->Raw;
 fn CGColorSpaceCreateDeviceGray()->Raw;
 fn CGColorSpaceCreateWithName(name:Raw)->Raw;static kCGColorSpaceSRGB:Raw;
 fn CGBitmapContextCreate(data:*mut c_void,width:usize,height:usize,bits:usize,stride:usize,space:Raw,info:u32)->Raw;
 fn CGContextDrawImage(context:Raw,rect:NativeRect,image:Raw);
 fn CGContextScaleCTM(context:Raw,x:f64,y:f64);fn CGContextSetGrayFillColor(context:Raw,gray:f64,alpha:f64);
 fn CGContextFillRect(context:Raw,rect:NativeRect);fn CGContextSetShouldAntialias(context:Raw,value:bool);
 fn CGContextSetShouldSmoothFonts(context:Raw,value:bool);fn CGContextSetAllowsFontSubpixelPositioning(context:Raw,value:bool);
 fn CGContextSetShouldSubpixelPositionFonts(context:Raw,value:bool);fn CGContextSetTextPosition(context:Raw,x:f64,y:f64);
}
#[link(name="CoreFoundation",kind="framework")]unsafe extern "C"{
 fn CFArrayGetCount(array:Raw)->isize;fn CFArrayGetValueAtIndex(array:Raw,index:isize)->Raw;
 fn CFDictionaryCreate(allocator:Raw,keys:*const Raw,values:*const Raw,count:isize,key_callbacks:Raw,value_callbacks:Raw)->Raw;
 fn CFAttributedStringCreate(allocator:Raw,text:Raw,attrs:Raw)->Raw;
}
#[link(name="CoreText",kind="framework")]unsafe extern "C"{
 static kCTFontAttributeName:Raw;static kCTForegroundColorFromContextAttributeName:Raw;
 static kCTFontFamilyNameAttribute:Raw;static kCTFontNameAttribute:Raw;
 fn CTFontDescriptorCreateWithAttributes(attrs:Raw)->Raw;fn CTFontDescriptorCreateMatchingFontDescriptors(descriptor:Raw,mandatory:Raw)->Raw;
 fn CTFontDescriptorCopyAttribute(descriptor:Raw,attribute:Raw)->Raw;
 fn CTLineCreateWithAttributedString(value:Raw)->Raw;fn CTLineGetTypographicBounds(line:Raw,ascent:*mut f64,descent:*mut f64,leading:*mut f64)->f64;
 fn CTLineDraw(line:Raw,context:Raw);fn CTFontGetSymbolicTraits(font:Raw)->u32;fn CTFontCopyTraits(font:Raw)->Raw;
}
fn native_rect(r:Rect)->NativeRect{NativeRect{origin:Point{x:r[0],y:r[1]},size:Size{width:r[2],height:r[3]}}}
fn rect(r:NSRect)->Rect{[r.origin.x,r.origin.y,r.size.width,r.size.height]}
fn valid(r:Rect)->bool{r.iter().all(|v|v.is_finite())&&r[2]>=0.0&&r[3]>0.0}
fn close(a:Rect,b:Rect,tolerance:f64)->bool{a.iter().zip(b).all(|(x,y)|(x-y).abs()<=tolerance)}
fn intersects(a:Rect,b:Rect)->bool{(a[0]+a[2]).min(b[0]+b[2])>a[0].max(b[0])&&(a[1]+a[3]).min(b[1]+b[3])>a[1].max(b[1])}
fn contains(a:Rect,b:Rect)->bool{b[0]>=a[0]-1.0&&b[1]>=a[1]-1.0&&b[0]+b[2]<=a[0]+a[2]+1.0&&b[1]+b[3]<=a[1]+a[3]+1.0}
fn gap(a:Rect,b:Rect)->f64{(0.0f64.max(b[0]-a[0]-a[2]).max(a[0]-b[0]-b[2])).hypot(0.0f64.max(b[1]-a[1]-a[3]).max(a[1]-b[1]-b[3]))}
pub fn capture_granted()->bool{unsafe{CGPreflightScreenCaptureAccess()}}
pub fn request_capture_permission()->Result<bool>{if unsafe{pthread_main_np()}==0{return Err(anyhow!("capture-permission-main-thread-required"))}Ok(capture_granted()||unsafe{CGRequestScreenCaptureAccess()})}
fn secure()->bool{unsafe{IsSecureEventInputEnabled()}}
fn frontmost()->i32{let Some(system)=super::ax::AxElement::system_wide()else{return 0};if system.set_timeout(0.25).is_err(){return 0}system.element("AXFocusedApplication").and_then(|app|app.pid()).unwrap_or(0)}
#[derive(Clone)]struct WindowRecord{pid:i32,layer:f64,frame:Rect,sharing:f64,alpha:f64,number:f64}
fn select_window(items:impl Iterator<Item=WindowRecord>,pid:i32,bounds:Rect)->Option<u32>{
 let mut result=None;
 for item in items{if item.pid!=pid||item.layer!=0.0||!close(item.frame,bounds,1.0){continue}
 if item.sharing==0.0{return None}if !item.alpha.is_finite()||item.alpha<=0.0{continue}
 if item.number<=0.0||!item.number.is_finite()||item.number.fract()!=0.0||item.number>u32::MAX as f64||result.is_some(){return None}result=Some(item.number as u32);}
 result
}
pub fn window_number(pid:i32,bounds:Rect)->Option<u32>{
 if pid<=0||!valid(bounds)||bounds[2]<=0.0{return None}
 let list=unsafe{AxValue::from_owned(CGWindowListCopyWindowInfo(1|16,0))}?;let count=unsafe{CFArrayGetCount(list.as_raw())};if !(0..=16384).contains(&count){return None}
 let records=(0..count).filter_map(|i|{
 let item=unsafe{AxValue::from_borrowed(CFArrayGetValueAtIndex(list.as_raw(),i))}?;let number=|key:&str|item.dictionary_value(key)?.as_number();
 let owner=number("kCGWindowOwnerPID")?;if owner!=pid as f64{return None}
 let b=item.dictionary_value("kCGWindowBounds")?;let frame=[b.dictionary_value("X")?.as_number()?,b.dictionary_value("Y")?.as_number()?,b.dictionary_value("Width")?.as_number()?,b.dictionary_value("Height")?.as_number()?];
 Some(WindowRecord{pid,layer:number("kCGWindowLayer").unwrap_or(-1.0),frame,sharing:number("kCGWindowSharingState").unwrap_or(1.0),alpha:number("kCGWindowAlpha").unwrap_or(1.0),number:number("kCGWindowNumber").unwrap_or(0.0)})
 });
 select_window(records,pid,bounds)
}
// Callback payloads are immutable native SC content/CGImage references. Only
// this handoff wrapper crosses threads; all inspection remains owner-thread.
enum Transfer{Object(usize),Image(usize)}
unsafe impl Send for Transfer{}
impl Drop for Transfer{fn drop(&mut self){unsafe{match *self{Self::Object(raw)=>{drop(Retained::<AnyObject>::from_raw(raw as *mut AnyObject));},Self::Image(raw)=>super::ax::CFRelease(raw as Raw)}}}}
struct Reply{accepting:AtomicBool,value:Mutex<Option<Transfer>>}
impl Reply{fn new()->Arc<Self>{let reply=Arc::new(Self{accepting:AtomicBool::new(true),value:Mutex::new(None)});let expiry=Arc::downgrade(&reply);std::thread::spawn(move||{std::thread::sleep(TIMEOUT);if let Some(reply)=expiry.upgrade(){reply.cancel()}});reply}
 fn put(&self,value:Transfer){let mut slot=self.value.lock().unwrap_or_else(|e|e.into_inner());if self.accepting.load(Ordering::Acquire){*slot=Some(value)}}
 fn take(&self)->Option<Transfer>{self.value.lock().unwrap_or_else(|e|e.into_inner()).take()}
 fn cancel(&self){let mut slot=self.value.lock().unwrap_or_else(|e|e.into_inner());self.accepting.store(false,Ordering::Release);*slot=None;}}
type ContentBlock=RcBlock<dyn Fn(*mut AnyObject,*mut AnyObject)>;
type ImageBlock=RcBlock<dyn Fn(Raw,*mut AnyObject)>;
enum Stage{Content{reply:Arc<Reply>,_callback:ContentBlock},Image{reply:Arc<Reply>,_callback:ImageBlock,_filter:Retained<AnyObject>,_configuration:Retained<AnyObject>},Done}
struct Capture{stage:Stage,deadline:Instant,pid:i32,bounds:Rect,number:u32,scale:f64,width:usize,height:usize}
struct Shot{image:AxValue,frame:Rect,number:u32,scale:f64}
impl Capture{
 fn start(pid:i32,bounds:Rect)->Result<Option<Self>>{
 let Some(number)=window_number(pid,bounds)else{return Ok(None)};let Some(class)=AnyClass::get(c"SCShareableContent")else{return Err(anyhow!("capture-framework-unavailable"))};
 let reply=Reply::new();let callback_reply=reply.clone();let callback=RcBlock::new(move|content:*mut AnyObject,error:*mut AnyObject|{if !error.is_null()||content.is_null(){return}let retained=unsafe{Retained::retain(content)};if let Some(retained)=retained{callback_reply.put(Transfer::Object(Retained::into_raw(retained) as usize));}});
 unsafe{let _:()=msg_send![class,getShareableContentExcludingDesktopWindows:true,onScreenWindowsOnly:true,completionHandler:&*callback];}
 Ok(Some(Self{stage:Stage::Content{reply,_callback:callback},deadline:Instant::now()+TIMEOUT,pid,bounds,number,scale:1.0,width:0,height:0}))
 }
 fn poll(&mut self)->Result<Option<Shot>>{
 if Instant::now()>=self.deadline{self.cancel();return Err(anyhow!("capture-timeout"))}
 match &self.stage{
 Stage::Content{reply,..}=>{let Some(value)=reply.take()else{return Ok(None)};
 let raw=match &value{Transfer::Object(raw)=>*raw,_=>return Err(anyhow!("capture-result-invalid"))};
 let content=unsafe{Retained::<AnyObject>::retain(raw as *mut AnyObject)}.ok_or_else(||anyhow!("capture-content-unavailable"))?;drop(value);
 unsafe{let windows:*mut AnyObject=msg_send![&*content,windows];let count:usize=msg_send![windows,count];if count>16384{return Err(anyhow!("capture-content-unbounded"))}
 let mut found=None;for i in 0..count{let window:*mut AnyObject=msg_send![windows,objectAtIndex:i];let id:u32=msg_send![window,windowID];if id==self.number{if found.is_some(){return Err(anyhow!("capture-window-ambiguous"))}found=Retained::retain(window);}}
 let window=found.ok_or_else(||anyhow!("capture-window-unavailable"))?;let owner:*mut AnyObject=msg_send![&*window,owningApplication];if owner.is_null(){return Err(anyhow!("capture-owner-unavailable"))}let pid:i32=msg_send![owner,processID];let on_screen:bool=msg_send![&*window,isOnScreen];let frame:NSRect=msg_send![&*window,frame];if pid!=self.pid||!on_screen||!close(rect(frame),self.bounds,1.0){return Err(anyhow!("capture-window-changed"))}
 let filter_class=AnyClass::get(c"SCContentFilter").ok_or_else(||anyhow!("capture-framework-unavailable"))?;let allocated:*mut AnyObject=msg_send![filter_class,alloc];let raw:*mut AnyObject=msg_send![allocated,initWithDesktopIndependentWindow:&*window];let filter=Retained::from_raw(raw).ok_or_else(||anyhow!("capture-filter-unavailable"))?;
 let scale:f32=msg_send![&*filter,pointPixelScale];self.scale=scale as f64;if !self.scale.is_finite()||!(0.5..=4.0).contains(&self.scale){return Err(anyhow!("capture-scale-unverified"))}
 self.width=(self.bounds[2]*self.scale).round() as usize;self.height=(self.bounds[3]*self.scale).round() as usize;if self.width==0||self.height==0||self.width.checked_mul(self.height).is_none_or(|n|n>100_000_000){return Err(anyhow!("capture-size-unbounded"))}
 let class=AnyClass::get(c"SCStreamConfiguration").ok_or_else(||anyhow!("capture-framework-unavailable"))?;let configuration=Retained::<AnyObject>::from_raw(msg_send![class,new]).ok_or_else(||anyhow!("capture-config-unavailable"))?;
 let responds:bool=msg_send![&*configuration,respondsToSelector:objc2::sel!(setIgnoreShadowsSingleWindow:)];if !responds{return Err(anyhow!("capture-shadow-mapping-unavailable"))}
 let _:()=msg_send![&*configuration,setIgnoreShadowsSingleWindow:true];let _:()=msg_send![&*configuration,setWidth:self.width];let _:()=msg_send![&*configuration,setHeight:self.height];let _:()=msg_send![&*configuration,setShowsCursor:false];
 let reply=Reply::new();let callback_reply=reply.clone();let callback=RcBlock::new(move|image:Raw,error:*mut AnyObject|{if error.is_null()&&!image.is_null(){super::ax::CFRetain(image);callback_reply.put(Transfer::Image(image as usize));}});
 let screenshot=AnyClass::get(c"SCScreenshotManager").ok_or_else(||anyhow!("capture-framework-unavailable"))?;let _:()=msg_send![screenshot,captureImageWithFilter:&*filter,configuration:&*configuration,completionHandler:&*callback];
 self.stage=Stage::Image{reply,_callback:callback,_filter:filter,_configuration:configuration};}
 Ok(None)
 },
 Stage::Image{reply,..}=>{let Some(value)=reply.take()else{return Ok(None)};let raw=match &value{Transfer::Image(raw)=>*raw,_=>return Err(anyhow!("capture-result-invalid"))};let image=unsafe{AxValue::from_borrowed(raw as Raw)}.ok_or_else(||anyhow!("capture-image-unavailable"))?;drop(value);
 if unsafe{CGImageGetWidth(image.as_raw())}.abs_diff(self.width)>1||unsafe{CGImageGetHeight(image.as_raw())}.abs_diff(self.height)>1{return Err(anyhow!("capture-pixel-mapping-unverified"))}self.stage=Stage::Done;Ok(Some(Shot{image,frame:self.bounds,number:self.number,scale:self.scale}))},Stage::Done=>Err(anyhow!("capture-consumed"))}
 }
 fn cancel(&mut self){match &self.stage{Stage::Content{reply,..}|Stage::Image{reply,..}=>reply.cancel(),Stage::Done=>{}}self.stage=Stage::Done;}
}
impl Drop for Capture{fn drop(&mut self){self.cancel()}}
#[derive(Clone)]struct Pixels{width:usize,height:usize,rgba:Vec<u8>,origin:[f64;2],scale:f64}
impl Shot{fn crop(&self,area:Rect)->Option<Pixels>{let [fx,fy,_,_]=self.frame;let scale=self.scale;let width=unsafe{CGImageGetWidth(self.image.as_raw())};let height=unsafe{CGImageGetHeight(self.image.as_raw())};
 let x0=((area[0]-fx)*scale).floor().max(0.0)as usize;let y0=((area[1]-fy)*scale).floor().max(0.0)as usize;let x1=(((area[0]+area[2]-fx)*scale).ceil().max(0.0)as usize).min(width);let y1=(((area[1]+area[3]-fy)*scale).ceil().max(0.0)as usize).min(height);if x1.saturating_sub(x0)<4||y1.saturating_sub(y0)<4{return None}let w=x1-x0;let h=y1-y0;
 let part=unsafe{AxValue::from_owned(CGImageCreateWithImageInRect(self.image.as_raw(),native_rect([x0 as f64,y0 as f64,w as f64,h as f64])))}?;
 let space=unsafe{AxValue::from_owned(CGColorSpaceCreateWithName(kCGColorSpaceSRGB))}?;let mut bytes=vec![0u8;w.checked_mul(h)?.checked_mul(4)?];let context=unsafe{AxValue::from_owned(CGBitmapContextCreate(bytes.as_mut_ptr().cast(),w,h,8,w*4,space.as_raw(),1| (4<<12)))}?;
 unsafe{CGContextDrawImage(context.as_raw(),native_rect([0.0,0.0,w as f64,h as f64]),part.as_raw())};
 Some(Pixels{width:w,height:h,rgba:bytes,origin:[fx+x0 as f64/scale,fy+y0 as f64/scale],scale})}}
#[derive(Clone,Debug)]struct Observation{text:String,confidence:f64,rect:Rect}
fn recognize(shot:&Shot)->Result<Vec<Observation>>{unsafe{
 let class=AnyClass::get(c"VNImageRequestHandler").ok_or_else(||anyhow!("vision-unavailable"))?;let allocated:*mut AnyObject=msg_send![class,alloc];let dict=AnyClass::get(c"NSDictionary").ok_or_else(||anyhow!("vision-unavailable"))?;let empty:*mut AnyObject=msg_send![dict,dictionary];
 let handler=Retained::<AnyObject>::from_raw(msg_send![allocated,initWithCGImage:shot.image.as_raw(),options:empty]).ok_or_else(||anyhow!("vision-unavailable"))?;
 let class=AnyClass::get(c"VNRecognizeTextRequest").ok_or_else(||anyhow!("vision-unavailable"))?;let request=Retained::<AnyObject>::from_raw(msg_send![class,new]).ok_or_else(||anyhow!("vision-unavailable"))?;
 let _:()=msg_send![&*request,setRecognitionLevel:0isize];let _:()=msg_send![&*request,setUsesLanguageCorrection:true];let responds:bool=msg_send![&*request,respondsToSelector:objc2::sel!(setAutomaticallyDetectsLanguage:)];if responds{let _:()=msg_send![&*request,setAutomaticallyDetectsLanguage:true];}
 let array=AnyClass::get(c"NSArray").ok_or_else(||anyhow!("vision-unavailable"))?;let requests:*mut AnyObject=msg_send![array,arrayWithObject:&*request];let mut error:*mut AnyObject=std::ptr::null_mut();let ok:bool=msg_send![&*handler,performRequests:requests,error:&mut error];if !ok||!error.is_null(){return Err(anyhow!("vision-recognition-failed"))}
 let results:*mut AnyObject=msg_send![&*request,results];if results.is_null(){return Ok(Vec::new())}let count:usize=msg_send![results,count];if count>16384{return Err(anyhow!("vision-results-unbounded"))}let mut observations=Vec::new();
 for i in 0..count{let item:*mut AnyObject=msg_send![results,objectAtIndex:i];let candidates:*mut AnyObject=msg_send![item,topCandidates:1usize];let count:usize=msg_send![candidates,count];if count==0{continue}let candidate:*mut AnyObject=msg_send![candidates,objectAtIndex:0usize];let text:*mut NSString=msg_send![candidate,string];if text.is_null(){continue}let confidence:f32=msg_send![candidate,confidence];let b:NSRect=msg_send![item,boundingBox];let b=rect(b);let r=[shot.frame[0]+b[0]*shot.frame[2],shot.frame[1]+(1.0-b[1]-b[3])*shot.frame[3],b[2]*shot.frame[2],b[3]*shot.frame[3]];if valid(r){observations.push(Observation{text:(&*text).to_string().trim().into(),confidence:confidence as f64,rect:r})}}
 Ok(observations)
}}
fn compose(observations:Vec<Observation>,field:Option<Rect>,limit:usize)->String{
 let mut kept:Vec<_>=observations.into_iter().filter(|o|o.confidence.is_finite()&&o.confidence>=0.4&&!o.text.replace(' ',"").is_empty()&&!o.text.chars().filter(|c|*c!=' ').all(|c|"•●*·∙◦".contains(c))&&!field.is_some_and(|f|intersects(o.rect,f))).collect();
 if let Some(field)=field{kept.sort_by(|a,b|gap(a.rect,field).total_cmp(&gap(b.rect,field)))}let mut chosen=Vec::new();let mut used=0;for item in kept{let count=item.text.chars().count()+1;if used+count<=limit{used+=count;chosen.push(item)}}chosen.sort_by(|a,b|(a.rect[1]+a.rect[3]/2.0).total_cmp(&(b.rect[1]+b.rect[3]/2.0)));
 let mut rows:Vec<Vec<Observation>>=Vec::new();for item in chosen{if rows.last().and_then(|r|r.first()).is_none_or(|first|(item.rect[1]+item.rect[3]/2.0-first.rect[1]-first.rect[3]/2.0).abs()>item.rect[3].max(first.rect[3])/2.0){rows.push(Vec::new())}rows.last_mut().unwrap().push(item)}
 rows.into_iter().map(|mut row|{row.sort_by(|a,b|a.rect[0].total_cmp(&b.rect[0]));row.into_iter().map(|o|o.text).collect::<Vec<_>>().join(" ")}).collect::<Vec<_>>().join("\n").chars().take(limit).collect()
}
#[derive(Clone,PartialEq)]struct TextKey{authority:Authority,window:Rect,field:Option<Rect>}
struct TextCache{epoch:u64,value:Option<(TextKey,Instant,String)>}
static TEXT:LazyLock<Mutex<TextCache>>=LazyLock::new(||Mutex::new(TextCache{epoch:0,value:None}));
fn text_cache()->&'static Mutex<TextCache>{&TEXT}
pub fn clear_text_cache(){let mut cache=text_cache().lock().unwrap_or_else(|e|e.into_inner());cache.epoch=cache.epoch.wrapping_add(1);cache.value=None;}
fn retain_text(key:TextKey,text:String,epoch:u64)->bool{
 let mut cache=text_cache().lock().unwrap_or_else(|e|e.into_inner());if cache.epoch!=epoch{return false}cache.epoch=cache.epoch.wrapping_add(1);let generation=cache.epoch;cache.value=Some((key,Instant::now(),text));
 // Timer retains generation only, never plaintext. Expiration still occurs
 // when the owner stops polling and older timers cannot erase newer results.
 std::thread::spawn(move||{std::thread::sleep(TEXT_TTL);let mut cache=text_cache().lock().unwrap_or_else(|e|e.into_inner());if cache.epoch==generation{cache.value=None;cache.epoch=cache.epoch.wrapping_add(1)}});true
}
fn context(status:&str,text:String)->ContextRead{ContextRead{status:status.into(),text,source:"screenshot".into()}}
pub fn recognize_window_text(snapshot:&Snapshot,authorized:&dyn Fn()->bool,current:&mut dyn FnMut()->Result<bool>,max_chars:usize)->Result<ContextRead>{
 if [super::terminal::TERMINAL,super::terminal::ITERM].contains(&snapshot.app_id.as_str())&&snapshot.field_id.starts_with("terminal:"){clear_text_cache();return Ok(context("blocked",String::new()))}
 if snapshot.protected||!authorized(){clear_text_cache();return Ok(context("blocked",String::new()))}let Some(window)=snapshot.window_bounds.filter(|r|valid(*r)&&r[2]>0.0)else{return Ok(context("unavailable",String::new()))};let pid=snapshot.window_pid;if pid<=0{return Ok(context("unavailable",String::new()))}
 if !capture_granted(){clear_text_cache();return Ok(context("screen_permission",String::new()))}if secure(){clear_text_cache();return Ok(context("blocked",String::new()))}
 let limit=max_chars.min(4000);let key=TextKey{authority:snapshot.authority(),window,field:snapshot.field_rect.filter(|r|valid(*r))};
 let (epoch,cached)={let mut cache=text_cache().lock().unwrap_or_else(|e|e.into_inner());if cache.value.as_ref().is_some_and(|(_,time,_)|time.elapsed()>=TEXT_TTL){cache.value=None;}let cached=cache.value.as_ref().filter(|(k,_,_)|k==&key).map(|(_,_,text)|text.chars().take(limit).collect::<String>());(cache.epoch,cached)};
 if let Some(text)=cached{if !authorized()||secure()||!capture_granted(){clear_text_cache();return Ok(context("blocked",String::new()))}if !current()?||frontmost()!=pid{return Ok(context("stale",String::new()))}return Ok(context(if text.is_empty(){"empty"}else{"ready"},text))}
 // Never carry another document's plaintext through screenshot/OCR.
 {text_cache().lock().unwrap_or_else(|e|e.into_inner()).value=None;}
 let mut valid_now=||->Result<bool>{Ok(authorized()&&capture_granted()&&!secure()&&frontmost()==pid&&current()?&&text_cache().lock().unwrap_or_else(|e|e.into_inner()).epoch==epoch)};
 if !valid_now()?{return Ok(context("stale",String::new()))}let Some(mut capture)=Capture::start(pid,window)?else{return Ok(context("unavailable",String::new()))};
 let shot=loop{if !valid_now()?{capture.cancel();return Ok(context(if authorized(){"stale"}else{"blocked"},String::new()))}match capture.poll(){Ok(Some(shot))=>break shot,Ok(None)=>std::thread::sleep(Duration::from_millis(5)),Err(_)=>return Ok(context("unavailable",String::new()))}};
 if !valid_now()?||window_number(pid,window)!=Some(shot.number){return Ok(context("stale",String::new()))}
 let observations=recognize(&shot);drop(shot);let observations=match observations{Ok(v)=>v,Err(_)=>return Ok(context("error",String::new()))};
 if !valid_now()?{return Ok(context(if authorized(){"stale"}else{"blocked"},String::new()))}let text=compose(observations,key.field,4000);let bounded=text.chars().take(limit).collect::<String>();if !retain_text(key,text,epoch){return Ok(context("stale",String::new()))}Ok(context(if bounded.is_empty(){"empty"}else{"ready"},bounded))
}

// Style work is polled on the native owner. Native screenshot callbacks are
// asynchronous; requesting a style never waits for a capture to complete.
#[derive(Clone,Debug,PartialEq)]enum FontSpec{Named(String),System(f64),Fixed(String,f64),Ax}
#[derive(Clone)]struct Line{samples:Vec<AppearanceSample>,suffix:Option<AppearanceSample>,caret:Rect,clip:Rect,top:f64,height:f64,font_name:String,font_family:String,font_size:f64,fixed_advance:f64,ax:Option<InlineRender>,pid:i32,window:Rect,style_key:String,line_key:String}
fn safe_span(sample:&AppearanceSample)->bool{
 !sample.text.trim().is_empty()&&sample.text.chars().count()<=400&&valid(sample.rect)&&sample.rect[2]>0.0
 &&!sample.text.chars().any(|c|matches!(get_general_category(c),G::Control|G::Format|G::LineSeparator|G::ParagraphSeparator)
 ||matches!(unicode_bidi::bidi_class(c),unicode_bidi::BidiClass::R|unicode_bidi::BidiClass::AL|unicode_bidi::BidiClass::AN|unicode_bidi::BidiClass::RLE|unicode_bidi::BidiClass::RLO|unicode_bidi::BidiClass::RLI))
}
fn parse(snapshot:&Snapshot,e:&AppearanceEvidence)->Option<Line>{
 let window=snapshot.window_bounds?;let caret=e.caret_rect;let clip=e.clip_rect;
 if snapshot.protected||snapshot.selection_length!=0||snapshot.window_pid<=0||snapshot.window_id.is_empty()||snapshot.field_id.is_empty()
 ||!valid(window)||window[2]<=0.0||!valid(caret)||!valid(clip)||clip[2]<=0.0||e.samples.len()>8
 ||e.samples.iter().any(|s|!safe_span(s))||e.suffix.as_ref().is_some_and(|s|!safe_span(s)){return None}
 let ax=e.ax_render.as_ref().map(|r|{let mut r=r.clone();r.suffix.clear();r.suffix_width=0.0;r.text_rgb=None;r.background_rgb=None;r});
 if ax.as_ref().is_some_and(|r|!r.valid(Some(caret))){return None}
 if ax.is_none()&&(e.samples.len()<2||e.samples.iter().all(|s|s.text==e.samples[0].text)){return None}
 let spans:Vec<_>=e.samples.iter().chain(e.suffix.iter()).collect();let first=spans.first()?;let top=first.rect[1];let height=first.rect[3];
 if spans.iter().any(|s|(s.rect[1]-top).abs()>1.0||(s.rect[3]-height).abs()>1.0||!contains(clip,s.rect)||!contains(window,s.rect))
 ||e.samples.windows(2).any(|p|p[1].rect[0]<p[0].rect[0]-0.5)
 ||e.samples.last().is_some_and(|s|(s.rect[0]+s.rect[2]-caret[0]).abs()>1.5)
 ||e.suffix.as_ref().is_some_and(|s|(s.rect[0]-caret[0]).abs()>1.5)
 ||caret[1]+caret[3]/2.0<top-1.0||caret[1]+caret[3]/2.0>top+height+1.0{return None}
 let fixed_advance=e.fixed_advance;
 if !e.font_size.is_finite()||(e.font_size!=0.0&&!(1.0..=300.0).contains(&e.font_size))||!fixed_advance.is_finite()||!(0.0..=300.0).contains(&fixed_advance)||(fixed_advance>0.0&&(e.font_name.is_empty()||e.font_size==0.0)){return None}
 let signature=if let Some(ax)=&ax{format!("ax:{}:{:.2}:{}",ax.font_name,ax.font_size,ax.fixed_advance)}else{format!("evidence:{}:{}:{:.2}:{}",e.font_name,e.font_family,e.font_size,fixed_advance)};
 let style_key=format!("{:?}",(&snapshot.app_id,&snapshot.domain,&snapshot.window_id,&snapshot.field_id,signature));
 let line_key=format!("{}:{:.0}:{:.0}:{}",style_key,top,height,e.suffix.is_some());
 Some(Line{samples:e.samples.clone(),suffix:e.suffix.clone(),caret,clip,top,height,font_name:e.font_name.clone(),font_family:e.font_family.clone(),font_size:e.font_size,fixed_advance,ax,pid:snapshot.window_pid,window,style_key,line_key})
}
fn font(spec:&FontSpec,size:f64,line:&Line)->Option<Retained<AnyObject>>{unsafe{
 let class=AnyClass::get(c"NSFont")?;
 let raw:*mut AnyObject=match spec{
 FontSpec::System(weight)=>msg_send![class,systemFontOfSize:size,weight:*weight],
 FontSpec::Named(name)|FontSpec::Fixed(name,_)=>{let name=NSString::from_str(name);msg_send![class,fontWithName:&*name,size:size]},
 FontSpec::Ax=>{
 let ax=line.ax.as_ref()?;if ax.system_font{msg_send![class,systemFontOfSize:ax.font_size,weight:ax.system_weight]}
 else{let name=NSString::from_str(&ax.font_name);msg_send![class,fontWithName:&*name,size:ax.font_size]}
 }};
 let font=Retained::retain(raw)?;
 if let FontSpec::Named(name)|FontSpec::Fixed(name,_)=spec{let actual:*mut NSString=msg_send![&*font,fontName];if actual.is_null()||(&*actual).to_string()!=*name{return None}}
 let fixed=match spec{FontSpec::Fixed(_,advance)=>*advance,FontSpec::Ax=>line.ax.as_ref()?.fixed_advance,_=>0.0};
 if fixed>0.0{
 let descriptor:*mut AnyObject=msg_send![&*font,fontDescriptor];let key=Retained::<NSString>::retain(NSFontFixedAdvanceAttribute as *mut NSString)?;let number=AnyClass::get(c"NSNumber")?;let value:*mut AnyObject=msg_send![number,numberWithDouble:fixed];
 let dictionary=AnyClass::get(c"NSDictionary")?;let attrs:*mut AnyObject=msg_send![dictionary,dictionaryWithObject:value,forKey:&*key];
 let descriptor:*mut AnyObject=msg_send![descriptor,fontDescriptorByAddingAttributes:attrs];let fixed:*mut AnyObject=msg_send![class,fontWithDescriptor:descriptor,size:size];return Retained::retain(fixed);
 }
 Some(font)
}}
fn attributed(font:&AnyObject,text:&str,context_color:bool)->Option<AxValue>{unsafe{
 let text=AxValue::string(text)?;let keys=[kCTFontAttributeName,kCTForegroundColorFromContextAttributeName];let values=[(font as *const AnyObject).cast(),super::ax::kCFBooleanTrue];
 let attrs=AxValue::from_owned(CFDictionaryCreate(std::ptr::null(),keys.as_ptr(),values.as_ptr(),if context_color{2}else{1},std::ptr::null(),std::ptr::null()))?;
 AxValue::from_owned(CFAttributedStringCreate(std::ptr::null(),text.as_raw(),attrs.as_raw()))
}}
fn width(font:&AnyObject,text:&str)->Option<f64>{let attrs=attributed(font,text,false)?;let line=unsafe{AxValue::from_owned(CTLineCreateWithAttributedString(attrs.as_raw()))}?;let value=unsafe{CTLineGetTypographicBounds(line.as_raw(),std::ptr::null_mut(),std::ptr::null_mut(),std::ptr::null_mut())};value.is_finite().then_some(value)}
fn width_ok(predicted:f64,observed:f64)->bool{(predicted-observed).abs()<=1.0f64.max(0.012*observed)}
fn widths_fit(font:&AnyObject,samples:&[AppearanceSample])->bool{samples.iter().all(|s|width(font,&s.text).is_some_and(|w|width_ok(w,s.rect[2])))}
fn system_specs()->Vec<FontSpec>{[0.0,0.23,0.30,0.40].into_iter().map(FontSpec::System).collect()}
fn family_specs(family:&str)->Vec<FontSpec>{unsafe{
 let Some(name)=AxValue::string(family)else{return Vec::new()};let key=kCTFontFamilyNameAttribute;let value=name.as_raw();let Some(attrs)=AxValue::from_owned(CFDictionaryCreate(std::ptr::null(),&key,&value,1,std::ptr::null(),std::ptr::null()))else{return Vec::new()};
 let Some(descriptor)=AxValue::from_owned(CTFontDescriptorCreateWithAttributes(attrs.as_raw()))else{return Vec::new()};let Some(matches)=AxValue::from_owned(CTFontDescriptorCreateMatchingFontDescriptors(descriptor.as_raw(),std::ptr::null()))else{return Vec::new()};let count=CFArrayGetCount(matches.as_raw());if !(0..=4096).contains(&count){return Vec::new()}
 (0..count.min(24)).filter_map(|i|{let descriptor=CFArrayGetValueAtIndex(matches.as_raw(),i);AxValue::from_owned(CTFontDescriptorCopyAttribute(descriptor,kCTFontNameAttribute))?.as_string().map(FontSpec::Named)}).collect()
}}
fn candidate_specs(line:&Line)->Vec<FontSpec>{
 if line.ax.is_some(){return vec![FontSpec::Ax]}
 if !line.font_name.is_empty(){return if line.fixed_advance>0.0{vec![FontSpec::Fixed(line.font_name.clone(),line.fixed_advance)]}else if line.font_name.starts_with('.') {system_specs()}else{vec![FontSpec::Named(line.font_name.clone())]}}
 let mut specs=Vec::new();
 for raw in line.font_family.split(','){let family=raw.trim().trim_matches(['\'','"']);if family.is_empty(){continue}let lower=family.to_lowercase();if ["-apple-system","system-ui","blinkmacsystemfont",".applesystemuifont","sf pro","sf pro text","sf pro display",".sf ns"].contains(&lower.as_str()){specs.extend(system_specs())}else{specs.extend(family_specs(match lower.as_str(){"sans-serif"=>"Helvetica","serif"=>"Times","monospace"|"ui-monospace"=>"Menlo","ui-serif"=>"New York",_=>family}))}}
 if specs.is_empty(){specs=system_specs();specs.extend(["Helvetica","Helvetica-Bold","HelveticaNeue","HelveticaNeue-Medium","HelveticaNeue-Bold","ArialMT","Arial-BoldMT","TimesNewRomanPSMT","TimesNewRomanPS-BoldMT","Times-Roman","Georgia","Georgia-Bold","Verdana","Verdana-Bold","Tahoma","TrebuchetMS","Menlo-Regular","Monaco","Courier","CourierNewPSMT","Avenir-Book","Avenir-Roman","GillSans","Palatino-Roman"].into_iter().map(|s|FontSpec::Named(s.into())))}
 let mut unique=Vec::new();for spec in specs{if !unique.contains(&spec){unique.push(spec)}if unique.len()>=40{break}}unique
}
fn fit(spec:&FontSpec,line:&Line)->Option<(f64,f64)>{
 let mut size=line.font_size;if size==0.0{size=100.0;for _ in 0..3{let f=font(spec,size,line)?;let mut numerator=0.0;let mut denominator=0.0;for sample in &line.samples{let unit=width(&f,&sample.text)?/size;if unit<=0.0{return None}numerator+=unit*sample.rect[2];denominator+=unit*unit;}size=numerator/denominator;if !(4.0..=200.0).contains(&size){return None}}}
 let f=font(spec,size,line)?;if !widths_fit(&f,&line.samples){return None}let ascent:f64=unsafe{msg_send![&*f,ascender]};let descent:f64=unsafe{msg_send![&*f,descender]};let box_height=ascent+descent.abs();if line.height<0.75*box_height||line.height>3.0*box_height{return None}
 let mut error=0.0;let mut total=0.0;for sample in &line.samples{error+=(width(&f,&sample.text)?-sample.rect[2]).abs();total+=sample.rect[2];}Some((size,error/total))
}
fn candidates(line:&Line)->Vec<(FontSpec,f64)>{
 if let Some(ax)=&line.ax{return vec![(FontSpec::Ax,ax.font_size)]}
 let mut fits:Vec<_>=candidate_specs(line).into_iter().filter_map(|spec|fit(&spec,line).map(|(size,error)|(error,spec,size))).collect();fits.sort_by(|a,b|a.0.total_cmp(&b.0));
 let mut result=Vec::new();for (_,spec,size) in fits.into_iter().take(8){let exact=(size*100.0).round()/100.0;result.push((spec.clone(),exact));let half=(size*2.0).round()/2.0;if half!=exact&&font(&spec,half,line).is_some_and(|f|widths_fit(&f,&line.samples)){result.push((spec,half))}}result
}
fn ncc_values(a:&[f64],b:impl Iterator<Item=f64>)->f64{
 let (mut count,mut sa,mut sb,mut aa,mut bb,mut ab)=(0usize,0.0,0.0,0.0,0.0,0.0);
 for (index,y) in b.enumerate(){let Some(&x)=a.get(index)else{return 0.0};count+=1;sa+=x;sb+=y;aa+=x*x;bb+=y*y;ab+=x*y;}
 if count==0||count!=a.len(){return 0.0}let n=count as f64;let va=aa-sa*sa/n;let vb=bb-sb*sb/n;
 if va<=0.0||vb<=0.0{0.0}else{(ab-sa*sb/n)/(va*vb).sqrt()}
}
#[cfg(test)]fn ncc(a:&[f64],b:&[f64])->f64{ncc_values(a,b.iter().copied())}
fn render_mask(font:&AnyObject,spans:&[AppearanceSample],pixels:&Pixels,baseline:f64)->Option<Vec<u8>>{
 let mut data=vec![0u8;pixels.width.checked_mul(pixels.height)?];let space=unsafe{AxValue::from_owned(CGColorSpaceCreateDeviceGray())}?;let context=unsafe{AxValue::from_owned(CGBitmapContextCreate(data.as_mut_ptr().cast(),pixels.width,pixels.height,8,pixels.width,space.as_raw(),0))}?;
 unsafe{
 CGContextSetGrayFillColor(context.as_raw(),0.0,1.0);CGContextFillRect(context.as_raw(),native_rect([0.0,0.0,pixels.width as f64,pixels.height as f64]));CGContextScaleCTM(context.as_raw(),pixels.scale,pixels.scale);
 CGContextSetShouldAntialias(context.as_raw(),true);CGContextSetShouldSmoothFonts(context.as_raw(),false);CGContextSetAllowsFontSubpixelPositioning(context.as_raw(),true);CGContextSetShouldSubpixelPositionFonts(context.as_raw(),true);CGContextSetGrayFillColor(context.as_raw(),1.0,1.0);
 for sample in spans{let value=attributed(font,&sample.text,true)?;let line=AxValue::from_owned(CTLineCreateWithAttributedString(value.as_raw()))?;CGContextSetTextPosition(context.as_raw(),sample.rect[0]-pixels.origin[0],pixels.height as f64/pixels.scale-baseline);CTLineDraw(line.as_raw(),context.as_raw());}
 }Some(data)
}
fn sample_values<'a>(buffer:&'a[u8],width:usize,rows:(usize,usize),cols:&'a[(usize,usize)],dx:isize)->impl Iterator<Item=f64>+'a{
 (rows.0..rows.1).flat_map(move|y|cols.iter().flat_map(move|&(start,end)|(start..end).map(move|x|{let source=(y*width+x)as isize-dx;if source>=0{buffer.get(source as usize).copied().unwrap_or(0)as f64}else{0.0}})))
}
fn gather(buffer:&[u8],width:usize,rows:(usize,usize),cols:&[(usize,usize)],dx:isize)->Vec<f64>{
 let mut output=Vec::with_capacity((rows.1-rows.0)*cols.iter().map(|(a,b)|b-a).sum::<usize>());output.extend(sample_values(buffer,width,rows,cols,dx));output
}
fn row_profile(buffer:&[u8],width:usize,height:usize,cols:&[(usize,usize)])->Vec<f64>{(0..height).map(|y|cols.iter().map(|&(start,end)|buffer[y*width+start..y*width+end].iter().map(|v|*v as f64).sum::<f64>()).sum()).collect()}
fn uniform(pixels:&Pixels,indexes:&[usize],minimum:usize,deviation:f64)->Option<[f64;3]>{if indexes.len()<minimum||indexes.iter().any(|i|pixels.rgba[i*4+3]<250){return None}let mut rgb=[0.0;3];for channel in 0..3{let mean=indexes.iter().map(|i|pixels.rgba[i*4+channel]as f64).sum::<f64>()/indexes.len()as f64;let variance=indexes.iter().map(|i|(pixels.rgba[i*4+channel]as f64-mean).powi(2)).sum::<f64>()/indexes.len()as f64;if variance.sqrt()>deviation{return None}rgb[channel]=(mean/255.0*10000.0).round()/10000.0;}Some(rgb)}
#[derive(Clone)]struct Style{spec:FontSpec,font_name:String,font_size:f64,offset:f64,line_height:f64,ascent:f64,descent:f64,weight:i32,italic:bool,line_top:f64,verified_at:Instant,text_rgb:Option<[f64;3]>,background_rgb:Option<[f64;3]>}
fn render_for(style:&Style,line:&Line)->Option<InlineRender>{
 if (line.height-style.line_height).abs()>0.5{return None}
 let fresh=(line.top-style.line_top).abs()<=1.0&&style.verified_at.elapsed()<=COLOUR_TTL;let text_rgb=if fresh{style.text_rgb}else{None};let background_rgb=if fresh{style.background_rgb}else{None};
 if (line.suffix.is_some()||line.ax.is_some())&&(text_rgb.is_none()||background_rgb.is_none()){return None}
 let render=if let Some(ax)=&line.ax{if style.spec!=FontSpec::Ax||ax.font_name!=style.font_name||(ax.font_size-style.font_size).abs()>0.01{return None}let mut render=ax.clone();render.source="macos-ax-attributed-run+vision".into();render.text_rgb=text_rgb;render.background_rgb=background_rgb;render}
 else{let font=font(&style.spec,style.font_size,line)?;let actual:*mut NSString=unsafe{msg_send![&*font,fontName]};if actual.is_null()||unsafe{&*actual}.to_string()!=style.font_name||!widths_fit(&font,&line.samples){return None}InlineRender{font_name:style.font_name.clone(),font_size:style.font_size,baseline:line.top+style.offset,clip_rect:line.clip,ascent:style.ascent,descent:style.descent,source:"macos-ax-geometry+vision".into(),font_weight:style.weight,italic:style.italic,system_font:matches!(style.spec,FontSpec::System(_)),system_weight:match style.spec{FontSpec::System(v)=>v,_=>0.0},text_rgb,background_rgb,fixed_advance:match style.spec{FontSpec::Fixed(_,v)=>v,_=>0.0},..Default::default()}};
 render.valid(Some(line.caret)).then_some(render)
}
struct Match{score:f64,spec:FontSpec,size:f64,baseline:f64,dx:isize,coverage:Vec<u8>,ascent:f64,descent:f64,font:Retained<AnyObject>}
fn match_pixels(pixels:&Pixels,line:&Line,options:&[(FontSpec,f64)])->Option<Style>{
 let mut spans:Vec<_>=line.samples.iter().chain(line.suffix.iter()).cloned().collect();spans.sort_by(|a,b|b.rect[2].total_cmp(&a.rect[2]));let mut kept:Vec<AppearanceSample>=Vec::new();for sample in spans{if !kept.iter().any(|s|intersects(s.rect,sample.rect)){kept.push(sample)}}kept.sort_by(|a,b|a.rect[0].total_cmp(&b.rect[0]));
 let scale=pixels.scale;let shift=(1.5*scale).ceil()as isize;let caret=scale.ceil()as isize;let mut cols=Vec::new();
 for sample in &kept{let mut start=((sample.rect[0]-pixels.origin[0])*scale).round()as isize;let mut end=((sample.rect[0]+sample.rect[2]-pixels.origin[0])*scale).round()as isize;if (sample.rect[0]+sample.rect[2]-line.caret[0]).abs()<=1.5{end-=caret}if (sample.rect[0]-line.caret[0]).abs()<=1.5{start+=caret}start=start.max(shift);end=end.min(pixels.width as isize-shift);if end-start>=2{cols.push((start as usize,end as usize))}}
 let rows=((((line.top-pixels.origin[1])*scale).floor().max(0.0)as usize).min(pixels.height),(((line.top+line.height-pixels.origin[1])*scale).ceil().max(0.0)as usize).min(pixels.height));
 if cols.is_empty()||rows.1.saturating_sub(rows.0)<4{return None}
 let luminance:Vec<u8>=pixels.rgba.chunks_exact(4).map(|p|((54*p[0]as u32+183*p[1]as u32+19*p[2]as u32)>>8)as u8).collect();let mut histogram=[0usize;256];for &v in &luminance{histogram[v as usize]+=1}let mut seen=0;let background=histogram.iter().position(|count|{seen+=count;seen>=(luminance.len()+1)/2}).unwrap_or(255)as u8;let ink:Vec<_>=luminance.into_iter().map(|v|v.abs_diff(background)).collect();
 let captured=gather(&ink,pixels.width,rows,&cols,0);if captured.iter().copied().fold(0.0,f64::max)<40.0||captured.iter().sum::<f64>()<4.0*captured.len()as f64{return None}
 let mut captured_rows=row_profile(&ink,pixels.width,pixels.height,&cols);for (y,value) in captured_rows.iter_mut().enumerate(){if y<rows.0||y>=rows.1{*value=0.0}}
 let vertical=((if line.ax.is_some(){1.0}else{0.5*line.height})*scale).ceil()as isize;let mut results=Vec::new();
 for (spec,size) in options{let Some(f)=font(spec,*size,line)else{continue};let ascent:f64=unsafe{msg_send![&*f,ascender]};let descent:f64=unsafe{msg_send![&*f,descender]};let descent=descent.abs();let guess=line.ax.as_ref().map_or(line.top-pixels.origin[1]+(line.height-ascent-descent)/2.0+ascent,|a|a.baseline-pixels.origin[1]);
 let Some(first)=render_mask(&f,&kept,pixels,guess)else{continue};let profile=row_profile(&first,pixels.width,pixels.height,&cols);let mut best_shift=0;let mut best_score=-2.0;
 for offset in -vertical..=vertical{let shifted=(0..pixels.height).map(|y|{let y=y as isize-offset;if y>=0{profile.get(y as usize).copied().unwrap_or(0.0)}else{0.0}});let score=ncc_values(&captured_rows,shifted);if score>best_score{best_score=score;best_shift=offset}}
 let baseline=guess+best_shift as f64/scale;let mask=if best_shift==0{first}else{let Some(mask)=render_mask(&f,&kept,pixels,baseline)else{continue};mask};let mut best=Match{score:-1.0,spec:spec.clone(),size:*size,baseline,dx:0,coverage:Vec::new(),ascent,descent,font:f};
 for dx in -shift..=shift{let score=ncc_values(&captured,sample_values(&mask,pixels.width,rows,&cols,dx));if score>best.score{best.score=score;best.dx=dx}}best.coverage=mask;
 for step in [-0.5,0.5]{let candidate=baseline+step/scale;let Some(mask)=render_mask(&best.font,&kept,pixels,candidate)else{continue};let score=ncc_values(&captured,sample_values(&mask,pixels.width,rows,&cols,best.dx));if score>best.score{best.score=score;best.baseline=candidate;best.coverage=mask}}
 results.push(best);
 }
 results.sort_by(|a,b|b.score.total_cmp(&a.score));if results.is_empty(){return None}let best=results.remove(0);if best.score<0.82||results.iter().find(|r|r.spec!=best.spec).is_some_and(|r|r.score>best.score-0.03){return None}
 let band=(cols.iter().map(|p|p.0).min()?,cols.iter().map(|p|p.1).max()?);let mut text=Vec::new();let mut bg=Vec::new();let mut total=0;
 for y in rows.0..rows.1{for x in band.0..band.1{let index=y*pixels.width+x;total+=1;let source=index as isize-best.dx;let covered=if source>=0{best.coverage.get(source as usize).copied().unwrap_or(0)}else{0};if covered>=230{text.push(index)}else if covered<=8&&ink[index]<=10{bg.push(index)}}}
 let mut text_rgb=uniform(pixels,&text,12,28.0);let mut background_rgb=uniform(pixels,&bg,12usize.max(total/2),6.0);
 if text_rgb.zip(background_rgb).is_some_and(|(a,b)|a.iter().zip(b).map(|(a,b)|(a-b).abs()).fold(0.0,f64::max)<0.08){text_rgb=None;background_rgb=None}
 if (line.suffix.is_some()||line.ax.is_some())&&(text_rgb.is_none()||background_rgb.is_none()){return None}
 let (name,baseline,ascent,descent,weight,italic)=if let Some(ax)=&line.ax{(ax.font_name.clone(),ax.baseline,ax.ascent,ax.descent,ax.font_weight,ax.italic)}else{unsafe{
 let name:*mut NSString=msg_send![&*best.font,fontName];if name.is_null(){return None}let traits=AxValue::from_owned(CTFontCopyTraits((&*best.font as *const AnyObject).cast()));let weight=traits.and_then(|v|v.dictionary_value("NSCTFontWeightTrait")?.as_number()).unwrap_or(0.0);
 let css=[(-0.8,100),(-0.6,200),(-0.4,300),(0.0,400),(0.23,500),(0.3,600),(0.4,700),(0.56,800),(0.62,900)].into_iter().min_by(|a,b|(a.0-weight).abs().total_cmp(&(b.0-weight).abs()))?.1;
 ((&*name).to_string(),best.baseline+pixels.origin[1],best.ascent,best.descent,css,CTFontGetSymbolicTraits((&*best.font as *const AnyObject).cast())&1!=0)
 }};
 let style=Style{spec:best.spec,font_name:name,font_size:best.size,offset:baseline-line.top,line_height:line.height,ascent,descent,weight,italic,line_top:line.top,verified_at:Instant::now(),text_rgb,background_rgb};
 render_for(&style,line).map(|_|style)
}
struct StyleJob{binding:[u8;32],work:Arc<Mutex<Option<StyleWork>>>,capture:Capture,epoch:u64}
fn snapshot_binding(snapshot:&Snapshot)->[u8;32]{
 let mut digest=Sha256::new();
 for text in [snapshot.app_id.as_str(),snapshot.field_id.as_str(),snapshot.text.as_ref(),snapshot.domain.as_str(),snapshot.window_id.as_str(),snapshot.window_revision.as_str()]{digest.update(text.len().to_le_bytes());digest.update(text.as_bytes());}
 digest.update(snapshot.caret.to_le_bytes());digest.update(snapshot.selection_length.to_le_bytes());digest.update(snapshot.window_pid.to_le_bytes());digest.update([snapshot.protected as u8,snapshot.multiline as u8,snapshot.on_demand_only as u8,snapshot.can_replace as u8]);digest.finalize().into()
}
struct StyleWork{line:Line,options:Vec<(FontSpec,f64)>,deadline:Instant}
fn expire_style_work(work:&Arc<Mutex<Option<StyleWork>>>){*work.lock().unwrap_or_else(|e|e.into_inner())=None;}
pub struct VisionBridge{enabled:bool,epoch:u64,job:Option<StyleJob>,styles:VecDeque<(String,Style)>,rejected:VecDeque<(String,Instant)>,last_capture:Option<Instant>}
impl Default for VisionBridge{fn default()->Self{Self{enabled:false,epoch:0,job:None,styles:VecDeque::new(),rejected:VecDeque::new(),last_capture:None}}}
impl VisionBridge{
 pub fn new()->Self{Self::default()}
 fn clear_styles(&mut self){self.epoch=self.epoch.wrapping_add(1);self.job=None;self.styles.clear();self.rejected.clear();self.last_capture=None;}
 pub fn reset(&mut self){self.clear_styles();clear_text_cache();}
 pub fn cancel(&mut self){self.reset()}
 fn reject(&mut self,line:&Line,seconds:u64){self.rejected.retain(|(key,_)|key!=&line.line_key);self.rejected.push_back((line.line_key.clone(),Instant::now()+Duration::from_secs(seconds)));while self.rejected.len()>64{self.rejected.pop_front();}}
 pub fn request(&mut self,snapshot:&Snapshot,evidence:&AppearanceEvidence,authorized:&mut dyn FnMut()->Result<bool>)->Result<Option<InlineRender>>{
 if !self.enabled{self.clear_styles();return Ok(None)}
 if unsafe{pthread_main_np()}==0{return Err(anyhow!("appearance-main-thread-required"))}
 if snapshot.protected||!authorized()?||secure()||!capture_granted(){self.reset();return Ok(None)}
 let Some(line)=parse(snapshot,evidence)else{return Ok(None)};
 if let Some(position)=self.styles.iter().position(|(key,_)|key==&line.style_key){let entry=self.styles.remove(position).unwrap();let render=render_for(&entry.1,&line);self.styles.push_back(entry);if render.is_some()&&authorized()?{return Ok(render)}}
 if let Some(mut job)=self.job.take(){
 let work=job.work.lock().unwrap_or_else(|e|e.into_inner()).take();
 if let Some(work)=work{if job.epoch==self.epoch&&job.binding==snapshot_binding(snapshot)&&work.line.line_key==line.line_key&&Instant::now()<work.deadline&&authorized()?{
 match job.capture.poll(){Ok(Some(shot))=>{
 let spans:Vec<_>=work.line.samples.iter().chain(work.line.suffix.iter()).collect();let left=spans.iter().map(|s|s.rect[0]).fold(f64::INFINITY,f64::min)-4.0;let right=spans.iter().map(|s|s.rect[0]+s.rect[2]).fold(f64::NEG_INFINITY,f64::max)+4.0;
 let pixels=shot.crop([left,work.line.top-work.line.height/2.0,right-left,work.line.height*2.0]);let number=shot.number;drop(shot);
 if authorized()?&&!secure()&&capture_granted()&&frontmost()==work.line.pid&&window_number(work.line.pid,work.line.window)==Some(number)&&job.epoch==self.epoch{
 if let Some(style)=pixels.and_then(|pixels|match_pixels(&pixels,&work.line,&work.options)){if authorized()?{let render=render_for(&style,&line);self.styles.retain(|(key,_)|key!=&line.style_key);self.styles.push_back((line.style_key.clone(),style));while self.styles.len()>32{self.styles.pop_front();}return Ok(render)}}else{self.reject(&line,3);}
 }
 },Ok(None)=>{let pending={let mut slot=job.work.lock().unwrap_or_else(|e|e.into_inner());if Instant::now()<work.deadline{*slot=Some(work);true}else{false}};if pending{self.job=Some(job);}return Ok(None)},Err(_)=>self.reject(&line,3)}
 }}
 }
 if self.rejected.iter().any(|(key,expiry)|key==&line.line_key&&*expiry>Instant::now())||self.last_capture.is_some_and(|t|t.elapsed()<CAPTURE_INTERVAL){return Ok(None)}
 if frontmost()!=line.pid||!authorized()?{return Ok(None)}let options=candidates(&line);if options.is_empty(){self.reject(&line,30);return Ok(None)}
 if let Some(capture)=Capture::start(line.pid,line.window)?{
 self.last_capture=Some(Instant::now());let work=Arc::new(Mutex::new(Some(StyleWork{line,options,deadline:Instant::now()+TIMEOUT})));let expires=Arc::downgrade(&work);
 std::thread::spawn(move||{std::thread::sleep(TIMEOUT);if let Some(work)=expires.upgrade(){expire_style_work(&work)}});
 self.job=Some(StyleJob{binding:snapshot_binding(snapshot),work,capture,epoch:self.epoch});
 }else{self.reject(&line,3)}
 Ok(None)
 }
}
impl AppearanceIntegration for VisionBridge{
 fn configure(&mut self,config:&Config)->Result<()>{self.enabled=config.screenshot_appearance;self.reset();Ok(())}
 fn style(&mut self,services:&mut CaptureServices<'_>,snapshot:&Snapshot,evidence:&AppearanceEvidence)->Result<Option<InlineRender>>{
 self.request(snapshot,evidence,&mut||Ok(services.secure_input()==Some(false)&&!services.dead_key()&&services.context_matches(snapshot)?))
 }
 fn stop(&mut self)->Result<()>{self.enabled=false;self.reset();Ok(())}
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn correlation_refuses_empty_and_constant(){assert_eq!(ncc(&[],&[]),0.0);assert_eq!(ncc(&[1.0,1.0],&[2.0,2.0]),0.0);assert!((ncc(&[1.0,2.0,4.0],&[3.0,6.0,12.0])-1.0).abs()<1e-12);}
 #[test]fn secure_unicode_samples_never_become_font_evidence(){for text in ["hello\nworld","אבג","a\u{202e}b",""]{assert!(!safe_span(&AppearanceSample{text:text.into(),rect:[0.0,0.0,100.0,20.0]}))}}
 #[test]fn ocr_omits_field_passwords_and_low_confidence(){let observations=vec![Observation{text:"Subject".into(),confidence:0.9,rect:[0.0,0.0,50.0,10.0]},Observation{text:"secret draft".into(),confidence:1.0,rect:[0.0,20.0,80.0,20.0]},Observation{text:"••••".into(),confidence:1.0,rect:[100.0,0.0,30.0,10.0]},Observation{text:"noise".into(),confidence:0.3,rect:[100.0,50.0,30.0,10.0]}];assert_eq!(compose(observations,Some([0.0,20.0,100.0,20.0]),4000),"Subject");}
 #[test]fn revoked_epoch_cannot_republish_plaintext(){clear_text_cache();let epoch=text_cache().lock().unwrap().epoch;clear_text_cache();assert!(!retain_text(TextKey{authority:Snapshot::default().authority(),window:[0.0,0.0,1.0,1.0],field:None},"synthetic".into(),epoch));assert!(text_cache().lock().unwrap().value.is_none());}
 #[test]fn callback_rejects_late_native_results(){let reply=Reply::new();reply.cancel();assert!(!reply.accepting.load(Ordering::Acquire));assert!(reply.take().is_none());}
 #[test]fn style_reset_forgets_positive_and_negative_caches(){let mut bridge=VisionBridge::new();bridge.rejected.push_back(("synthetic".into(),Instant::now()+Duration::from_secs(30)));let epoch=bridge.epoch;bridge.reset();assert!(bridge.rejected.is_empty());assert!(bridge.styles.is_empty());assert!(bridge.job.is_none());assert_ne!(epoch,bridge.epoch);}
 #[test]fn terminal_context_is_blocked_before_any_capture_or_authorization(){for app_id in [super::super::terminal::TERMINAL,super::super::terminal::ITERM]{let snapshot=Snapshot{app_id:app_id.into(),field_id:"terminal:synthetic".into(),window_pid:123,window_bounds:Some([0.0,0.0,800.0,600.0]),..Default::default()};let result=recognize_window_text(&snapshot,&||panic!("terminal consent must not reach OCR"),&mut||panic!("terminal snapshot must not be captured"),4000).unwrap();assert_eq!(result.status,"blocked");assert!(result.text.is_empty());}}
 fn synthetic_line()->Line{let ax=InlineRender{font_name:"Synthetic".into(),font_size:14.0,baseline:115.0,clip_rect:[0.0,0.0,800.0,600.0],ascent:10.0,descent:3.0,source:"synthetic".into(),..Default::default()};Line{samples:vec![],suffix:None,caret:[200.0,100.0,1.0,20.0],clip:ax.clip_rect,top:100.0,height:20.0,font_name:"Synthetic".into(),font_family:String::new(),font_size:14.0,fixed_advance:0.0,ax:Some(ax),pid:123,window:[0.0,0.0,800.0,600.0],style_key:"synthetic-style".into(),line_key:"synthetic-line".into()}}
 #[test]fn appearance_colour_cache_expires_and_cannot_cross_lines(){let line=synthetic_line();let mut style=Style{spec:FontSpec::Ax,font_name:"Synthetic".into(),font_size:14.0,offset:15.0,line_height:20.0,ascent:10.0,descent:3.0,weight:400,italic:false,line_top:100.0,verified_at:Instant::now(),text_rgb:Some([0.0;3]),background_rgb:Some([1.0;3])};assert!(render_for(&style,&line).is_some());style.verified_at=Instant::now()-COLOUR_TTL-Duration::from_secs(1);assert!(render_for(&style,&line).is_none());style.verified_at=Instant::now();style.line_top=102.0;assert!(render_for(&style,&line).is_none());}
 #[test]fn pending_sample_expiry_drops_text_without_owner_polling(){let work=Arc::new(Mutex::new(Some(StyleWork{line:synthetic_line(),options:Vec::new(),deadline:Instant::now()})));expire_style_work(&work);assert!(work.lock().unwrap().is_none());}
 #[test]fn authority_binding_rejects_same_length_edit_and_window_change(){let original=Snapshot{text:"hello".into(),caret:5,window_id:"window1".into(),..Default::default()};let mut changed=original.clone();changed.text="world".into();assert_ne!(snapshot_binding(&original),snapshot_binding(&changed));changed=original.clone();changed.window_id="window2".into();assert_ne!(snapshot_binding(&original),snapshot_binding(&changed));}
 #[test]fn sibling_windows_ambiguous_frames_and_unshared_content_cannot_be_captured(){let bounds=[0.0,0.0,800.0,600.0];let target=WindowRecord{pid:123,layer:0.0,frame:bounds,sharing:1.0,alpha:1.0,number:7.0};let mut sibling=target.clone();sibling.pid=456;assert_eq!(select_window([sibling,target.clone()].into_iter(),123,bounds),Some(7));assert_eq!(select_window([target.clone(),target.clone()].into_iter(),123,bounds),None);let mut private=target.clone();private.sharing=0.0;assert_eq!(select_window([private].into_iter(),123,bounds),None);let mut moved=target;moved.frame[0]=10.0;assert_eq!(select_window([moved].into_iter(),123,bounds),None);}
 #[test]fn protected_ocr_never_invokes_native_authority_or_capture(){let snapshot=Snapshot{protected:true,..Default::default()};let result=recognize_window_text(&snapshot,&||panic!("protected read"),&mut||panic!("protected native capture"),4000).unwrap();assert_eq!(result.status,"blocked");assert!(result.text.is_empty());}
}
