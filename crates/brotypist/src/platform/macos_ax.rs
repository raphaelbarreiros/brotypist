//! Typed, retained Accessibility/Core Foundation values. Native owners never cross threads.
use crate::core::Rect;
use std::{ffi::c_void, fmt, marker::PhantomData, ptr::NonNull, rc::Rc};

pub type Raw = *const c_void;
#[repr(C)] #[derive(Clone,Copy,Debug,Default,PartialEq,Eq)] pub struct CfRange { pub location:isize,pub length:isize }
#[repr(C)] #[derive(Clone,Copy,Debug,Default)] pub struct Point { pub x:f64,pub y:f64 }
#[repr(C)] #[derive(Clone,Copy,Debug,Default)] pub struct Size { pub width:f64,pub height:f64 }
#[repr(C)] #[derive(Clone,Copy,Debug,Default)] pub struct NativeRect { pub origin:Point,pub size:Size }
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)] pub struct AxRange { pub location:usize,pub length:usize }
impl AxRange {
    pub fn end(self)->Option<usize>{self.location.checked_add(self.length)}
    fn native(self)->Option<CfRange>{Some(CfRange{location:self.location.try_into().ok()?,length:self.length.try_into().ok()?})}
    fn from_native(value:CfRange)->Option<Self>{Some(Self{location:value.location.try_into().ok()?,length:value.length.try_into().ok()?})}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)] pub struct AxError(pub i32);
impl fmt::Display for AxError {fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{write!(f,"Accessibility operation failed ({})",self.0)}}
impl std::error::Error for AxError{}

/// Canonical ABI declarations shared by AX, Quartz and special adapters.
/// CFTypeRef/CFStringRef are const; runloops, events and Objective-C objects
/// are SDK mutable opaque references. CoreGraphics uses C `bool`, unlike the
/// CoreFoundation `Boolean` byte used by CFRunLoopRunInMode.
pub mod ffi {
    use super::Raw;
    use std::ffi::{c_char,c_void};
    pub type Native=*mut c_void;
    pub type RunLoopRef=*mut c_void;
    pub type RunLoopSourceRef=*mut c_void;
    pub type ObjcRef=*mut c_void;
    pub type TapCallback=unsafe extern "C" fn(Native,u32,Native,Native)->Native;
    pub use super::{CFRetain,CFRelease};
    #[link(name="CoreFoundation",kind="framework")]
    unsafe extern "C" {
        pub static kCFRunLoopCommonModes:Raw;
        pub static kCFRunLoopDefaultMode:Raw;
        pub fn CFRunLoopGetCurrent()->RunLoopRef;
        pub fn CFRunLoopAddSource(runloop:RunLoopRef,source:RunLoopSourceRef,mode:Raw);
        pub fn CFRunLoopRemoveSource(runloop:RunLoopRef,source:RunLoopSourceRef,mode:Raw);
        pub fn CFRunLoopRunInMode(mode:Raw,seconds:f64,return_after_source:u8)->i32;
        pub fn CFMachPortCreateRunLoopSource(allocator:Raw,port:Native,order:isize)->RunLoopSourceRef;
        pub fn CFMachPortInvalidate(port:Native);
    }
    #[link(name="ApplicationServices",kind="framework")]
    unsafe extern "C" {
        pub fn CGEventTapCreate(location:u32,placement:u32,options:u32,mask:u64,callback:TapCallback,context:Native)->Native;
        pub fn CGEventTapEnable(tap:Native,enabled:bool);
        pub fn CGEventCreateCopy(event:Native)->Native;
        pub fn CGEventGetIntegerValueField(event:Native,field:u32)->i64;
        pub fn CGEventSetIntegerValueField(event:Native,field:u32,value:i64);
        pub fn CGEventGetFlags(event:Native)->u64;
        pub fn CGEventSetFlags(event:Native,flags:u64);
        pub fn CGEventKeyboardGetUnicodeString(event:Native,capacity:usize,length:*mut usize,text:*mut u16);
        pub fn CGEventCreateKeyboardEvent(source:Native,code:u16,down:bool)->Native;
        pub fn CGEventKeyboardSetUnicodeString(event:Native,length:usize,text:*const u16);
        pub fn CGEventPost(location:u32,event:Native);
        pub fn CGEventSourceFlagsState(state:i32)->u64;
        pub fn CGPreflightPostEventAccess()->bool;
        pub fn CGRequestListenEventAccess()->bool;
        pub fn CGRequestPostEventAccess()->bool;
        pub fn CGPreflightScreenCaptureAccess()->bool;
        pub fn CGRequestScreenCaptureAccess()->bool;
    }
    #[link(name="objc")]
    unsafe extern "C" {
        pub fn objc_getClass(name:*const c_char)->ObjcRef;
        pub fn sel_registerName(name:*const c_char)->ObjcRef;
        pub fn objc_msgSend();
        pub fn objc_allocateClassPair(superclass:ObjcRef,name:*const c_char,extra:usize)->ObjcRef;
        pub fn objc_registerClassPair(class:ObjcRef);
        pub fn class_addIvar(class:ObjcRef,name:*const c_char,size:usize,alignment:u8,types:*const c_char)->bool;
        pub fn class_addMethod(class:ObjcRef,selector:ObjcRef,implementation:unsafe extern "C" fn(),types:*const c_char)->bool;
        pub fn object_getInstanceVariable(object:ObjcRef,name:*const c_char,value:*mut ObjcRef)->ObjcRef;
        pub fn object_setInstanceVariable(object:ObjcRef,name:*const c_char,value:ObjcRef)->ObjcRef;
        pub fn objc_release(object:ObjcRef);
        pub fn objc_autoreleasePoolPush()->*mut c_void;
        pub fn objc_autoreleasePoolPop(pool:*mut c_void);
    }
}
use ffi::{CFRunLoopGetCurrent,CFRunLoopAddSource,CFRunLoopRemoveSource,kCFRunLoopCommonModes};

