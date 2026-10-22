//! Dynamically loaded Lilv keeps the mixer usable when LV2 is not installed.
//! Plugin UI libraries are never opened.
mod host;
#[cfg(test)]
mod compatibility_tests;
use anyhow::{bail, Context, Result};
use libloading::Library;
use alixer_core::Insert;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::{c_char,c_void,CStr,CString}, ptr, sync::{Arc, atomic::{AtomicU32, Ordering}}};

type Ptr = *mut c_void;
const LV2: &str = "http://lv2plug.in/ns/lv2core#";
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Port { pub index:u32,pub symbol:String,pub name:String,pub input:bool,pub audio:bool,pub control:bool,pub optional:bool,pub default:f32,pub min:f32,pub max:f32,pub toggle:bool,pub integer:bool,pub latency:bool, #[serde(default)] pub logarithmic:bool, #[serde(default)] pub unit:String, #[serde(default)] pub choices:Vec<(f32,String)> }
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Plugin { pub uri:String,pub name:String,pub ports:Vec<Port>,pub required:Vec<String>, pub unsupported:Vec<String> }
impl Plugin {
    pub fn compatible(&self, channels:usize)->bool {
        self.required.iter().all(|r|host::supported(r)) && self.ports.iter().filter(|p|p.audio&&p.input).count()==channels && self.ports.iter().filter(|p|p.audio&&!p.input).count()==channels
    }
}
pub struct World { lib:Library, world:Ptr }
impl World {
    pub fn open()->Result<Self> {
        // SAFETY: all symbols and argument layouts below follow Lilv's public C ABI.
        unsafe {
            let lib=Library::new(std::env::var("ALIXER_LILV_LIBRARY").or_else(|_|std::env::var("LIXER_LILV_LIBRARY")).unwrap_or_else(|_|"liblilv-0.so.0".into())).context("LV2 unavailable: install Lilv (liblilv-0.so.0)")?;
            let new=*lib.get::<unsafe extern "C" fn()->Ptr>(b"lilv_world_new\0")?;
            let world=new(); if world.is_null(){bail!("Lilv could not create a world");}
            let this=Self{lib,world}; (this.f::<unsafe extern "C" fn(Ptr)>(b"lilv_world_load_all\0")?)(world); Ok(this)
        }
    }
    unsafe fn f<T:Copy>(&self,name:&[u8])->Result<T> { Ok(unsafe{*self.lib.get::<T>(name)?}) }
    unsafe fn text(&self,node:Ptr,uri:bool)->Result<String> {
        if node.is_null(){return Ok(String::new());}
        let fun=if uri { b"lilv_node_as_uri\0".as_slice() } else { b"lilv_node_as_string\0".as_slice() };
        let s=unsafe{(self.f::<unsafe extern "C" fn(Ptr)->*const c_char>(fun)?)(node)};
        Ok(if s.is_null(){String::new()}else{unsafe{CStr::from_ptr(s)}.to_string_lossy().into_owned()})
    }
    unsafe fn node(&self,uri:&str)->Result<Ptr> { let s=CString::new(uri)?; Ok(unsafe{(self.f::<unsafe extern "C" fn(Ptr,*const c_char)->Ptr>(b"lilv_new_uri\0")?)(self.world,s.as_ptr())}) }
    unsafe fn free_node(&self,node:Ptr) { if let Ok(f)=unsafe{self.f::<unsafe extern "C" fn(Ptr)>(b"lilv_node_free\0")}{unsafe{f(node)}} }
    unsafe fn is_a(&self,plugin:Ptr,port:Ptr,class:&str)->Result<bool> {
        let node=unsafe{self.node(class)?};
        let result=unsafe{(self.f::<unsafe extern "C" fn(Ptr,Ptr,Ptr)->bool>(b"lilv_port_is_a\0")?)(plugin,port,node)};
        unsafe{self.free_node(node)}; Ok(result)
    }
    unsafe fn property(&self,plugin:Ptr,port:Ptr,class:&str)->Result<bool> {
        let node=unsafe{self.node(class)?};
        let result=unsafe{(self.f::<unsafe extern "C" fn(Ptr,Ptr,Ptr)->bool>(b"lilv_port_has_property\0")?)(plugin,port,node)};
        unsafe{self.free_node(node)}; Ok(result)
    }
    unsafe fn control_metadata(&self, plugin:Ptr, port:Ptr)->Result<(String,Vec<(f32,String)>)> {
        unsafe {
            let predicate=self.node("http://lv2plug.in/ns/extensions/units#unit")?;
            let unit=(self.f::<unsafe extern "C" fn(Ptr,Ptr,Ptr)->Ptr>(b"lilv_port_get\0")?)(plugin,port,predicate);
            self.free_node(predicate);
            let mut unit_name=String::new();
            if !unit.is_null() {
                let symbol=self.node("http://lv2plug.in/ns/extensions/units#symbol")?;
                let value=(self.f::<unsafe extern "C" fn(Ptr,Ptr,Ptr,Ptr)->Ptr>(b"lilv_world_get\0")?)(self.world,unit,symbol,ptr::null_mut());
                unit_name=self.text(value,false)?;
                self.free_node(value);self.free_node(symbol);self.free_node(unit);
            }
            let points=(self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_port_get_scale_points\0")?)(plugin,port);
            let mut choices=vec![];
            if !points.is_null() {
                let mut it=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_scale_points_begin\0")?)(points);
                while !(self.f::<unsafe extern "C" fn(Ptr,Ptr)->bool>(b"lilv_scale_points_is_end\0")?)(points,it) {
                    let point=(self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_scale_points_get\0")?)(points,it);
                    let value=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_scale_point_get_value\0")?)(point);
                    let label=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_scale_point_get_label\0")?)(point);
                    choices.push(((self.f::<unsafe extern "C" fn(Ptr)->f32>(b"lilv_node_as_float\0")?)(value),self.text(label,false)?));
                    it=(self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_scale_points_next\0")?)(points,it);
                }
                (self.f::<unsafe extern "C" fn(Ptr)>(b"lilv_scale_points_free\0")?)(points);
            }
            choices.sort_by(|a,b|a.0.total_cmp(&b.0));
            Ok((unit_name,choices))
        }
    }
    pub fn plugins(&self)->Result<Vec<Plugin>> {
        unsafe {
            let plugins=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_world_get_all_plugins\0")?)(self.world);
            let begin=self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_plugins_begin\0")?;
            let end=self.f::<unsafe extern "C" fn(Ptr,Ptr)->bool>(b"lilv_plugins_is_end\0")?;
            let next=self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_plugins_next\0")?;
            let get=self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_plugins_get\0")?;
            let mut it=begin(plugins); let mut result=vec![];
            while !end(plugins,it){result.push(self.describe(get(plugins,it))?);it=next(plugins,it);}
            result.sort_by(|a,b|a.name.cmp(&b.name)); Ok(result)
        }
    }
    unsafe fn find(&self,uri:&str)->Result<Ptr> {
        unsafe {
            let plugins=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_world_get_all_plugins\0")?)(self.world);
            let uri=self.node(uri)?;
            let plugin=(self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_plugins_get_by_uri\0")?)(plugins,uri);
            self.free_node(uri);
            if plugin.is_null(){bail!("Plugin is not installed");} Ok(plugin)
        }
    }
    unsafe fn describe(&self,plugin:Ptr)->Result<Plugin> {
        unsafe {
            let uri=self.text((self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_plugin_get_uri\0")?)(plugin),true)?;
            let node=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_plugin_get_name\0")?)(plugin);
            let name=self.text(node,false)?; self.free_node(node);
            let count=(self.f::<unsafe extern "C" fn(Ptr)->u32>(b"lilv_plugin_get_num_ports\0")?)(plugin);
            let mut ports=vec![];let mut unsupported=vec![];
            for index in 0..count {
                let port=(self.f::<unsafe extern "C" fn(Ptr,u32)->Ptr>(b"lilv_plugin_get_port_by_index\0")?)(plugin,index);
                let audio=self.is_a(plugin,port,&format!("{LV2}AudioPort"))?;
                let control=self.is_a(plugin,port,&format!("{LV2}ControlPort"))?;
                let optional=self.property(plugin,port,&format!("{LV2}connectionOptional"))?;
                let input=self.is_a(plugin,port,&format!("{LV2}InputPort"))?;
                let symbol=self.text((self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_port_get_symbol\0")?)(plugin,port),false)?;
                let node=(self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_port_get_name\0")?)(plugin,port);
                let port_name=self.text(node,false)?; self.free_node(node);
                if !audio&&!control&&!optional { unsupported.push(format!("Unsupported port type: {port_name} ({symbol}, index {index})")); }
                let(mut default,mut min,mut max)=(ptr::null_mut(),ptr::null_mut(),ptr::null_mut());
                (self.f::<unsafe extern "C" fn(Ptr,Ptr,*mut Ptr,*mut Ptr,*mut Ptr)>(b"lilv_port_get_range\0")?)(plugin,port,&mut default,&mut min,&mut max);
                let number=|n:Ptr,fallback:f32| ->Result<f32>{ if n.is_null(){Ok(fallback)}else{Ok((self.f::<unsafe extern "C" fn(Ptr)->f32>(b"lilv_node_as_float\0")?)(n))} };
                let (d,lo,hi)=(number(default,0.0)?,number(min,0.0)?,number(max,1.0)?);
                for n in [default,min,max]{self.free_node(n);}
                let designation=self.node(&format!("{LV2}designation"))?;
                let value=(self.f::<unsafe extern "C" fn(Ptr,Ptr,Ptr)->Ptr>(b"lilv_port_get\0")?)(plugin,port,designation);
                let latency=self.text(value,true)?==format!("{LV2}latency");self.free_node(value);self.free_node(designation);
                let (unit,choices)=if control {self.control_metadata(plugin,port)?}else{Default::default()};
                ports.push(Port {unit,choices,logarithmic:self.property(plugin,port,"http://lv2plug.in/ns/ext/port-props#logarithmic")?,index,symbol,name:port_name,input,audio,control,optional,default:d,min:lo,max:hi,toggle:self.property(plugin,port,&format!("{LV2}toggled"))?,integer:self.property(plugin,port,&format!("{LV2}integer"))?,latency:control&&latency});
            }
            let nodes=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_plugin_get_required_features\0")?)(plugin);
            let mut required=unsupported;
            if !nodes.is_null(){
                let mut it=(self.f::<unsafe extern "C" fn(Ptr)->Ptr>(b"lilv_nodes_begin\0")?)(nodes);
                while !(self.f::<unsafe extern "C" fn(Ptr,Ptr)->bool>(b"lilv_nodes_is_end\0")?)(nodes,it){
                    required.push(self.text((self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_nodes_get\0")?)(nodes,it),true)?);
                    it=(self.f::<unsafe extern "C" fn(Ptr,Ptr)->Ptr>(b"lilv_nodes_next\0")?)(nodes,it);
                }
                (self.f::<unsafe extern "C" fn(Ptr)>(b"lilv_nodes_free\0")?)(nodes);
            }
            let unsupported=required.iter().filter(|r|!host::supported(r)).cloned().collect();
            Ok(Plugin{uri,name,ports,required,unsupported})
        }
    }
}
impl Drop for World { fn drop(&mut self){ unsafe {if let Ok(f)=self.f::<unsafe extern "C" fn(Ptr)>(b"lilv_world_free\0"){f(self.world)}} } }
#[repr(C)]
struct Descriptor {
    uri:*const c_char,
    instantiate:unsafe extern "C" fn(*const Descriptor,f64,*const c_char,*const Ptr)->Ptr,
    connect:unsafe extern "C" fn(Ptr,u32,Ptr),
    activate:Option<unsafe extern "C" fn(Ptr)>,
    run:unsafe extern "C" fn(Ptr,u32),
    deactivate:Option<unsafe extern "C" fn(Ptr)>,
    cleanup:unsafe extern "C" fn(Ptr),
    extension:Option<unsafe extern "C" fn(*const c_char)->*const c_void>,
}
#[repr(C)]
struct NativeInstance { descriptor:*const Descriptor,handle:Ptr,pimpl:Ptr }
pub struct Instance {
    pub id:alixer_core::Id,
    host:Box<host::Host>,
    plugin_ptr:Ptr,
    world:World, raw:*mut NativeInstance, pub plugin:Plugin,
    buffers:Vec<Vec<f32>>, controls:Vec<f32>,
    pub parameters:BTreeMap<String,Arc<AtomicU32>>,
    pub meters:BTreeMap<String,Arc<AtomicU32>>,
}
impl Instance {
    pub fn new(insert:&Insert,channels:usize,rate:f64,max_frames:usize)->Result<Self>{
        let world=World::open()?;
        unsafe {
            let plugin=world.find(&insert.uri)?; let info=world.describe(plugin)?;
            check_compatible(&info,channels)?;
            let host=host::Host::new(rate,max_frames);
            let raw=(world.f::<unsafe extern "C" fn(Ptr,f64,*const *const c_void)->*mut NativeInstance>(b"lilv_plugin_instantiate\0")?)(plugin,rate,host.pointers().cast());
            if raw.is_null(){bail!("Plugin instantiation failed");}
            let mut this=Self{id:insert.id,host,plugin_ptr:plugin,world,raw,controls:vec![0.0;info.ports.len()],buffers:info.ports.iter().map(|_|vec![0.0;max_frames]).collect(),parameters:BTreeMap::new(),meters:BTreeMap::new(),plugin:info};
            for port in &this.plugin.ports {
                let i=port.index as usize;
                let location=if port.audio{this.buffers[i].as_mut_ptr()}else if port.control{
                    let value=insert.controls.get(&port.symbol).copied().unwrap_or(port.default).clamp(port.min,port.max);
                    this.controls[i]=value;
                    if port.input{this.parameters.insert(port.symbol.clone(),Arc::new(AtomicU32::new(value.to_bits())));}
                    if !port.input && !port.latency {this.meters.insert(port.symbol.clone(),Arc::new(AtomicU32::new(value.to_bits())));}
                    &mut this.controls[i]
                }else{
                    // LV2 requires explicitly connecting unsupported optional
                    // ports to NULL, never to an undersized control buffer.
                    ptr::null_mut()
                };
                ((*(*raw).descriptor).connect)((*raw).handle,port.index,location.cast());
            }
            if let Some(path)=&insert.state { this.restore_state(std::path::Path::new(path))?; }
            let worker=(*(*raw).descriptor).extension.map(|extension|extension(c"http://lv2plug.in/ns/ext/worker#interface".as_ptr())).unwrap_or(std::ptr::null());
            this.host.start_worker((*raw).handle,worker.cast());
            if let Some(activate)=(*(*raw).descriptor).activate{activate((*raw).handle);}
            Ok(this)
        }
    }
    pub fn run(&mut self,channels:&mut [Vec<f32>],frames:usize){
        let mut input=0;
        for port in &self.plugin.ports {
            let i=port.index as usize;
            if port.audio&&port.input {self.buffers[i][..frames].copy_from_slice(&channels[input][..frames]);input+=1;}
            if port.control&&port.input {if let Some(value)=self.parameters.get(&port.symbol){self.controls[i]=f32::from_bits(value.load(Ordering::Relaxed));}}
        }
        self.host.responses();
        // SAFETY: instance and all connected buffers remain valid; only the audio thread calls run.
        unsafe {((*(*self.raw).descriptor).run)((*self.raw).handle,frames as u32);}
        self.host.end_run();
        for port in &self.plugin.ports {if let Some(meter)=self.meters.get(&port.symbol){let value=self.controls[port.index as usize];meter.store(if value.is_finite(){value.to_bits()}else{0.0_f32.to_bits()},Ordering::Relaxed);}}
        let mut output=0;
        for port in &self.plugin.ports {if port.audio&&!port.input{channels[output][..frames].copy_from_slice(&self.buffers[port.index as usize][..frames]);output+=1;}}
    }
    pub fn latency(&self)->usize{self.plugin.ports.iter().filter(|p|p.latency).map(|p|self.controls[p.index as usize].max(0.0) as usize).max().unwrap_or(0)}
}
impl Drop for Instance {fn drop(&mut self){self.host.stop();unsafe{
    if let Some(deactivate)=(*(*self.raw).descriptor).deactivate{deactivate((*self.raw).handle);}
    if let Ok(f)=self.world.f::<unsafe extern "C" fn(*mut NativeInstance)>(b"lilv_instance_free\0"){f(self.raw);}
}}}
pub fn catalog()->Result<Vec<Plugin>>{World::open()?.plugins()}
pub fn validate(insert:&Insert,channels:usize)->Result<Plugin>{
    let world=World::open()?;
    let p=unsafe{world.describe(world.find(&insert.uri)?)?};
    check_compatible(&p,channels)?;
    Ok(p)
}
fn check_compatible(plugin:&Plugin,channels:usize)->Result<()> {
    let unsupported:Vec<_>=plugin.required.iter().filter(|r|!host::supported(r)).map(String::as_str).collect();
    if !unsupported.is_empty(){bail!("{} is not supported by this LV2 host: {}",plugin.name,unsupported.join("; "));}
    if !plugin.compatible(channels){
        let inputs=plugin.ports.iter().filter(|p|p.audio&&p.input).count();
        let outputs=plugin.ports.iter().filter(|p|p.audio&&!p.input).count();
        bail!("{} has {inputs} audio inputs and {outputs} outputs; this strip requires {channels} of each",plugin.name);
    }
    Ok(())
}

impl Instance {
    /// The caller must have stopped processing this instance before checkpointing.
    pub fn save_state(&mut self,dir:&std::path::Path)->Result<String>{
        self.host.stop();self.host.responses();
        std::fs::create_dir_all(dir)?;
        let directory=CString::new(dir.as_os_str().as_encoded_bytes())?;
        unsafe {
            let state=(self.world.f::<unsafe extern "C" fn(Ptr,*mut NativeInstance,*mut host::Map,*const c_char,*const c_char,*const c_char,*const c_char,Option<unsafe extern "C" fn(*const c_char,Ptr,*mut u32,*mut u32)->*const c_void>,Ptr,u32,*const *const host::Feature)->Ptr>(b"lilv_state_new_from_instance\0")?)(self.plugin_ptr,self.raw,&mut self.host.map,ptr::null(),directory.as_ptr(),ptr::null(),directory.as_ptr(),None,ptr::null_mut(),3,self.host.pointers());
            if state.is_null(){bail!("Could not capture LV2 state");}
            let save=self.world.f::<unsafe extern "C" fn(Ptr,*mut host::Map,*mut host::Unmap,Ptr,*const c_char,*const c_char,*const c_char)->i32>(b"lilv_state_save\0")?;
            let result=save(self.world.world,&mut self.host.map,&mut self.host.unmap,state,ptr::null(),directory.as_ptr(),c"state.ttl".as_ptr());
            (self.world.f::<unsafe extern "C" fn(Ptr)>(b"lilv_state_free\0")?)(state);
            if result!=0{bail!("LV2 state save failed: {result}");}
        }
        Ok(dir.join("state.ttl").to_string_lossy().into_owned())
    }
    fn restore_state(&mut self,path:&std::path::Path)->Result<()>{
        let path=CString::new(path.as_os_str().as_encoded_bytes())?;
        unsafe {
            let state=(self.world.f::<unsafe extern "C" fn(Ptr,*mut host::Map,Ptr,*const c_char)->Ptr>(b"lilv_state_new_from_file\0")?)(self.world.world,&mut self.host.map,ptr::null_mut(),path.as_ptr());
            if state.is_null(){bail!("Could not load LV2 state");}
            (self.world.f::<unsafe extern "C" fn(Ptr,*mut NativeInstance,Option<unsafe extern "C" fn(*const c_char,Ptr,*const c_void,u32,u32)>,Ptr,u32,*const *const host::Feature)>(b"lilv_state_restore\0")?)(state,self.raw,None,ptr::null_mut(),0,self.host.pointers());
            (self.world.f::<unsafe extern "C" fn(Ptr)>(b"lilv_state_free\0")?)(state);
        }Ok(())
    }
}
