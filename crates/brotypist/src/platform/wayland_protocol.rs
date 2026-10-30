//! Private input-method-v1 wire ABI. Protocol descriptors are Rust data, not a C sidecar.
//! Signatures/opcodes follow wayland-protocols unstable/input-method/input-method-unstable-v1.xml.
#![allow(unsafe_op_in_unsafe_fn)]
use libc::{c_char,c_int,c_void};
use std::ptr;
#[repr(C)] pub(super) struct Interface {pub name:*const c_char,pub version:c_int,pub method_count:c_int,pub methods:*const Message,pub event_count:c_int,pub events:*const Message}
#[repr(C)] pub(super) struct Message {pub name:*const c_char,pub signature:*const c_char,pub types:*const *const Interface}
unsafe impl Sync for Interface {} unsafe impl Sync for Message {}
#[repr(C)] #[derive(Clone,Copy)] pub(super) union Argument {pub i:i32,pub u:u32,pub s:*const c_char,pub o:*mut c_void,pub n:u32,pub a:*mut c_void,pub h:i32}
pub(super) type Proxy=*mut c_void;
pub(super) type Dispatcher=unsafe extern "C" fn(*const c_void,*mut c_void,u32,*const Message,*mut Argument)->c_int;
#[link(name="wayland-client")]
unsafe extern "C" {
 pub(super) static wl_registry_interface:Interface; pub(super) static wl_compositor_interface:Interface;
 pub(super) static wl_shm_interface:Interface; pub(super) static wl_shm_pool_interface:Interface;
 pub(super) static wl_surface_interface:Interface; pub(super) static wl_seat_interface:Interface;
 pub(super) static wl_pointer_interface:Interface; pub(super) static wl_keyboard_interface:Interface;
 pub(super) static wl_buffer_interface:Interface; pub(super) static wl_output_interface:Interface;
 pub(super) fn wl_display_connect_to_fd(fd:c_int)->Proxy;
 pub(super) fn wl_display_disconnect(display:Proxy);
 pub(super) fn wl_display_get_fd(display:Proxy)->c_int;
 pub(super) fn wl_display_flush(display:Proxy)->c_int;
 pub(super) fn wl_display_roundtrip(display:Proxy)->c_int;
 pub(super) fn wl_display_prepare_read(display:Proxy)->c_int;
 pub(super) fn wl_display_cancel_read(display:Proxy);
 pub(super) fn wl_display_read_events(display:Proxy)->c_int;
 pub(super) fn wl_display_dispatch_pending(display:Proxy)->c_int;
 pub(super) fn wl_proxy_get_version(proxy:Proxy)->u32;
 pub(super) fn wl_proxy_get_user_data(proxy:Proxy)->*mut c_void;
 pub(super) fn wl_proxy_add_dispatcher(proxy:Proxy,dispatcher:Option<Dispatcher>,implementation:*const c_void,data:*mut c_void)->c_int;
 pub(super) fn wl_proxy_destroy(proxy:Proxy);
 pub(super) fn wl_proxy_marshal_flags(proxy:Proxy,opcode:u32,interface:*const Interface,version:u32,flags:u32,...)->Proxy;
}
macro_rules! msg {($name:literal,$sig:literal)=>{Message{name:concat!($name,"\0").as_ptr().cast(),signature:concat!($sig,"\0").as_ptr().cast(),types:ptr::null()}};($name:literal,$sig:literal,$types:expr)=>{Message{name:concat!($name,"\0").as_ptr().cast(),signature:concat!($sig,"\0").as_ptr().cast(),types:$types.as_ptr()}};}
// Object-bearing argument tables must include the null entries for scalar arguments.
struct Types<const N:usize>([*const Interface;N]); unsafe impl<const N:usize> Sync for Types<N>{}
static KEYBOARD_TYPES:Types<1>=Types([unsafe{&raw const wl_keyboard_interface}]);
static CONTEXT_TYPES:Types<1>=Types([&CONTEXT]);
static PANEL_TYPES:Types<2>=Types([&PANEL_SURFACE,unsafe{&raw const wl_surface_interface}]);
static OUTPUT_TYPES:Types<2>=Types([unsafe{&raw const wl_output_interface},ptr::null()]);
static CONTEXT_REQUESTS:[Message;14]=[
 msg!("destroy",""),msg!("commit_string","us"),msg!("preedit_string","uss"),msg!("preedit_styling","uuu"),
 msg!("preedit_cursor","i"),msg!("delete_surrounding_text","iu"),msg!("cursor_position","ii"),msg!("modifiers_map","a"),
 msg!("keysym","uuuuu"),msg!("grab_keyboard","n",KEYBOARD_TYPES.0),msg!("key","uuuu"),msg!("modifiers","uuuuu"),msg!("language","us"),msg!("text_direction","uu")];