#[link(name="CoreFoundation",kind="framework")]
unsafe extern "C" {
    pub fn CFRetain(value:Raw)->Raw;
    pub fn CFRelease(value:Raw);
    pub fn CFGetTypeID(value:Raw)->usize;
    pub fn CFHash(value:Raw)->usize;
    pub fn CFEqual(a:Raw,b:Raw)->u8;
    fn CFStringGetTypeID()->usize;
    fn CFStringCreateWithCharacters(allocator:Raw,characters:*const u16,length:isize)->Raw;
    fn CFStringGetLength(value:Raw)->isize;
    fn CFStringGetCharacters(value:Raw,range:CfRange,buffer:*mut u16);
    fn CFBooleanGetTypeID()->usize;
    fn CFBooleanGetValue(value:Raw)->u8;
    fn CFNumberGetTypeID()->usize;
    fn CFNumberGetValue(value:Raw,kind:isize,output:*mut c_void)->u8;
    fn CFNumberCreate(allocator:Raw,kind:isize,value:*const c_void)->Raw;
    fn CFURLGetTypeID()->usize;
    fn CFURLGetString(value:Raw)->Raw;
    fn CFArrayGetTypeID()->usize;
    fn CFArrayGetCount(value:Raw)->isize;
    fn CFArrayGetValueAtIndex(value:Raw,index:isize)->Raw;
    fn CFDictionaryGetTypeID()->usize;
    fn CFDictionaryGetValue(dictionary:Raw,key:Raw)->Raw;
    fn CFDictionaryCreate(allocator:Raw,keys:*const Raw,values:*const Raw,count:isize,key_callbacks:Raw,value_callbacks:Raw)->Raw;
    fn CFAttributedStringGetTypeID()->usize;
    fn CFAttributedStringGetLength(value:Raw)->isize;
    fn CFAttributedStringGetString(value:Raw)->Raw;
    fn CFAttributedStringGetAttributeAndLongestEffectiveRange(value:Raw,index:isize,name:Raw,limit:CfRange,effective:*mut CfRange)->Raw;
    pub static kCFBooleanTrue:Raw;
    pub static kCFBooleanFalse:Raw;
}
#[link(name="ApplicationServices",kind="framework")]
unsafe extern "C" {
    fn AXUIElementGetTypeID()->usize;
    fn AXUIElementCreateApplication(pid:i32)->Raw;
    fn AXUIElementCreateSystemWide()->Raw;
    fn AXUIElementGetPid(element:Raw,pid:*mut i32)->i32;
    fn AXUIElementCopyAttributeValue(element:Raw,name:Raw,result:*mut Raw)->i32;
    fn AXUIElementCopyParameterizedAttributeValue(element:Raw,name:Raw,parameter:Raw,result:*mut Raw)->i32;
    fn AXUIElementCopyActionNames(element:Raw,result:*mut Raw)->i32;
    fn AXUIElementPerformAction(element:Raw,name:Raw)->i32;
    fn AXUIElementIsAttributeSettable(element:Raw,name:Raw,result:*mut u8)->i32;
    fn AXUIElementSetAttributeValue(element:Raw,name:Raw,value:Raw)->i32;
    fn AXUIElementSetMessagingTimeout(element:Raw,seconds:f32)->i32;
    fn AXValueGetTypeID()->usize;
    fn AXValueGetType(value:Raw)->u32;
    fn AXValueGetValue(value:Raw,kind:u32,result:*mut c_void)->u8;
    fn AXValueCreate(kind:u32,value:*const c_void)->Raw;
    pub fn AXIsProcessTrusted()->u8;
    fn AXIsProcessTrustedWithOptions(options:Raw)->u8;
    static kAXTrustedCheckOptionPrompt:Raw;
    fn AXObserverCreate(pid:i32,callback:unsafe extern "C" fn(Raw,Raw,Raw,*mut c_void),observer:*mut Raw)->i32;
    fn AXObserverGetRunLoopSource(observer:Raw)->ffi::RunLoopSourceRef;
    fn AXObserverAddNotification(observer:Raw,element:Raw,name:Raw,context:*mut c_void)->i32;
    fn AXObserverRemoveNotification(observer:Raw,element:Raw,name:Raw)->i32;
}
#[link(name="AppKit",kind="framework")]
unsafe extern "C" {fn NSAccessibilityRoleDescription(role:Raw,subrole:Raw)->Raw;}

