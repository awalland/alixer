#![allow(unsafe_op_in_unsafe_fn)]
use std::ffi::{c_char,c_void,CStr};
type Ptr=*mut c_void;
#[repr(C)]pub struct Feature{uri:*const c_char,data:Ptr}
#[repr(C)]struct Map{handle:Ptr,map:unsafe extern "C" fn(Ptr,*const c_char)->u32}
#[repr(C)]struct Schedule{handle:Ptr,schedule:unsafe extern "C" fn(Ptr,u32,*const c_void)->i32}
#[repr(C)]pub struct Descriptor{
 uri:*const c_char,
 instantiate:unsafe extern "C" fn(*const Descriptor,f64,*const c_char,*const *const Feature)->Ptr,
 connect:unsafe extern "C" fn(Ptr,u32,Ptr),
 activate:Option<unsafe extern "C" fn(Ptr)>,
 run:unsafe extern "C" fn(Ptr,u32),
 deactivate:Option<unsafe extern "C" fn(Ptr)>,
 cleanup:unsafe extern "C" fn(Ptr),
 extension:unsafe extern "C" fn(*const c_char)->*const c_void,
}
unsafe impl Sync for Descriptor{}
struct State{ports:[*mut f32;6],connected:u32,schedule:*const Schedule,worker:f32,key:u32,float:u32}
unsafe extern "C" fn instantiate(_: *const Descriptor,_:f64,_:*const c_char,features:*const *const Feature)->Ptr{
 let mut map=std::ptr::null::<Map>();let mut schedule=std::ptr::null::<Schedule>();let mut i=0;
 while !(*features.add(i)).is_null(){let f=&**features.add(i);let uri=CStr::from_ptr(f.uri).to_bytes();
 if uri==b"http://lv2plug.in/ns/ext/urid#map"{map=f.data.cast();}if uri==b"http://lv2plug.in/ns/ext/worker#schedule"{schedule=f.data.cast();}i+=1;}
 if map.is_null()||schedule.is_null(){return std::ptr::null_mut();}
 Box::into_raw(Box::new(State{ports:[std::ptr::null_mut();6],connected:0,schedule,worker:0.0,key:((*map).map)((*map).handle,c"urn:alixer:test:worker".as_ptr()),float:((*map).map)((*map).handle,c"http://lv2plug.in/ns/ext/atom#Float".as_ptr())})).cast()
}
unsafe extern "C" fn connect(h:Ptr,index:u32,data:Ptr){let s=&mut*h.cast::<State>();s.ports[index as usize]=data.cast();s.connected|=1<<index;}
unsafe extern "C" fn run(h:Ptr,n:u32){let s=&mut *h.cast::<State>();let value=2.0f32;((*s.schedule).schedule)((*s.schedule).handle,4,(&value as *const f32).cast());
 assert_eq!(s.connected,0b111111,"Every port must be explicitly connected");
 assert!(s.ports[4].is_null()&&s.ports[5].is_null(),"Unsupported optional ports must be NULL");
 for i in 0..n as usize{*s.ports[1].add(i)=*s.ports[0].add(i)* *s.ports[2]*(1.0+s.worker);}*s.ports[3]=s.worker;}
unsafe extern "C" fn cleanup(h:Ptr){drop(Box::from_raw(h.cast::<State>()));}
#[repr(C)]struct Worker{
 work:unsafe extern "C" fn(Ptr,unsafe extern "C" fn(Ptr,u32,*const c_void)->i32,Ptr,u32,*const c_void)->i32,
 response:unsafe extern "C" fn(Ptr,u32,*const c_void)->i32,end:Option<unsafe extern "C" fn(Ptr)->i32>}
unsafe extern "C" fn work(_:Ptr,respond:unsafe extern "C" fn(Ptr,u32,*const c_void)->i32,handle:Ptr,size:u32,data:*const c_void)->i32{respond(handle,size,data)}
unsafe extern "C" fn response(h:Ptr,_:u32,data:*const c_void)->i32{(*h.cast::<State>()).worker=*data.cast::<f32>();0}
static WORKER:Worker=Worker{work,response,end:None};
type Store=unsafe extern "C" fn(Ptr,u32,*const c_void,usize,u32,u32)->u32;
type Retrieve=unsafe extern "C" fn(Ptr,u32,*mut usize,*mut u32,*mut u32)->*const c_void;
#[repr(C)]struct StateInterface{save:unsafe extern "C" fn(Ptr,Store,Ptr,u32,*const *const Feature)->u32,restore:unsafe extern "C" fn(Ptr,Retrieve,Ptr,u32,*const *const Feature)->u32}
unsafe extern "C" fn save(h:Ptr,store:Store,handle:Ptr,_:u32,_:*const *const Feature)->u32{let s=&*h.cast::<State>();store(handle,s.key,(&s.worker as *const f32).cast(),4,s.float,3)}
unsafe extern "C" fn restore(h:Ptr,get:Retrieve,handle:Ptr,_:u32,_:*const *const Feature)->u32{let s=&mut*h.cast::<State>();let(mut size,mut ty,mut flags)=(0,0,0);let value=get(handle,s.key,&mut size,&mut ty,&mut flags);if !value.is_null()&&size==4{s.worker=*value.cast::<f32>();}0}
static STATE:StateInterface=StateInterface{save,restore};
unsafe extern "C" fn extension(uri:*const c_char)->*const c_void{match CStr::from_ptr(uri).to_bytes(){b"http://lv2plug.in/ns/ext/state#interface"=>(&STATE as *const StateInterface).cast(),b"http://lv2plug.in/ns/ext/worker#interface"=>(&WORKER as *const Worker).cast(),_=>std::ptr::null()}}
static DESCRIPTOR:Descriptor=Descriptor{uri:c"urn:alixer:test:gain".as_ptr(),instantiate,connect,activate:None,run,deactivate:None,cleanup,extension};
#[unsafe(no_mangle)]
pub extern "C" fn lv2_descriptor(index:u32)->*const Descriptor{if index==0{&DESCRIPTOR}else{std::ptr::null()}}
