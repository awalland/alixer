//! Stable feature storage and bounded LV2 worker queues.
use std::{ffi::{c_char,c_void,CStr,CString},ptr,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicPtr,Ordering}},thread::JoinHandle};
use crossbeam_queue::ArrayQueue;
type Ptr=*mut c_void;
#[repr(C)] pub struct Feature{pub uri:*const c_char,pub data:Ptr}
#[repr(C)] pub struct Map{pub handle:Ptr,pub map:unsafe extern "C" fn(Ptr,*const c_char)->u32}
#[repr(C)] pub struct Unmap{pub handle:Ptr,pub unmap:unsafe extern "C" fn(Ptr,u32)->*const c_char}
#[repr(C)] struct Schedule{handle:Ptr,schedule:unsafe extern "C" fn(Ptr,u32,*const c_void)->i32}
#[repr(C)] struct OptionValue{context:u32,subject:u32,key:u32,size:u32,type_:u32,value:*const c_void}
#[repr(C)] pub struct WorkerInterface{
    pub work:unsafe extern "C" fn(Ptr,unsafe extern "C" fn(Ptr,u32,*const c_void)->i32,Ptr,u32,*const c_void)->i32,
    pub response:unsafe extern "C" fn(Ptr,u32,*const c_void)->i32,
    pub end:Option<unsafe extern "C" fn(Ptr)->i32>,
}
struct Item{size:u32,bytes:[u8;4096]}
struct Queues{jobs:ArrayQueue<Item>,responses:ArrayQueue<Item>,stop:AtomicBool,handle:AtomicPtr<c_void>,interface:AtomicPtr<WorkerInterface>}
pub struct Host{
    uris:Mutex<Vec<CString>>,
    pub map:Map,pub unmap:Unmap,schedule:Schedule,
    options:Vec<OptionValue>,rate:Box<f64>,min:Box<i32>,max:Box<i32>,nominal:Box<i32>,
    features:Vec<Feature>,pointers:Vec<*const Feature>,
    queues:Arc<Queues>,thread:Option<JoinHandle<()>>,
}
pub fn supported(uri:&str)->bool{
    matches!(uri,"http://lv2plug.in/ns/ext/urid#map"|"http://lv2plug.in/ns/ext/urid#unmap"|"http://lv2plug.in/ns/ext/worker#schedule"|"http://lv2plug.in/ns/ext/options#options"|"http://lv2plug.in/ns/ext/buf-size#boundedBlockLength")
}
unsafe extern "C" fn map_uri(handle:Ptr,uri:*const c_char)->u32{
    if uri.is_null(){return 0;}
    // Mapping normally occurs during instantiation/state operations. A plugin that
    // calls map during run must not claim hard real-time capability.
    let host=unsafe{&*handle.cast::<Host>()};
    let Ok(mut uris)=host.uris.lock()else{return 0};
    let value=unsafe{CStr::from_ptr(uri)};
    if let Some(i)=uris.iter().position(|u|u.as_c_str()==value){return i as u32+1;}
    uris.push(value.to_owned());uris.len()as u32
}
unsafe extern "C" fn unmap_uri(handle:Ptr,id:u32)->*const c_char{
    let host=unsafe{&*handle.cast::<Host>()};
    let Ok(uris)=host.uris.lock()else{return ptr::null()};
    if id==0{return ptr::null();}uris.get(id as usize-1).map_or(ptr::null(),|u|u.as_ptr())
}
fn enqueue(queue:&ArrayQueue<Item>,size:u32,data:*const c_void)->i32{
    if size>4096|| (size>0&&data.is_null()){return 2;}
    let mut item=Item{size,bytes:[0;4096]};
    if size>0{unsafe{ptr::copy_nonoverlapping(data.cast::<u8>(),item.bytes.as_mut_ptr(),size as usize);}}
    if queue.push(item).is_ok(){0}else{1}
}
unsafe extern "C" fn schedule(handle:Ptr,size:u32,data:*const c_void)->i32{
    let host=unsafe{&*handle.cast::<Host>()};enqueue(&host.queues.jobs,size,data)
}
unsafe extern "C" fn respond(handle:Ptr,size:u32,data:*const c_void)->i32{
    let queues=unsafe{&*handle.cast::<Queues>()};enqueue(&queues.responses,size,data)
}
impl Host{
    pub fn new(rate:f64,max:usize)->Box<Self>{
        let mut host=Box::new(Self{uris:Mutex::new(vec![]),map:Map{handle:ptr::null_mut(),map:map_uri},unmap:Unmap{handle:ptr::null_mut(),unmap:unmap_uri},schedule:Schedule{handle:ptr::null_mut(),schedule},options:vec![],rate:Box::new(rate),min:Box::new(1),max:Box::new(max as i32),nominal:Box::new(512),features:vec![],pointers:vec![],queues:Arc::new(Queues{jobs:ArrayQueue::new(8),responses:ArrayQueue::new(8),stop:AtomicBool::new(false),handle:AtomicPtr::new(ptr::null_mut()),interface:AtomicPtr::new(ptr::null_mut())}),thread:None});
        let handle=(&mut *host as *mut Host).cast();
        host.map.handle=handle;host.unmap.handle=handle;host.schedule.handle=handle;
        let float=host.id("http://lv2plug.in/ns/ext/atom#Double");let int=host.id("http://lv2plug.in/ns/ext/atom#Int");
        host.options=vec![
            OptionValue{context:0,subject:0,key:host.id("http://lv2plug.in/ns/ext/parameters#sampleRate"),size:8,type_:float,value:(&*host.rate as *const f64).cast()},
            OptionValue{context:0,subject:0,key:host.id("http://lv2plug.in/ns/ext/buf-size#minBlockLength"),size:4,type_:int,value:(&*host.min as *const i32).cast()},
            OptionValue{context:0,subject:0,key:host.id("http://lv2plug.in/ns/ext/buf-size#maxBlockLength"),size:4,type_:int,value:(&*host.max as *const i32).cast()},
            OptionValue{context:0,subject:0,key:host.id("http://lv2plug.in/ns/ext/buf-size#nominalBlockLength"),size:4,type_:int,value:(&*host.nominal as *const i32).cast()},
            OptionValue{context:0,subject:0,key:0,size:0,type_:0,value:ptr::null()},
        ];
        host.features=vec![
            Feature{uri:c"http://lv2plug.in/ns/ext/urid#map".as_ptr(),data:(&mut host.map as *mut Map).cast()},
            Feature{uri:c"http://lv2plug.in/ns/ext/urid#unmap".as_ptr(),data:(&mut host.unmap as *mut Unmap).cast()},
            Feature{uri:c"http://lv2plug.in/ns/ext/worker#schedule".as_ptr(),data:(&mut host.schedule as *mut Schedule).cast()},
            Feature{uri:c"http://lv2plug.in/ns/ext/options#options".as_ptr(),data:host.options.as_mut_ptr().cast()},
            Feature{uri:c"http://lv2plug.in/ns/ext/buf-size#boundedBlockLength".as_ptr(),data:ptr::null_mut()},
        ];
        host.pointers=host.features.iter().map(|f|f as *const Feature).chain(std::iter::once(ptr::null())).collect();host
    }
    pub fn pointers(&self)->*const *const Feature{self.pointers.as_ptr()}
    pub fn id(&self,uri:&str)->u32{let s=CString::new(uri).expect("static URI");unsafe{map_uri(self.map.handle,s.as_ptr())}}
    /// Called after instantiation; plugin owns the interface and remains alive until stop().
    pub unsafe fn start_worker(&mut self,handle:Ptr,interface:*const WorkerInterface){
        if interface.is_null(){return;}
        self.queues.handle.store(handle,Ordering::Release);self.queues.interface.store(interface.cast_mut(),Ordering::Release);
        let q=self.queues.clone();
        self.thread=Some(std::thread::spawn(move||{
            while !q.stop.load(Ordering::Acquire){
                if let Some(item)=q.jobs.pop(){
                    let interface=q.interface.load(Ordering::Acquire);let handle=q.handle.load(Ordering::Acquire);
                    // SAFETY: stop joins this worker before the plugin is freed.
                    unsafe{((*interface).work)(handle,respond,Arc::as_ptr(&q).cast_mut().cast(),item.size,item.bytes.as_ptr().cast());}
                }else{std::thread::sleep(std::time::Duration::from_millis(2));}
            }
        }));
    }
    pub fn responses(&self){
        let interface=self.queues.interface.load(Ordering::Acquire);
        if interface.is_null(){return;}
        while let Some(item)=self.queues.responses.pop(){
            unsafe{((*interface).response)(self.queues.handle.load(Ordering::Acquire),item.size,item.bytes.as_ptr().cast());}
        }
    }
    pub fn end_run(&self){
        let interface=self.queues.interface.load(Ordering::Acquire);
        if !interface.is_null(){unsafe{if let Some(end)=(*interface).end{end(self.queues.handle.load(Ordering::Acquire));}}}
    }
    pub fn stop(&mut self){self.queues.stop.store(true,Ordering::Release);if let Some(thread)=self.thread.take(){let _=thread.join();}}
}
impl Drop for Host{fn drop(&mut self){self.stop();}}