/// Every instance owns one +1 CF reference and is deliberately !Send/!Sync.
#[derive(Debug)] pub struct AxValue {raw:NonNull<c_void>,owner:PhantomData<Rc<()>>}
impl Clone for AxValue {fn clone(&self)->Self{unsafe{CFRetain(self.as_raw());} Self{raw:self.raw,owner:PhantomData}}}
impl Drop for AxValue {fn drop(&mut self){unsafe{CFRelease(self.as_raw())}}}
impl AxValue {
    /// SAFETY: caller transfers one valid owned Core Foundation reference.
    pub unsafe fn from_owned(raw:Raw)->Option<Self>{Some(Self{raw:NonNull::new(raw.cast_mut())?,owner:PhantomData})}
    /// SAFETY: caller supplies a live borrowed CF reference; it is retained here.
    pub unsafe fn from_borrowed(raw:Raw)->Option<Self>{if raw.is_null(){return None} unsafe{CFRetain(raw);} unsafe{Self::from_owned(raw)}}
    pub fn as_raw(&self)->Raw{self.raw.as_ptr()}
    pub fn string(value:&str)->Option<Self>{let units:Vec<u16>=value.encode_utf16().collect(); unsafe{Self::from_owned(CFStringCreateWithCharacters(std::ptr::null(),units.as_ptr(),units.len().try_into().ok()?))}}
    fn is_type(&self,id:unsafe extern "C" fn()->usize)->bool{unsafe{CFGetTypeID(self.as_raw())==id()}}
    pub fn as_string(&self)->Option<String>{
        if self.is_type(CFURLGetTypeID){let value=unsafe{Self::from_borrowed(CFURLGetString(self.as_raw()))}?;return value.as_string()}
        if !self.is_type(CFStringGetTypeID){return None}
        let length=unsafe{CFStringGetLength(self.as_raw())}; if !(0..=2_000_000).contains(&length){return None}
        let mut buffer=vec![0u16;length as usize]; unsafe{CFStringGetCharacters(self.as_raw(),CfRange{location:0,length},buffer.as_mut_ptr())}; String::from_utf16(&buffer).ok()
    }
    pub fn as_bool(&self)->Option<bool>{self.is_type(CFBooleanGetTypeID).then(||unsafe{CFBooleanGetValue(self.as_raw())!=0})}
    pub fn as_number(&self)->Option<f64>{if !self.is_type(CFNumberGetTypeID){return None}let mut number=0f64; (unsafe{CFNumberGetValue(self.as_raw(),13,(&mut number as *mut f64).cast())}!=0).then_some(number)}
    fn native_value<T:Default>(&self,kind:u32)->Option<T>{if !self.is_type(AXValueGetTypeID)||unsafe{AXValueGetType(self.as_raw())}!=kind{return None}let mut value=T::default(); (unsafe{AXValueGetValue(self.as_raw(),kind,(&mut value as *mut T).cast())}!=0).then_some(value)}
    pub fn as_range(&self)->Option<AxRange>{AxRange::from_native(self.native_value::<CfRange>(4)?)}
    pub fn as_point(&self)->Option<[f64;2]>{let value=self.native_value::<Point>(1)?;Some([value.x,value.y])}
    pub fn as_size(&self)->Option<[f64;2]>{let value=self.native_value::<Size>(2)?;Some([value.width,value.height])}
    pub fn as_rect(&self)->Option<Rect>{let value=self.native_value::<NativeRect>(3)?;Some([value.origin.x,value.origin.y,value.size.width,value.size.height])}
    pub fn as_element(&self)->Option<AxElement>{self.is_type(AXUIElementGetTypeID).then(||AxElement(self.clone()))}
    pub fn as_elements(&self)->Option<Vec<AxElement>>{
        if !self.is_type(CFArrayGetTypeID){return None}let count=unsafe{CFArrayGetCount(self.as_raw())};if !(0..=4096).contains(&count){return None}
        (0..count).map(|index|{let value=unsafe{Self::from_borrowed(CFArrayGetValueAtIndex(self.as_raw(),index))}?;value.as_element()}).collect()
    }
    pub fn dictionary_value(&self,key:&str)->Option<AxValue>{if !self.is_type(CFDictionaryGetTypeID){return None}let key=Self::string(key)?;unsafe{Self::from_borrowed(CFDictionaryGetValue(self.as_raw(),key.as_raw()))}}
    pub fn as_strings(&self)->Option<Vec<String>>{
        if !self.is_type(CFArrayGetTypeID){return None}let count=unsafe{CFArrayGetCount(self.as_raw())};if !(0..=4096).contains(&count){return None}
        (0..count).map(|index|unsafe{Self::from_borrowed(CFArrayGetValueAtIndex(self.as_raw(),index))}?.as_string()).collect()
    }
    fn integer(value:usize)->Option<Self>{let value:isize=value.try_into().ok()?;unsafe{Self::from_owned(CFNumberCreate(std::ptr::null(),14,(&value as *const isize).cast()))}}
    fn range(value:AxRange)->Option<Self>{let native=value.native()?;unsafe{Self::from_owned(AXValueCreate(4,(&native as *const CfRange).cast()))}}
}
#[derive(Clone,Debug)] pub struct AxElement(AxValue);
impl PartialEq for AxElement {fn eq(&self,other:&Self)->bool{unsafe{CFEqual(self.as_raw(),other.as_raw())!=0}}}impl Eq for AxElement{}
impl AxElement {
    pub fn application(pid:i32)->Option<Self>{if pid<=0{return None}unsafe{AxValue::from_owned(AXUIElementCreateApplication(pid))}.map(Self)}
    pub fn system_wide()->Option<Self>{unsafe{AxValue::from_owned(AXUIElementCreateSystemWide())}.map(Self)}
    pub fn as_raw(&self)->Raw{self.0.as_raw()}
    pub fn identity(&self)->u64{unsafe{CFHash(self.as_raw()) as u64}}
    pub fn pid(&self)->Option<i32>{let mut pid=0;(unsafe{AXUIElementGetPid(self.as_raw(),&mut pid)}==0&&pid>0).then_some(pid)}
    pub fn attribute(&self,name:&str)->Result<Option<AxValue>,AxError>{
        let name=AxValue::string(name).ok_or(AxError(-25200))?;let mut value=std::ptr::null();
        let error=unsafe{AXUIElementCopyAttributeValue(self.as_raw(),name.as_raw(),&mut value)};
        if error==0{return Ok(unsafe{AxValue::from_owned(value)})}
        // AX no-value/unsupported are absence. Other errors remain distinguishable.
        if matches!(error,-25205|-25212){Ok(None)}else{Err(AxError(error))}
    }
    pub fn string(&self,name:&str)->Option<String>{self.attribute(name).ok()??.as_string()}
    pub fn bool(&self,name:&str)->Option<bool>{self.attribute(name).ok()??.as_bool()}
    pub fn number(&self,name:&str)->Option<f64>{self.attribute(name).ok()??.as_number()}
    pub fn range(&self,name:&str)->Option<AxRange>{self.attribute(name).ok()??.as_range()}
    pub fn element(&self,name:&str)->Option<Self>{self.attribute(name).ok()??.as_element()}
    pub fn children(&self)->Option<Vec<Self>>{self.attribute("AXChildren").ok()??.as_elements()}
    pub fn frame(&self)->Option<Rect>{let point=self.attribute("AXPosition").ok()??.as_point()?;let size=self.attribute("AXSize").ok()??.as_size()?;let rect=[point[0],point[1],size[0],size[1]]; (rect.iter().all(|x|x.is_finite())&&rect[2]>0.&&rect[3]>0.).then_some(rect)}
    pub fn settable(&self,name:&str)->bool{let Some(name)=AxValue::string(name)else{return false};let mut value=0;unsafe{AXUIElementIsAttributeSettable(self.as_raw(),name.as_raw(),&mut value)==0&&value!=0}}
    fn set(&self,name:&str,value:&AxValue)->Result<(),AxError>{let name=AxValue::string(name).ok_or(AxError(-25200))?;let code=unsafe{AXUIElementSetAttributeValue(self.as_raw(),name.as_raw(),value.as_raw())};if code==0{Ok(())}else{Err(AxError(code))}}
    pub fn set_string(&self,name:&str,value:&str)->Result<(),AxError>{self.set(name,&AxValue::string(value).ok_or(AxError(-25200))?)}
    pub fn set_bool(&self,name:&str,value:bool)->Result<(),AxError>{let value=unsafe{AxValue::from_borrowed(if value{kCFBooleanTrue}else{kCFBooleanFalse})}.ok_or(AxError(-25200))?;self.set(name,&value)}
    pub fn set_range(&self,name:&str,value:AxRange)->Result<(),AxError>{self.set(name,&AxValue::range(value).ok_or(AxError(-25200))?)}
    pub fn set_timeout(&self,seconds:f32)->Result<(),AxError>{if !seconds.is_finite()||seconds<=0.{return Err(AxError(-25201))}let code=unsafe{AXUIElementSetMessagingTimeout(self.as_raw(),seconds)};if code==0{Ok(())}else{Err(AxError(code))}}
    fn parameterized(&self,name:&str,range:AxRange)->Option<AxValue>{let name=AxValue::string(name)?;let range=AxValue::range(range)?;let mut value=std::ptr::null();if unsafe{AXUIElementCopyParameterizedAttributeValue(self.as_raw(),name.as_raw(),range.as_raw(),&mut value)}!=0{return None}unsafe{AxValue::from_owned(value)}}
    pub fn bounds_for_range(&self,range:AxRange)->Option<Rect>{let rect=self.parameterized("AXBoundsForRange",range)?.as_rect()?;(rect.iter().all(|x|x.is_finite())&&rect[3]>0.&&(rect[2]>0.||(rect[2]==0.&&range.length==0))).then_some(rect)}
    pub fn attributed_string_for_range(&self,range:AxRange)->Option<AxAttributedString>{let value=self.parameterized("AXAttributedStringForRange",range)?;value.is_type(CFAttributedStringGetTypeID).then_some(AxAttributedString(value))}
    pub fn strings(&self,name:&str)->Result<Option<Vec<String>>,AxError>{self.attribute(name)?.map(|v|v.as_strings().ok_or(AxError(-25200))).transpose()}
    pub fn actions(&self)->Result<Vec<String>,AxError>{let mut raw=std::ptr::null();let code=unsafe{AXUIElementCopyActionNames(self.as_raw(),&mut raw)};if code!=0{return Err(AxError(code))}unsafe{AxValue::from_owned(raw)}.and_then(|v|v.as_strings()).ok_or(AxError(-25200))}
    pub fn perform_action(&self,name:&str)->Result<(),AxError>{let name=AxValue::string(name).ok_or(AxError(-25200))?;let code=unsafe{AXUIElementPerformAction(self.as_raw(),name.as_raw())};if code==0{Ok(())}else{Err(AxError(code))}}
    fn parameter(&self,name:&str,parameter:&AxValue)->Result<Option<AxValue>,AxError>{let name=AxValue::string(name).ok_or(AxError(-25200))?;let mut raw=std::ptr::null();let code=unsafe{AXUIElementCopyParameterizedAttributeValue(self.as_raw(),name.as_raw(),parameter.as_raw(),&mut raw)};if code==0{Ok(unsafe{AxValue::from_owned(raw)})}else if matches!(code,-25205|-25212|-25213){Ok(None)}else{Err(AxError(code))}}
    pub fn range_for_line(&self,line:usize)->Result<Option<AxRange>,AxError>{self.parameter("AXRangeForLine",&AxValue::integer(line).ok_or(AxError(-25201))?)?.map(|v|v.as_range().ok_or(AxError(-25200))).transpose()}
    pub fn line_for_index(&self,index:usize)->Result<Option<usize>,AxError>{self.parameter("AXLineForIndex",&AxValue::integer(index).ok_or(AxError(-25201))?)?.map(|v|{let value=v.as_number().ok_or(AxError(-25200))?;if value.is_finite()&&value>=0.&&value.fract()==0.&&value<usize::MAX as f64{Ok(value as usize)}else{Err(AxError(-25201))}}).transpose()}
    pub fn string_for_range(&self,range:AxRange)->Result<Option<String>,AxError>{self.parameter("AXStringForRange",&AxValue::range(range).ok_or(AxError(-25201))?)?.map(|v|v.as_string().ok_or(AxError(-25200))).transpose()}
}
#[derive(Clone,Debug)] pub struct AxAttributedString(AxValue);
impl AxAttributedString {
    pub fn text(&self)->Option<String>{unsafe{AxValue::from_borrowed(CFAttributedStringGetString(self.0.as_raw()))}?.as_string()}
    pub fn len_utf16(&self)->usize{unsafe{CFAttributedStringGetLength(self.0.as_raw())}.try_into().unwrap_or(0)}
    pub fn attribute_longest(&self,key:&str,index:usize,limit:AxRange)->Option<(Option<AxValue>,AxRange)>{
        if index>=self.len_utf16()||limit.end()?>self.len_utf16(){return None}let key=AxValue::string(key)?;let mut effective=CfRange::default();
        let value=unsafe{CFAttributedStringGetAttributeAndLongestEffectiveRange(self.0.as_raw(),index.try_into().ok()?,key.as_raw(),limit.native()?,&mut effective)};
        Some((unsafe{AxValue::from_borrowed(value)},AxRange::from_native(effective)?))
    }
}
pub fn trusted()->bool{unsafe{AXIsProcessTrusted()!=0}}
pub fn request_trust()->bool{
    let keys=[unsafe{kAXTrustedCheckOptionPrompt}];let values=[unsafe{kCFBooleanTrue}];
    // Null callbacks borrow key/value for this synchronous explicit permission call.
    let Some(options)=(unsafe{AxValue::from_owned(CFDictionaryCreate(std::ptr::null(),keys.as_ptr(),values.as_ptr(),1,std::ptr::null(),std::ptr::null()))})else{return false};
    unsafe{AXIsProcessTrustedWithOptions(options.as_raw())!=0}
}
pub fn secure_role_description()->Option<String>{let role=AxValue::string("AXTextField")?;let subrole=AxValue::string("AXSecureTextField")?;unsafe{AxValue::from_borrowed(NSAccessibilityRoleDescription(role.as_raw(),subrole.as_raw()))}?.as_string()}

