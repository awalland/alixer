//! Native PipeWire graph observer and controller. All proxies stay on one thread.
mod render;
use alixer_core::{reconcile::runtime_key, *};
use anyhow::Result;
use pipewire as pw;
use pw::{properties::properties, proxy::Listener, types::ObjectType};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Apply {
    pub scene: Scene,
    pub bindings: BTreeMap<String, Id>,
    pub suspended: BTreeSet<Id>,
    pub state_dir: PathBuf,
    pub output_controls: Vec<OutputControl>,
}
#[derive(Clone, Debug)]
pub struct OutputControl {
    pub key: String,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
    pub balance: Vec<f32>,
}

/// Only changes to the compiled audio graph require rebuilding it. Keep this
/// explicit so new presentation fields cannot accidentally interrupt playback.
fn topology_signature(apply: &Apply) -> String {
    let mut strips: Vec<_> = apply
        .scene
        .strips
        .iter()
        .map(|s| {
            (
                s.id,
                &s.kind,
                &s.output_key,
                &s.input_key,
                &s.channels,
                s.inserts
                    .iter()
                    .map(|i| (i.id, &i.uri, i.bypass))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    strips.sort_by_key(|s| s.0);
    let mut routes: Vec<_> = apply
        .scene
        .routes
        .iter()
        .map(|r| {
            (
                r.id,
                r.source,
                &r.destination,
                r.source_channel,
                r.target_channel,
            )
        })
        .collect();
    routes.sort_by_key(|r| r.0);
    serde_json::to_string(&(strips, routes, &apply.suspended))
        .expect("Audio topology contains only serializable identifiers and channel layouts")
}
#[derive(Clone, Debug)]
pub struct PluginState {
    pub strip: Id,
    pub insert: Id,
    pub path: String,
}
#[derive(Clone, Debug)]
pub struct Update {
    pub observation: Observation,
    pub status: String,
    pub peaks: Vec<(Id, f32)>,
    pub channel_peaks: Vec<(Id, Vec<f32>)>,
    pub plugin_meters: Vec<(Id, String, f32)>,
    pub states: Vec<PluginState>,
}
pub struct Backend {
    pub commands: mpsc::Sender<Apply>,
    pub updates: mpsc::Receiver<Update>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Vec<PluginState>>>,
}
impl Backend {
    pub fn start() -> Self {
        let (commands, rx) = mpsc::channel();
        let (tx, updates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            let rx = Rc::new(rx);
            loop {
                if stopping.load(Ordering::Acquire) {
                    return vec![];
                }
                match run(rx.clone(), tx.clone(), stopping.clone()) {
                    Ok(states) => return states,
                    Err(e) => {
                        let _ = tx.send(Update {
                            observation: Observation::default(),
                            status: format!("Disconnected: {e:#}"),
                            peaks: vec![],
                            channel_peaks: vec![],
                            plugin_meters: vec![],
                            states: vec![],
                        });
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        Self {
            commands,
            updates,
            stop,
            thread: Some(thread),
        }
    }
    pub fn shutdown(&mut self) -> Vec<PluginState> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
#[derive(Clone, Default)]
struct Raw {
    kind: String,
    props: BTreeMap<String, String>,
    volume: f32,
    mute: bool,
    channel_volumes: Vec<f32>,
    control_revision: u64,
}
#[derive(Default)]
struct Graph {
    objects: BTreeMap<u32, Raw>,
    epoch: String,
    targets: BTreeMap<u32, String>,
}
impl Graph {
    fn has_link(&self, source: u32, target: u32) -> bool {
        self.objects.values().any(|o| {
            o.kind == "Link"
                && o.props
                    .get("link.output.node")
                    .and_then(|v| v.parse::<u32>().ok())
                    == Some(source)
                && o.props
                    .get("link.input.node")
                    .and_then(|v| v.parse::<u32>().ok())
                    == Some(target)
        })
    }
    fn ports(&self, node: u32, output: bool) -> Vec<(u32, String, String)> {
        let mut ports: Vec<_> = self
            .objects
            .iter()
            .filter(|(_, o)| {
                o.kind == "Port"
                    && o.props.get("node.id").and_then(|s| s.parse::<u32>().ok()) == Some(node)
                    && o.props.get("port.direction").map(String::as_str)
                        == Some(if output { "out" } else { "in" })
            })
            .map(|(id, o)| {
                (
                    *id,
                    o.props.get("port.name").cloned().unwrap_or_default(),
                    o.props
                        .get("audio.channel")
                        .cloned()
                        .unwrap_or_else(|| "MONO".into()),
                )
            })
            .collect();
        // Global registry IDs can be reused in the opposite order after hotplug.
        // The node-local port ID defines the channel order.
        ports.sort_by_key(|(id, _, _)| {
            self.objects
                .get(id)
                .and_then(|o| o.props.get("port.id"))
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(*id)
        });
        ports
    }
    fn observation(&self) -> Observation {
        let mut result = Observation::default();
        for (&id, node) in &self.objects {
            if node.kind != "Node" {
                continue;
            }
            let name = node.props.get("node.name").cloned().unwrap_or_default();
            let class = node
                .props
                .get("media.class")
                .map(String::as_str)
                .unwrap_or_default();
            if name.starts_with("alixer.") || name.starts_with("lixer.") {
                continue;
            }
            if matches!(class, "Audio/Source" | "Audio/Source/Virtual") && !name.ends_with(".monitor")
                && node.props.get("device.class").map(String::as_str) != Some("monitor")
            {
                let ports = self.ports(id, true);
                if !ports.is_empty() {
                    result.inputs.push(Input {
                        key: name,
                        name: node.props.get("node.description").cloned().unwrap_or_default(),
                        channels: ports.iter().map(|p| p.2.clone()).collect(),
                    });
                }
            } else if class == "Audio/Sink" {
                let ports = self.ports(id, false);
                result.outputs.push(Output {
                    key: name,
                    name: node
                        .props
                        .get("node.description")
                        .cloned()
                        .unwrap_or_default(),
                    channels: ports.iter().map(|p| p.2.clone()).collect(),
                    volume: node.volume
                        * node
                            .channel_volumes
                            .iter()
                            .copied()
                            .reduce(f32::max)
                            .unwrap_or(1.0),
                    mute: node.mute,
                    channel_volumes: node
                        .channel_volumes
                        .iter()
                        .map(|v| v * node.volume)
                        .collect(),
                    control_revision: node.control_revision,
                });
            } else if class == "Stream/Output/Audio"
                && node.props.get("stream.monitor").map(String::as_str) != Some("true")
            {
                let ports = self.ports(id, true);
                if ports.is_empty() {
                    continue;
                }
                let mut targets = vec![];
                let mut managed = None;
                for link in self.objects.values().filter(|o| o.kind == "Link") {
                    if link
                        .props
                        .get("link.output.node")
                        .and_then(|s| s.parse::<u32>().ok())
                        != Some(id)
                    {
                        continue;
                    }
                    let Some(target) = link
                        .props
                        .get("link.input.node")
                        .and_then(|s| s.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    if let Some(sink) = self.objects.get(&target) {
                        let key = sink.props.get("node.name").cloned().unwrap_or_default();
                        managed = sink
                            .props
                            .get("alixer.strip")
                            .or_else(|| sink.props.get("lixer.strip"))
                            .and_then(|s| s.parse().ok())
                            .or(managed);
                        let source_port = link
                            .props
                            .get("link.output.port")
                            .and_then(|s| s.parse::<u32>().ok());
                        let target_port = link
                            .props
                            .get("link.input.port")
                            .and_then(|s| s.parse::<u32>().ok());
                        let sc = ports
                            .iter()
                            .position(|p| Some(p.0) == source_port)
                            .unwrap_or(0);
                        let tc = self
                            .ports(target, false)
                            .iter()
                            .position(|p| Some(p.0) == target_port)
                            .unwrap_or(0);
                        targets.push((key, sc, tc));
                    }
                }
                targets.sort();
                targets.dedup();
                if targets.is_empty() {
                    managed = self
                        .targets
                        .get(&id)
                        .and_then(|name| {
                            name.strip_prefix("alixer.input.")
                                .or_else(|| name.strip_prefix("lixer.input."))
                        })
                        .and_then(|id| id.parse().ok())
                        .or(managed);
                }
                result.streams.push(StreamObservation {
                    runtime: RuntimeId {
                        epoch: self.epoch.clone(),
                        serial: node
                            .props
                            .get("object.serial")
                            .cloned()
                            .unwrap_or_else(|| id.to_string()),
                    },
                    identity: Identity {
                        application: node
                            .props
                            .get("application.id")
                            .or_else(|| node.props.get("application.process.binary"))
                            .or_else(|| node.props.get("application.name"))
                            .cloned()
                            .unwrap_or_else(|| "Unknown application".into()),
                        name: node
                            .props
                            .get("media.name")
                            .or_else(|| node.props.get("node.description"))
                            .cloned()
                            .unwrap_or(name),
                        role: node.props.get("media.role").cloned().unwrap_or_default(),
                        channels: ports.iter().map(|p| p.2.clone()).collect(),
                    },
                    volume: node.volume
                        * node
                            .channel_volumes
                            .iter()
                            .copied()
                            .reduce(f32::max)
                            .unwrap_or(1.0),
                    mute: node.mute,
                    targets,
                    managed,
                });
            }
        }
        result
    }
}
fn run(
    rx: Rc<mpsc::Receiver<Apply>>,
    tx: mpsc::Sender<Update>,
    stop: Arc<AtomicBool>,
) -> Result<Vec<PluginState>> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(Some(
        properties! {"application.name"=>"Alixer","application.id"=>"org.alixer.Engine"},
    ))?;
    let registry = core.get_registry_rc()?;
    let graph = Rc::new(RefCell::new(Graph::default()));
    let proxies: Rc<RefCell<BTreeMap<u32, (pw::node::Node, Box<dyn Listener>)>>> = Rc::default();
    let metadata: Rc<RefCell<Option<pw::metadata::Metadata>>> = Rc::default();
    let metadata_listener: Rc<RefCell<Option<Box<dyn Listener>>>> = Rc::default();
    let ready = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let g = graph.clone();
    let f = failed.clone();
    let ml = mainloop.clone();
    let r = ready.clone();
    let _core_listener = core
        .add_listener_local()
        .info(move |info| {
            g.borrow_mut().epoch = info.cookie().to_string();
        })
        .done(move |_, _| {
            r.set(true);
        })
        .error(move |id, _, _, message| {
            eprintln!("PipeWire {id}: {message}");
            if id == 0 {
                f.set(true);
                ml.quit();
            }
        })
        .register();
    let reg = registry.clone();
    let g = graph.clone();
    let keep = proxies.clone();
    let md = metadata.clone();
    let md_listener = metadata_listener.clone();
    let _listener = registry
        .add_listener_local()
        .global(move |obj| {
            let Some(props) = obj.props else { return };
            let kind = match obj.type_ {
                ObjectType::Node => "Node",
                ObjectType::Port => "Port",
                ObjectType::Link => "Link",
                ObjectType::Metadata => "Metadata",
                _ => return,
            };
            let props: BTreeMap<String, String> =
                props.iter().map(|(k, v)| (k.into(), v.into())).collect();
            g.borrow_mut().objects.insert(
                obj.id,
                Raw {
                    kind: kind.into(),
                    props: props.clone(),
                    volume: 1.0,
                    mute: false,
                    channel_volumes: vec![],
                    control_revision: 0,
                },
            );
            if obj.type_ == ObjectType::Node {
                let Ok(node) = reg.bind::<pw::node::Node, _>(obj) else {
                    return;
                };
                let id = obj.id;
                let ginfo = g.clone();
                let gparam = g.clone();
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        if let Some(properties) = info.props() {
                            if let Some(raw) = ginfo.borrow_mut().objects.get_mut(&id) {
                                for (k, v) in properties.iter() {
                                    raw.props.insert(k.into(), v.into());
                                }
                            }
                        }
                    })
                    .param(move |_, _, _, _, pod| {
                        let Some(pod) = pod else { return };
                        if let Ok((_, pw::spa::pod::Value::Object(value))) =
                            pw::spa::pod::deserialize::PodDeserializer::deserialize_any_from(
                                pod.as_bytes(),
                            )
                        {
                            if let Some(raw) = gparam.borrow_mut().objects.get_mut(&id) {
                                raw.control_revision = raw.control_revision.wrapping_add(1);
                                for prop in value.properties {
                                    match (prop.key, prop.value) {
                                        (
                                            pw::spa::sys::SPA_PROP_volume,
                                            pw::spa::pod::Value::Float(v),
                                        ) => raw.volume = v,
                                        (
                                            pw::spa::sys::SPA_PROP_channelVolumes,
                                            pw::spa::pod::Value::ValueArray(
                                                pw::spa::pod::ValueArray::Float(v),
                                            ),
                                        ) => raw.channel_volumes = v,
                                        (
                                            pw::spa::sys::SPA_PROP_mute,
                                            pw::spa::pod::Value::Bool(v),
                                        ) => raw.mute = v,
                                        _ => {}
                                    }
                                }
                            }
                        }
                    })
                    .register();
                node.subscribe_params(&[pw::spa::param::ParamType::Props]);
                keep.borrow_mut().insert(obj.id, (node, Box::new(listener)));
            } else if obj.type_ == ObjectType::Metadata
                && props.get("metadata.name").map(String::as_str) == Some("default")
            {
                if let Ok(m) = reg.bind::<pw::metadata::Metadata, _>(obj) {
                    let graph = g.clone();
                    let listener = m
                        .add_listener_local()
                        .property(move |subject, key, _, value| {
                            if key == Some("target.object") {
                                if let Some(value) = value {
                                    graph.borrow_mut().targets.insert(subject, value.into());
                                } else {
                                    graph.borrow_mut().targets.remove(&subject);
                                }
                            }
                            0
                        })
                        .register();
                    *md_listener.borrow_mut() = Some(Box::new(listener));
                    *md.borrow_mut() = Some(m);
                }
            }
        })
        .global_remove({
            let g = graph.clone();
            let keep = proxies.clone();
            move |id| {
                g.borrow_mut().objects.remove(&id);
                keep.borrow_mut().remove(&id);
            }
        })
        .register();
    core.sync(0)?;
    struct Controller {
        desired: Option<Apply>,
        render: Option<render::Render>,
        signature: String,
        links: Vec<pw::link::Link>,
        linked: BTreeSet<(u32, u32)>,
        ingresses: Vec<pw::node::Node>,
        requested: BTreeSet<Id>,
        render_error: String,
        states: Vec<PluginState>,
        rate: u32,
    }
    let controller = Rc::new(RefCell::new(Controller {
        desired: None,
        render: None,
        signature: String::new(),
        links: vec![],
        linked: BTreeSet::new(),
        ingresses: vec![],
        requested: BTreeSet::new(),
        render_error: String::new(),
        states: vec![],
        rate: 48000,
    }));
    let teardown = controller.clone();
    // Timer callbacks are on the control thread; FFI process callbacks never touch this state.
    let loop_quit = mainloop.clone();
    let updates = tx.clone();
    let timer=mainloop.loop_().add_timer(move|_|{
        if stop.load(Ordering::Acquire){loop_quit.quit();return;}
        if !ready.get(){return;}
        let mut state=controller.borrow_mut();
        let Controller{desired,render,signature,links,linked,ingresses,requested,render_error,states,rate}=&mut *state;
        if let Some(current)=render.as_mut(){current.reclaim();let observed=current.rate();if observed>0&&observed!=*rate{*rate=observed;signature.clear();}}
        while let Ok(mut command)=rx.try_recv(){
            for control in command.output_controls.drain(..){
                let g=graph.borrow();
                if let Some((&id,raw))=g.objects.iter().find(|(_,o)|o.kind=="Node"&&o.props.get("node.name")==Some(&control.key)){
                    if let Some((node,_))=proxies.borrow().get(&id){if let Err(e)=set_output_control(node,raw,g.ports(id,false).len(),&control){*render_error=format!("Output control: {e:#}");}}
                }
            }
            *desired=Some(command);
        }
        if let Some(apply)=desired.as_ref() {
            let key=topology_signature(apply);
            if *signature!=key {
                let rebuild=if let Some(current)=render.as_mut(){
                    match current.reconfigure(&apply.scene,&apply.suspended,*rate){
                        Ok(render::Reconfigure::Applied)=>{*signature=key.clone();render_error.clear();false}
                        Ok(render::Reconfigure::Busy)=>false,
                        Ok(render::Reconfigure::Rebuild)=>true,
                        Err(e)=>{*render_error=format!("{e:#}");false}
                    }
                }else{true};
                if rebuild {
                links.clear();linked.clear();
                let mut scene=apply.scene.clone();
                if let Some(mut old)=render.take(){
                    let captured=old.checkpoint(&apply.state_dir);
                    for captured in &captured{if let Some(strip)=scene.strips.iter_mut().find(|s|s.id==captured.strip){if let Some(slot)=strip.inserts.iter_mut().find(|s|s.id==captured.insert){slot.state=Some(captured.path.clone());}}}
                    states.extend(captured);
                }
                match render::Render::new(&core,&scene,&apply.suspended,*rate) {
                    Ok(new)=>{*render=Some(new);render_error.clear();*signature=key;}
                    Err(e)=>{*render_error=format!("{e:#}");}
                }
                }
            }
            if let Some(render)=render.as_mut() {
                render.update_controls(&apply.scene);
                let g=graph.borrow();
                linked.retain(|(source,target)|g.objects.values().any(|o|o.kind=="Link"&&o.props.get("link.output.port").and_then(|v|v.parse::<u32>().ok())==Some(*source)&&o.props.get("link.input.port").and_then(|v|v.parse::<u32>().ok())==Some(*target)));
                for strip in apply.scene.strips.iter().filter(|s|s.kind==StripKind::VirtualMic) {
                    let exists=g.objects.values().any(|o|o.kind=="Node"&&o.props.get("node.name").map(String::as_str)==Some(MICROPHONE_KEY));
                    if !exists&&requested.insert(strip.id) {
                        let props=properties!{"factory.name"=>"support.null-audio-sink","node.name"=>MICROPHONE_KEY,"node.description"=>"Alixer Microphone","media.class"=>"Audio/Source/Virtual","audio.channels"=>"2","audio.position"=>"[ FL FR ]","monitor.passthrough"=>"true","node.virtual"=>"true","object.linger"=>"true","priority.session"=>"-1000","monitor.channel-volumes"=>"false"};
                        // Retain a silent endpoint on shutdown, just like playback
                        // ingresses. Recorders must not fall back to the raw mic.
                        match core.create_object::<pw::node::Node>("adapter",&props){Ok(node)=>ingresses.push(node),Err(e)=>{*render_error=e.to_string();requested.remove(&strip.id);}}
                    }
                }
                for strip in apply.scene.strips.iter().filter(|s|s.kind==StripKind::Input) {
                    let Some(source)=strip.input_key.as_ref() else{continue};
                    if let Some((&id,_))=g.objects.iter().find(|(_,o)|o.kind=="Node"&&o.props.get("node.name")==Some(source)) {
                        let ports=g.ports(id,true);
                        for (ch,expected) in strip.channels.iter().enumerate() {
                            let matches:Vec<_>=ports.iter().filter(|(_,_,channel)|channel==expected).collect();
                            let port=ports.get(ch).filter(|(_,_,channel)|channel==expected).or_else(||if matches.len()==1{Some(matches[0])}else{None});
                            if let (Some((_,port,_)),Some(input))=(port,render.input_name(strip.id,ch)) {
                                wire(&core,&g,linked,links,source,port,&render.name,&input);
                            }
                        }
                    }
                }
                for strip in apply.scene.strips.iter().filter(|s|s.kind==StripKind::Stream&&!apply.suspended.contains(&s.id)) {
                    let name=format!("alixer.input.{}",strip.id);
                    let ingress=g.objects.iter().find(|(_,o)|o.kind=="Node"&&o.props.get("node.name")==Some(&name)).map(|(id,_)|*id);
                    if ingress.is_none()&&requested.insert(strip.id) {
                        let props=properties!{"factory.name"=>"support.null-audio-sink","node.name"=>name.clone(),"node.description"=>format!("Alixer · {}",strip.name),"media.class"=>"Audio/Sink","audio.channels"=>strip.channels.len().to_string(),"audio.position"=>format!("[ {} ]",strip.channels.join(" ")),"object.linger"=>"true","node.virtual"=>"true","priority.session"=>"-1000","alixer.strip"=>strip.id.to_string(),"monitor.channel-volumes"=>"false"};
                        match core.create_object::<pw::node::Node>("adapter",&props){Ok(node)=>ingresses.push(node),Err(e)=>{*render_error=e.to_string();requested.remove(&strip.id);}}
                    }
                    let Some(ingress)=ingress else{continue};
                    if let Some(m)=metadata.borrow().as_ref() {
                        for stream in &g.observation().streams {
                            if apply.bindings.get(&runtime_key(&stream.runtime))==Some(&strip.id) {
                                if let Some((&id,_))=g.objects.iter().find(|(_,o)|o.props.get("object.serial")==Some(&stream.runtime.serial)){
                                    // A legacy ingress can carry the same strip ID.
                                    // Check the actual node link before considering
                                    // this stream connected to the current engine.
                                    if !g.has_link(id,ingress){m.set_property(id,"target.object",Some("Spa:Id"),Some(&name));}
                                }
                            }
                        }
                    }
                    let monitors=g.ports(ingress,true);
                    for (ch,(_,port,_)) in monitors.iter().enumerate() {
                        if let Some(input)=render.input_name(strip.id,ch){
                            wire(&core,&g,linked,links,&name,port,&render.name,&input);
                        }
                    }
                }
                for (output,ch,port) in &render.outputs {
                    if let Some((&id,_))=g.objects.iter().find(|(_,o)|o.props.get("node.name")==Some(output)){
                        let expected=apply.scene.strips.iter().find(|s|s.output_key.as_ref()==Some(output)).and_then(|s|s.channels.get(*ch));
                        let ports=g.ports(id,false);
                        let target=if let Some(expected)=expected{
                            ports.get(*ch).filter(|(_,_,channel)|channel==expected).or_else(||{
                                let mut matches=ports.iter().filter(|(_,_,channel)|channel==expected);
                                let first=matches.next();if matches.next().is_none(){first}else{None}
                            })
                        }else{ports.get(*ch)};
                        if let Some((_,target,_))=target{wire(&core,&g,linked,links,&render.name,port,output,target);}
                    }
                }
            }
        }
        let observation=graph.borrow().observation();
        let status=if !render_error.is_empty(){format!("Audio error: {render_error}")}else if let Some(render)=render.as_ref(){if render.latency_overflow(){"Audio error: plugin compensation exceeds 16384 frames; outputs silenced".into()}else if !render.warnings.is_empty(){format!("Connected · unavailable inserts bypassed: {}",render.warnings.join("; "))}else{format!("Connected to PipeWire · {rate} Hz")}}else{"Connected to PipeWire".into()};
        let peaks=render.as_ref().map(|r|r.peaks()).unwrap_or_default();
        let channel_peaks=render.as_ref().map(|r|r.channel_peaks()).unwrap_or_default();
        let plugin_meters=render.as_ref().map(|r|r.plugin_meters()).unwrap_or_default();
        if updates.send(Update{observation,status,peaks,channel_peaks,plugin_meters,states:std::mem::take(states)}).is_err(){loop_quit.quit();}
    });
    timer
        .update_timer(
            Some(Duration::from_millis(150)),
            Some(Duration::from_millis(100)),
        )
        .into_result()?;
    mainloop.run();
    drop(timer);
    let mut controller = teardown.borrow_mut();
    let path = controller.desired.as_ref().map(|a| a.state_dir.clone());
    let states = if let (Some(mut render), Some(path)) = (controller.render.take(), path) {
        render.checkpoint(&path)
    } else {
        vec![]
    };
    if failed.get() {
        let _ = tx.send(Update {
            observation: Observation::default(),
            status: "Disconnected: audio server restarted".into(),
            peaks: vec![],
            channel_peaks: vec![],
            plugin_meters: vec![],
            states,
        });
        anyhow::bail!("audio server disconnected");
    }
    Ok(states)
}
fn set_output_control(
    node: &pw::node::Node,
    raw: &Raw,
    channels: usize,
    control: &OutputControl,
) -> Result<()> {
    use pw::spa::pod::{Object, Pod, Property, PropertyFlags, Value, ValueArray};
    let mut properties = vec![];
    if let Some(gain) = control.gain {
        let values = balanced_volumes(&raw.channel_volumes, &control.balance, channels, gain);
        properties.push(Property {
            key: pw::spa::sys::SPA_PROP_volume,
            flags: PropertyFlags::empty(),
            value: Value::Float(1.0),
        });
        properties.push(Property {
            key: pw::spa::sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(values)),
        });
    }
    if let Some(mute) = control.mute {
        properties.push(Property {
            key: pw::spa::sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(mute),
        });
    }
    let value = Value::Object(Object {
        type_: pw::spa::sys::SPA_TYPE_OBJECT_Props,
        id: pw::spa::sys::SPA_PARAM_Props,
        properties,
    });
    let bytes = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &value,
    )?
    .0
    .into_inner();
    node.set_param(
        pw::spa::param::ParamType::Props,
        0,
        Pod::from_bytes(&bytes).ok_or_else(|| anyhow::anyhow!("Invalid control POD"))?,
    );
    Ok(())
}
fn balanced_volumes(values: &[f32], fallback: &[f32], channels: usize, gain: f32) -> Vec<f32> {
    let max = values.iter().copied().reduce(f32::max).unwrap_or(0.0);
    if max > 0.0 {
        values.iter().map(|v| v / max * gain).collect()
    } else if fallback.len() == channels {
        fallback.iter().map(|ratio| ratio * gain).collect()
    } else {
        vec![gain; channels]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_and_new_internal_nodes_are_excluded_from_physical_outputs() {
        let mut graph = Graph::default();
        for (id, prefix) in [(1, "lixer"), (2, "alixer")] {
            graph.objects.insert(
                id,
                Raw {
                    kind: "Node".into(),
                    props: BTreeMap::from([
                        (
                            "node.name".into(),
                            format!("{prefix}.input.{}", Id::new_v4()),
                        ),
                        ("media.class".into(), "Audio/Sink".into()),
                    ]),
                    ..Default::default()
                },
            );
        }
        assert!(graph.observation().outputs.is_empty());
    }
    #[test]
    fn presentation_and_live_controls_preserve_the_compiled_audio_graph() {
        let source = Strip::new("Music", StripKind::Stream, vec!["FL".into(), "FR".into()]);
        let mut output = Strip::new("Speakers", StripKind::Output, source.channels.clone());
        output.output_key = Some("speakers".into());
        let mut apply = Apply {
            scene: Scene {
                strips: vec![source.clone(), output.clone()],
                routes: vec![Route {
                    id: Id::new_v4(),
                    source: source.id,
                    destination: Destination::Output("speakers".into()),
                    source_channel: 0,
                    target_channel: 0,
                    gain: 1.0,
                    tap: Tap::Post,
                }],
                ..Default::default()
            },
            bindings: BTreeMap::new(),
            suspended: BTreeSet::new(),
            state_dir: PathBuf::new(),
            output_controls: vec![],
        };
        let signature = topology_signature(&apply);
        apply.bindings.insert("server:new-stream-incarnation".into(), source.id);
        assert_eq!(topology_signature(&apply), signature, "runtime bindings do not change the DSP graph");
        for edit in [
            Edit::Color {
                strip: source.id,
                color: [35, 24, 151],
            },
            Edit::Color {
                strip: output.id,
                color: [119, 24, 151],
            },
            Edit::Rename {
                strip: output.id,
                name: "Desk".into(),
            },
            Edit::Reorder {
                strips: vec![output.id, source.id],
            },
            Edit::Gain {
                strip: source.id,
                value: 0.5,
            },
            Edit::Mute {
                strip: source.id,
                value: true,
            },
        ] {
            apply.scene.edit(edit).unwrap();
            assert_eq!(topology_signature(&apply), signature);
        }
        apply.scene.routes[0].gain = 0.5;
        apply.scene.routes[0].tap = Tap::Pre;
        assert_eq!(topology_signature(&apply), signature);
        apply.scene.routes[0].target_channel = 1;
        assert_ne!(topology_signature(&apply), signature);
        apply.scene.routes[0].target_channel = 0;
        apply.scene.strips[0].inserts.push(Insert {
            id: Id::new_v4(),
            uri: "urn:test:effect".into(),
            bypass: false,
            controls: BTreeMap::new(),
            state: None,
        });
        let with_insert = topology_signature(&apply);
        assert_ne!(with_insert, signature);
        apply.scene.strips[0].inserts[0]
            .controls
            .insert("gain".into(), 0.5);
        assert_eq!(topology_signature(&apply), with_insert);
        apply.scene.strips[0].inserts[0].bypass = true;
        assert_ne!(topology_signature(&apply), with_insert);
    }
    #[test]
    fn channel_order_survives_reused_global_ids() {
        let mut graph = Graph::default();
        for (global, local, channel) in [(10, 1, "FR"), (20, 0, "FL")] {
            graph.objects.insert(
                global,
                Raw {
                    kind: "Port".into(),
                    props: BTreeMap::from([
                        ("node.id".into(), "7".into()),
                        ("port.direction".into(), "in".into()),
                        ("port.id".into(), local.to_string()),
                        ("audio.channel".into(), channel.into()),
                    ]),
                    ..Default::default()
                },
            );
        }
        let ports = graph.ports(7, false);
        assert_eq!(ports[0].2, "FL");
        assert_eq!(ports[1].2, "FR");
    }
    #[test]
    fn master_volume_preserves_channel_balance() {
        assert_eq!(balanced_volumes(&[0.5, 0.25], &[], 2, 0.8), vec![0.8, 0.4]);
        assert_eq!(balanced_volumes(&[0.0, 0.0], &[], 2, 0.8), vec![0.8, 0.8]);
        assert_eq!(
            balanced_volumes(&[0.0, 0.0], &[1.0, 0.5], 2, 0.8),
            vec![0.8, 0.4]
        );
    }
}
fn wire(
    core: &pw::core::CoreRc,
    g: &Graph,
    linked: &mut BTreeSet<(u32, u32)>,
    links: &mut Vec<pw::link::Link>,
    source: &str,
    source_port: &str,
    target: &str,
    target_port: &str,
) {
    let node = |name: &str| {
        g.objects
            .iter()
            .find(|(_, o)| {
                o.kind == "Node" && o.props.get("node.name").map(String::as_str) == Some(name)
            })
            .map(|(id, _)| *id)
    };
    let (Some(sn), Some(tn)) = (node(source), node(target)) else {
        return;
    };
    let port = |node, out, name: &str| {
        g.ports(node, out)
            .into_iter()
            .find(|p| p.1 == name)
            .map(|p| p.0)
    };
    let (Some(sp), Some(tp)) = (port(sn, true, source_port), port(tn, false, target_port)) else {
        return;
    };
    if linked.contains(&(sp, tp)) {
        return;
    }
    if let Ok(link)=core.create_object::<pw::link::Link>("link-factory",&properties!{"link.output.port"=>sp.to_string(),"link.input.port"=>tp.to_string(),"object.linger"=>"false"}) {linked.insert((sp,tp));links.push(link);}
}
