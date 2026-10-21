use anyhow::{Result, bail};
use alixer_core::{dsp::Processor, *};
use pipewire::{self as pw, properties::properties};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

const MAX_FRAMES: usize = 8192;
struct Controls {
    gain: AtomicU32,
    mute: AtomicBool,
    solo: AtomicBool,
    peak: AtomicU32,
    channels: Vec<AtomicU32>,
}
struct RouteControls {
    gain: AtomicU32,
    pre: AtomicBool,
}
struct Data {
    processor: Processor,
    inputs: Vec<(Id, usize, *mut c_void)>,
    outputs: Vec<(String, usize, *mut c_void)>,
    controls: Vec<(Id, Arc<Controls>)>,
    route_controls: Vec<(Id, Arc<RouteControls>)>,
    effects: BTreeMap<Id, Vec<alixer_lv2::Instance>>,
    rate: Arc<AtomicU32>,
    expected_rate: u32,
    overflow: Arc<AtomicBool>,
}
pub struct Render {
    filter: *mut pw::sys::pw_filter,
    _hook: Box<pw::spa::sys::spa_hook>,
    _events: Box<pw::sys::pw_filter_events>,
    _data: Box<Data>,
    controls: BTreeMap<Id, Arc<Controls>>,
    route_controls: BTreeMap<Id, Arc<RouteControls>>,
    parameters: BTreeMap<(Id, Id, String), Arc<AtomicU32>>,
    pub name: String,
    input_names: BTreeMap<(Id, usize), String>,
    pub outputs: Vec<(String, usize, String)>,
    pub warnings: Vec<String>,
    rate: Arc<AtomicU32>,
    overflow: Arc<AtomicBool>,
}
unsafe extern "C" fn process(data: *mut c_void, position: *mut pw::spa::sys::spa_io_position) {
    // SAFETY: PipeWire serializes callbacks for this filter. All buffers and user data
    // outlive it. pw_filter_destroy synchronizes teardown before Data is dropped.
    unsafe {
        let data = &mut *data.cast::<Data>();
        let frames = (*position).clock.duration as usize;
        let rate = (*position).clock.rate.denom as f32;
        data.rate.store(rate as u32, Ordering::Relaxed);
        if frames > MAX_FRAMES || rate as u32 != data.expected_rate {
            for (_, _, port) in &data.outputs {
                let out = pw::sys::pw_filter_get_dsp_buffer(*port, frames as u32).cast::<f32>();
                if !out.is_null() {
                    ptr::write_bytes(out, 0, frames);
                }
            }
            return;
        }
        data.processor.clear();
        for (id, ch, port) in &data.inputs {
            let input = pw::sys::pw_filter_get_dsp_buffer(*port, frames as u32).cast::<f32>();
            if !input.is_null() {
                if let Some(buffer) = data.processor.input(*id, *ch) {
                    buffer[..frames].copy_from_slice(std::slice::from_raw_parts(input, frames));
                }
            }
        }
        for (id, c) in &data.controls {
            data.processor.set_controls(
                *id,
                f32::from_bits(c.gain.load(Ordering::Relaxed)),
                c.mute.load(Ordering::Relaxed),
                c.solo.load(Ordering::Relaxed),
            );
        }
        for (id, c) in &data.route_controls {
            data.processor.set_route_controls(
                *id,
                f32::from_bits(c.gain.load(Ordering::Relaxed)),
                if c.pre.load(Ordering::Relaxed) {
                    Tap::Pre
                } else {
                    Tap::Post
                },
            );
        }
        for (id, chain) in &data.effects {
            data.processor
                .set_latency(*id, chain.iter().map(|i| i.latency()).sum());
        }
        let effects = &mut data.effects;
        data.processor
            .process_with(frames, rate.max(1.0), |id, channels, frames| {
                if let Some(chain) = effects.get_mut(&id) {
                    for effect in chain {
                        effect.run(channels, frames);
                    }
                }
            });
        data.overflow
            .store(data.processor.latency_overflow(), Ordering::Relaxed);
        for (name, ch, port) in &data.outputs {
            let output = pw::sys::pw_filter_get_dsp_buffer(*port, frames as u32).cast::<f32>();
            if !output.is_null() {
                data.processor
                    .output(name, *ch, std::slice::from_raw_parts_mut(output, frames));
            }
        }
        for (id, c) in &data.controls {
            c.peak.store(
                data.processor.peak(*id, frames).to_bits(),
                Ordering::Relaxed,
            );
            for (ch, peak) in c.channels.iter().enumerate() {
                peak.store(
                    data.processor.channel_peak(*id, ch, frames).to_bits(),
                    Ordering::Relaxed,
                );
            }
        }
    }
}
impl Render {
    pub fn new(
        core: &pw::core::CoreRc,
        scene: &Scene,
        suspended: &BTreeSet<Id>,
        sample_rate: u32,
    ) -> Result<Self> {
        let mut effective = scene.clone();
        for s in &mut effective.strips {
            if suspended.contains(&s.id) {
                s.mute = true;
            }
        }
        let controls: BTreeMap<_, _> = effective
            .strips
            .iter()
            .map(|s| {
                (
                    s.id,
                    Arc::new(Controls {
                        gain: AtomicU32::new(s.gain.to_bits()),
                        mute: AtomicBool::new(s.mute),
                        solo: AtomicBool::new(s.solo),
                        peak: AtomicU32::new(0),
                        channels: s.channels.iter().map(|_| AtomicU32::new(0)).collect(),
                    }),
                )
            })
            .collect();
        let route_controls: BTreeMap<_, _> = effective
            .routes
            .iter()
            .map(|r| {
                (
                    r.id,
                    Arc::new(RouteControls {
                        gain: AtomicU32::new(r.gain.to_bits()),
                        pre: AtomicBool::new(r.tap == Tap::Pre),
                    }),
                )
            })
            .collect();
        let mut effects = BTreeMap::new();
        let mut parameters = BTreeMap::new();
        let mut warnings = vec![];
        for strip in &effective.strips {
            let mut chain = vec![];
            for insert in strip.inserts.iter().filter(|i| !i.bypass) {
                let instance = match alixer_lv2::Instance::new(
                    insert,
                    strip.channels.len(),
                    sample_rate as f64,
                    MAX_FRAMES,
                ) {
                    Ok(instance) => instance,
                    Err(e) => {
                        warnings.push(format!("{}: {e:#}", insert.uri));
                        continue;
                    }
                };
                for (symbol, value) in &instance.parameters {
                    parameters.insert((strip.id, insert.id, symbol.clone()), value.clone());
                }
                chain.push(instance);
            }
            effects.insert(strip.id, chain);
        }
        let rate = Arc::new(AtomicU32::new(sample_rate));
        let overflow = Arc::new(AtomicBool::new(false));
        let mut data = Box::new(Data {
            processor: Processor::new(effective, MAX_FRAMES)?,
            inputs: vec![],
            outputs: vec![],
            controls: controls.iter().map(|(id, c)| (*id, c.clone())).collect(),
            route_controls: route_controls
                .iter()
                .map(|(id, c)| (*id, c.clone()))
                .collect(),
            effects,
            rate: rate.clone(),
            expected_rate: sample_rate,
            overflow: overflow.clone(),
        });
        let mut events: Box<pw::sys::pw_filter_events> = Box::new(unsafe { std::mem::zeroed() });
        events.version = 1;
        events.process = Some(process);
        let mut hook = Box::new(unsafe { std::mem::zeroed() });
        let name = format!("alixer.engine.{}", Id::new_v4());
        let props = properties! {"node.name"=>name.clone(),"node.description"=>"Alixer mixer","media.class"=>"Audio/Filter","node.autoconnect"=>"false","node.link-group"=>"alixer","audio.rate"=>"48000"};
        let filter = unsafe {
            pw::sys::pw_filter_new(core.as_raw_ptr(), c"Alixer".as_ptr(), props.into_raw())
        };
        if filter.is_null() {
            bail!("Could not create PipeWire DSP filter");
        }
        unsafe {
            pw::sys::pw_filter_add_listener(
                filter,
                &mut *hook,
                &*events,
                (&mut *data as *mut Data).cast(),
            );
        }
        let mut this = Self {
            filter,
            _hook: hook,
            _events: events,
            _data: data,
            controls,
            route_controls,
            parameters,
            name,
            input_names: BTreeMap::new(),
            outputs: vec![],
            warnings,
            rate,
            overflow,
        };
        for strip in scene
            .strips
            .iter()
            .filter(|s| matches!(s.kind, StripKind::Stream | StripKind::Input) && !suspended.contains(&s.id))
        {
            for ch in 0..strip.channels.len() {
                let name = format!("in_{}_{}", strip.id.simple(), ch);
                let port = this.port(&name, false)?;
                this._data.inputs.push((strip.id, ch, port));
                this.input_names.insert((strip.id, ch), name);
            }
        }
        let mut outputs: BTreeSet<_> = scene
            .routes
            .iter()
            .filter_map(|r| {
                if let Destination::Output(name) = &r.destination {
                    Some((name.clone(), r.target_channel))
                } else {
                    None
                }
            })
            .collect();
        for strip in &scene.strips {
            if let Some(key) = &strip.output_key {
                for ch in 0..strip.channels.len() {
                    outputs.insert((key.clone(), ch));
                }
            }
        }
        for (output, ch) in outputs {
            let name = format!("out_{}", this.outputs.len());
            let port = this.port(&name, true)?;
            this._data.outputs.push((output.clone(), ch, port));
            this.outputs.push((output, ch, name));
        }
        let result = unsafe {
            pw::sys::pw_filter_connect(
                filter,
                pw::sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
                ptr::null_mut(),
                0,
            )
        };
        if result < 0 {
            bail!(
                "PipeWire DSP connection failed: {}",
                std::io::Error::from_raw_os_error(-result)
            );
        }
        Ok(this)
    }
    fn port(&self, name: &str, output: bool) -> Result<*mut c_void> {
        let props = properties! {"format.dsp"=>"32 bit float mono audio","port.name"=>name,"port.physical"=>"false"};
        let port = unsafe {
            pw::sys::pw_filter_add_port(
                self.filter,
                if output {
                    pw::spa::sys::SPA_DIRECTION_OUTPUT
                } else {
                    pw::spa::sys::SPA_DIRECTION_INPUT
                },
                pw::sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                1,
                props.into_raw(),
                ptr::null_mut(),
                0,
            )
        };
        if port.is_null() {
            bail!("Could not add DSP port {name}");
        }
        Ok(port)
    }
    pub fn update_controls(&self, scene: &Scene) {
        for route in &scene.routes {
            if let Some(c) = self.route_controls.get(&route.id) {
                c.gain.store(route.gain.to_bits(), Ordering::Relaxed);
                c.pre.store(route.tap == Tap::Pre, Ordering::Relaxed);
            }
        }
        for strip in &scene.strips {
            if let Some(c) = self.controls.get(&strip.id) {
                c.gain.store(strip.gain.to_bits(), Ordering::Relaxed);
                c.mute.store(strip.mute, Ordering::Relaxed);
                c.solo.store(strip.solo, Ordering::Relaxed);
            }
            for insert in &strip.inserts {
                for (symbol, value) in &insert.controls {
                    if let Some(control) =
                        self.parameters.get(&(strip.id, insert.id, symbol.clone()))
                    {
                        control.store(value.to_bits(), Ordering::Relaxed);
                    }
                }
            }
        }
    }
    pub fn input_name(&self, id: Id, ch: usize) -> Option<String> {
        self.input_names.get(&(id, ch)).cloned()
    }
    pub fn peaks(&self) -> Vec<(Id, f32)> {
        self.controls
            .iter()
            .map(|(id, c)| (*id, f32::from_bits(c.peak.load(Ordering::Relaxed))))
            .collect()
    }
    pub fn channel_peaks(&self) -> Vec<(Id, Vec<f32>)> {
        self.controls
            .iter()
            .map(|(id, c)| {
                (
                    *id,
                    c.channels
                        .iter()
                        .map(|p| f32::from_bits(p.load(Ordering::Relaxed)))
                        .collect(),
                )
            })
            .collect()
    }
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }
    pub fn latency_overflow(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }
    /// Stop the native callbacks before invoking any non-real-time state API.
    pub fn checkpoint(&mut self, root: &std::path::Path) -> Vec<super::PluginState> {
        if !self.filter.is_null() {
            unsafe {
                pw::sys::pw_filter_destroy(self.filter);
            }
            self.filter = ptr::null_mut();
        }
        let mut result = vec![];
        for (strip, chain) in &mut self._data.effects {
            for instance in chain {
                let path = root
                    .join("plugins")
                    .join(instance.id.to_string())
                    .join(Id::new_v4().to_string());
                match instance.save_state(&path) {
                    Ok(path) => result.push(super::PluginState {
                        strip: *strip,
                        insert: instance.id,
                        path,
                    }),
                    Err(e) => {
                        eprintln!("Could not checkpoint plugin {}: {e:#}", instance.plugin.uri)
                    }
                }
            }
        }
        result
    }
}
impl Drop for Render {
    fn drop(&mut self) {
        if !self.filter.is_null() {
            unsafe {
                pw::sys::pw_filter_destroy(self.filter);
            }
        }
    }
}