const NOTIFICATIONS:&[&str]=&["AXFocusedUIElementChanged","AXFocusedWindowChanged","AXMainWindowChanged","AXValueChanged","AXSelectedTextChanged","AXWindowMoved","AXWindowResized"];
struct NotificationContext{callback:Box<dyn Fn(&str,u64)>}
pub struct AxObservation{observer:AxValue,app:AxElement,runloop:ffi::RunLoopRef,context:Box<NotificationContext>}
impl AxObservation {
    pub fn new(pid:i32,callback:Box<dyn Fn(&str,u64)>)->Result<Self,AxError>{
        let app=AxElement::application(pid).ok_or(AxError(-25200))?;let mut context=Box::new(NotificationContext{callback});let mut raw=std::ptr::null();
        unsafe extern "C" fn notified(_observer:Raw,element:Raw,name:Raw,context:*mut c_void){
            // The context lives until notifications are detached on this same runloop.
            // No unwinding or AX capture is permitted across this C callback boundary.
            let _=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
                let context=unsafe{&*(context as *const NotificationContext)};
                let name=unsafe{AxValue::from_borrowed(name)}.and_then(|v|v.as_string());
                if let Some(name)=name{(context.callback)(&name,unsafe{CFHash(element)} as u64)}
            }));
        }
        let code=unsafe{AXObserverCreate(pid,notified,&mut raw)};if code!=0{return Err(AxError(code))}
        let observer=unsafe{AxValue::from_owned(raw)}.ok_or(AxError(-25200))?;let runloop=unsafe{CFRunLoopGetCurrent()};
        for name in NOTIFICATIONS{let native=AxValue::string(name).ok_or(AxError(-25200))?;unsafe{AXObserverAddNotification(observer.as_raw(),app.as_raw(),native.as_raw(),(&mut *context as *mut NotificationContext).cast())};}
        unsafe{CFRunLoopAddSource(runloop,AXObserverGetRunLoopSource(observer.as_raw()),kCFRunLoopCommonModes)};
        Ok(Self{observer,app,runloop,context})
    }
}
impl Drop for AxObservation{fn drop(&mut self){
    // Remove the runloop source and all registrations before releasing callback storage.
    unsafe{CFRunLoopRemoveSource(self.runloop,AXObserverGetRunLoopSource(self.observer.as_raw()),kCFRunLoopCommonModes)};
    for name in NOTIFICATIONS{if let Some(native)=AxValue::string(name){unsafe{AXObserverRemoveNotification(self.observer.as_raw(),self.app.as_raw(),native.as_raw())};}}
    let _=&self.context;
}}

#[cfg(test)] mod tests{
    use super::*;
    #[test]fn native_range_never_wraps(){assert_eq!(AxRange{location:usize::MAX,length:1}.end(),None);assert!(AxRange{location:usize::MAX,length:0}.native().is_none());assert!(AxRange::from_native(CfRange{location:-1,length:0}).is_none());}
    #[test]fn cf_strings_preserve_unormalized_scalars_and_reject_wrong_types(){for text in ["a😀e\u{301}","\u{2028}",""]{let value=AxValue::string(text).unwrap();assert_eq!(value.as_string().as_deref(),Some(text));assert_eq!(value.as_range(),None);assert_eq!(value.as_bool(),None);}}
}