static CONTEXT_EVENTS:[Message;6]=[msg!("surrounding_text","suu"),msg!("reset",""),msg!("content_type","uu"),msg!("invoke_action","uu"),msg!("commit_state","u"),msg!("preferred_language","s")];
static METHOD_EVENTS:[Message;2]=[msg!("activate","n",CONTEXT_TYPES.0),msg!("deactivate","o",CONTEXT_TYPES.0)];
static PANEL_REQUESTS:[Message;1]=[msg!("get_input_panel_surface","no",PANEL_TYPES.0)];
static PANEL_SURFACE_REQUESTS:[Message;2]=[msg!("set_toplevel","ou",OUTPUT_TYPES.0),msg!("set_overlay_panel","")];
pub(super) static CONTEXT:Interface=Interface{name:c"zwp_input_method_context_v1".as_ptr(),version:1,method_count:14,methods:CONTEXT_REQUESTS.as_ptr(),event_count:6,events:CONTEXT_EVENTS.as_ptr()};
pub(super) static METHOD:Interface=Interface{name:c"zwp_input_method_v1".as_ptr(),version:1,method_count:0,methods:ptr::null(),event_count:2,events:METHOD_EVENTS.as_ptr()};
pub(super) static PANEL:Interface=Interface{name:c"zwp_input_panel_v1".as_ptr(),version:1,method_count:1,methods:PANEL_REQUESTS.as_ptr(),event_count:0,events:ptr::null()};
pub(super) static PANEL_SURFACE:Interface=Interface{name:c"zwp_input_panel_surface_v1".as_ptr(),version:1,method_count:2,methods:PANEL_SURFACE_REQUESTS.as_ptr(),event_count:0,events:ptr::null()};

pub(super) unsafe fn request0(proxy:Proxy,opcode:u32){if !proxy.is_null(){wl_proxy_marshal_flags(proxy,opcode,ptr::null(),wl_proxy_get_version(proxy),0);}}
pub(super) unsafe fn destroy(proxy:&mut Proxy,opcode:Option<u32>){if !proxy.is_null(){if let Some(opcode)=opcode{wl_proxy_marshal_flags(*proxy,opcode,ptr::null(),wl_proxy_get_version(*proxy),1);}else{wl_proxy_destroy(*proxy);} *proxy=ptr::null_mut();}}

#[link(name="xkbcommon")]
unsafe extern "C" {
 pub(super) fn xkb_context_new(flags:u32)->Proxy; pub(super) fn xkb_context_unref(p:Proxy);
 pub(super) fn xkb_keymap_new_from_string(context:Proxy,string:*const c_char,format:u32,flags:u32)->Proxy;
 pub(super) fn xkb_keymap_unref(p:Proxy); pub(super) fn xkb_state_new(keymap:Proxy)->Proxy; pub(super) fn xkb_state_unref(p:Proxy);
 pub(super) fn xkb_state_update_mask(state:Proxy,depressed:u32,latched:u32,locked:u32,layout_depressed:u32,layout_latched:u32,layout_locked:u32)->u32;
 pub(super) fn xkb_keymap_mod_get_index(map:Proxy,name:*const c_char)->u32;
 pub(super) fn xkb_keymap_num_mods(map:Proxy)->u32; pub(super) fn xkb_keymap_mod_get_name(map:Proxy,index:u32)->*const c_char;
 pub(super) fn xkb_state_mod_index_is_active(state:Proxy,index:u32,components:u32)->c_int;
 pub(super) fn xkb_state_mod_name_is_active(state:Proxy,name:*const c_char,components:u32)->c_int;
 pub(super) fn xkb_state_serialize_mods(state:Proxy,components:u32)->u32;
 pub(super) fn xkb_state_key_get_one_sym(state:Proxy,key:u32)->u32; pub(super) fn xkb_state_key_get_layout(state:Proxy,key:u32)->u32;
 pub(super) fn xkb_keymap_key_get_syms_by_level(map:Proxy,key:u32,layout:u32,level:u32,syms:*mut *const u32)->c_int;
 pub(super) fn xkb_keysym_get_name(sym:u32,buffer:*mut c_char,size:usize)->c_int;
}
