use alixer_core::{dsp::Processor, *};
use anyhow::{Result, bail};
use pipewire::{self as pw, properties::properties};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
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
struct Plan {
    processor: Processor,
    inputs: Vec<(Id, usize, *mut c_void)>,
    outputs: Vec<(String, usize, *mut c_void)>,
    controls: Vec<(Id, Arc<Controls>)>,
    route_controls: Vec<(Id, Arc<RouteControls>)>,
    effects: Vec<(Id, Vec<alixer_lv2::Instance>)>,
    transfer: Option<alixer_core::dsp::RuntimeTransfer>,
    reuse_effects: Vec<(usize, usize)>,
    rate: Arc<AtomicU32>,
    expected_rate: u32,
    overflow: Arc<AtomicBool>,
}

#[derive(Default)]
struct Handoff {
    pending: AtomicPtr<Plan>,
    retired: AtomicPtr<Plan>,
}

impl Drop for Handoff {
    fn drop(&mut self) {
        // The filter has been destroyed before the last owner is dropped.
        for slot in [&self.pending, &self.retired] {
            let plan = slot.swap(ptr::null_mut(), Ordering::AcqRel);
            if !plan.is_null() {
                unsafe {
                    drop(Box::from_raw(plan));
                }
            }
        }
    }
}

struct Data {
    metering: Arc<super::Metering>,
    hold_peaks: bool,
    active: Box<Plan>,
    handoff: Arc<Handoff>,
}

impl Data {
    fn activate_pending(&mut self) {
        // At most one retired plan. Its allocations and any removed plugins are
        // reclaimed on the control thread, never in the process callback.
        if !self.handoff.retired.load(Ordering::Acquire).is_null() {
            return;
        }
        let pending = self.handoff.pending.swap(ptr::null_mut(), Ordering::AcqRel);
        if pending.is_null() {
            return;
        }
        let mut next = unsafe { Box::from_raw(pending) };
        if let Some(transfer) = &next.transfer {
            next.processor
                .inherit_runtime(&mut self.active.processor, transfer);
        }
        for &(new, old) in &next.reuse_effects {
            std::mem::swap(&mut next.effects[new].1, &mut self.active.effects[old].1);
        }
        let old = std::mem::replace(&mut self.active, next);
        self.handoff
            .retired
            .store(Box::into_raw(old), Ordering::Release);
    }
}

pub enum Reconfigure {
    Applied,
    Busy,
    Rebuild,
}

pub struct Render {
    filter: *mut pw::sys::pw_filter,
    _hook: Box<pw::spa::sys::spa_hook>,
    _events: Box<pw::sys::pw_filter_events>,
    _data: Box<Data>,
    handoff: Arc<Handoff>,
    scene: Scene,
    suspended: BTreeSet<Id>,
    sample_rate: u32,
    in_flight: bool,
    input_ports: BTreeMap<(Id, usize), *mut c_void>,
    output_ports: BTreeMap<(String, usize), (*mut c_void, String)>,
    next_output: usize,
    controls: BTreeMap<Id, Arc<Controls>>,
    route_controls: BTreeMap<Id, Arc<RouteControls>>,
    parameters: BTreeMap<(Id, Id, String), Arc<AtomicU32>>,
    meters: BTreeMap<(Id, Id, String), Arc<AtomicU32>>,
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
        data.activate_pending();
        let metering = data.metering.enabled();
        let hold_peaks = data.hold_peaks;
        let data = &mut data.active;
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
        data.processor.clear_frames(frames);
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
                if let Some((_, chain)) = effects.iter_mut().find(|(key, _)| *key == id) {
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
        if metering {
            for (id, c) in &data.controls {
                let peak = data.processor.peak(*id, frames).to_bits();
                if hold_peaks {
                    // Event sounds can finish between telemetry ticks. Keep
                    // their highest nonnegative peak until it is consumed.
                    c.peak.fetch_max(peak, Ordering::Relaxed);
                } else {
                    c.peak.store(peak, Ordering::Relaxed);
                }
                for (ch, peak) in c.channels.iter().enumerate() {
                    peak.store(
                        data.processor.channel_peak(*id, ch, frames).to_bits(),
                        Ordering::Relaxed,
                    );
                }
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
        metering: Arc<super::Metering>,
    ) -> Result<Self> {
        Self::new_named(core, scene, suspended, sample_rate, metering, "alixer.engine")
    }
    /// An unrouted hardware input gets its own clock domain for UI previews.
    pub fn new_preview(
        core: &pw::core::CoreRc,
        scene: &Scene,
        sample_rate: u32,
        metering: Arc<super::Metering>,
    ) -> Result<Self> {
        Self::new_named(core, scene, &BTreeSet::new(), sample_rate, metering, "alixer.preview")
    }
    pub fn new_event(
        core: &pw::core::CoreRc,
        scene: &Scene,
        sample_rate: u32,
        metering: Arc<super::Metering>,
    ) -> Result<Self> {
        Self::new_named(core, scene, &BTreeSet::new(), sample_rate, metering, "alixer.system")
    }
    fn new_named(
        core: &pw::core::CoreRc,
        scene: &Scene,
        suspended: &BTreeSet<Id>,
        sample_rate: u32,
        metering: Arc<super::Metering>,
        prefix: &str,
    ) -> Result<Self> {
        let mut mixer_scene = scene.clone();
        mixer_scene.strips.retain(|s| s.kind != StripKind::SystemSounds);
        let scene = &mixer_scene;
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
        let mut effects = vec![];
        let mut parameters = BTreeMap::new();
        let mut meters = BTreeMap::new();
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
                for (symbol, value) in &instance.meters {
                    meters.insert((strip.id, insert.id, symbol.clone()), value.clone());
                }
                chain.push(instance);
            }
            effects.push((strip.id, chain));
        }
        let rate = Arc::new(AtomicU32::new(sample_rate));
        let overflow = Arc::new(AtomicBool::new(false));
        let handoff = Arc::new(Handoff::default());
        let mut data = Box::new(Data {
            metering,
            hold_peaks: prefix == "alixer.system",
            handoff: handoff.clone(),
            active: Box::new(Plan {
                processor: Processor::new(effective, MAX_FRAMES)?,
                inputs: vec![],
                outputs: vec![],
                controls: controls.iter().map(|(id, c)| (*id, c.clone())).collect(),
                route_controls: route_controls
                    .iter()
                    .map(|(id, c)| (*id, c.clone()))
                    .collect(),
                effects,
                transfer: None,
                reuse_effects: vec![],
                rate: rate.clone(),
                expected_rate: sample_rate,
                overflow: overflow.clone(),
            }),
        });
        let mut events: Box<pw::sys::pw_filter_events> = Box::new(unsafe { std::mem::zeroed() });
        events.version = 1;
        events.process = Some(process);
        let mut hook = Box::new(unsafe { std::mem::zeroed() });
        let name = format!("{prefix}.{}", Id::new_v4());
        let props = properties! {"node.name"=>name.clone(),"node.description"=>"Alixer mixer","media.class"=>"Audio/Filter","node.autoconnect"=>"false","node.link-group"=>name.clone(),"audio.rate"=>sample_rate.to_string()};
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
            handoff,
            scene: scene.clone(),
            suspended: suspended.clone(),
            sample_rate,
            in_flight: false,
            input_ports: BTreeMap::new(),
            output_ports: BTreeMap::new(),
            next_output: 0,
            controls,
            route_controls,
            parameters,
            meters,
            name,
            input_names: BTreeMap::new(),
            outputs: vec![],
            warnings,
            rate,
            overflow,
        };
        for strip in scene.strips.iter().filter(|s| {
            matches!(s.kind, StripKind::Stream | StripKind::Input) && !suspended.contains(&s.id)
        }) {
            for ch in 0..strip.channels.len() {
                let name = format!("in_{}_{}", strip.id.simple(), ch);
                let port = this.port(
                    &name,
                    false,
                    if strip.kind == StripKind::Stream {
                        "follow"
                    } else {
                        "false"
                    },
                )?;
                this._data.active.inputs.push((strip.id, ch, port));
                this.input_ports.insert((strip.id, ch), port);
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
            let port = this.port(&name, true, "true")?;
            this._data.active.outputs.push((output.clone(), ch, port));
            this.output_ports
                .insert((output.clone(), ch), (port, name.clone()));
            this.next_output += 1;
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
    // Playback inputs follow their ingress; outputs must not wake idle sinks.
    // Hardware input ports actively drive the graph only while wired by the controller.
    fn port(&self, name: &str, output: bool, passive: &str) -> Result<*mut c_void> {
        let props = properties! {"format.dsp"=>"32 bit float mono audio","port.name"=>name,"port.physical"=>"false","port.passive"=>passive};
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
    /// Called on the PipeWire control thread. Never read the live RT plan here.
    pub fn reclaim(&mut self) {
        let retired = self.handoff.retired.swap(ptr::null_mut(), Ordering::AcqRel);
        if retired.is_null() {
            return;
        }
        unsafe {
            drop(Box::from_raw(retired));
        }
        self.in_flight = false;
        if self.filter.is_null() {
            return;
        }
        self.input_ports.retain(|key, port| {
            let keep = self.input_names.contains_key(key);
            if !keep {
                unsafe {
                    pw::sys::pw_filter_remove_port(*port);
                }
            }
            keep
        });
        self.output_ports.retain(|(key, ch), (port, _)| {
            let keep = self
                .outputs
                .iter()
                .any(|(name, channel, _)| name == key && channel == ch);
            if !keep {
                unsafe {
                    pw::sys::pw_filter_remove_port(*port);
                }
            }
            keep
        });
    }

    pub fn reconfigure(
        &mut self,
        scene: &Scene,
        suspended: &BTreeSet<Id>,
        sample_rate: u32,
    ) -> Result<Reconfigure> {
        let mut mixer_scene = scene.clone();
        mixer_scene.strips.retain(|s| s.kind != StripKind::SystemSounds);
        let scene = &mixer_scene;
        self.reclaim();
        if self.in_flight {
            // A suspended filter cannot acknowledge a plan. Rebuild on the
            // control thread so subsequent edits can introduce new wakeup links.
            return Ok(if self.running() {
                Reconfigure::Busy
            } else {
                Reconfigure::Rebuild
            });
        }
        // Preserve running plugin instances for normal graph edits. A sample-rate
        // change or insert-chain edit still uses the checkpoint/rebuild fallback.
        if sample_rate != self.sample_rate
            || scene.strips.iter().any(|s| {
                if let Some(old) = self.scene.strip(s.id) {
                    old.channels != s.channels
                        || old
                            .inserts
                            .iter()
                            .map(|i| (i.id, &i.uri, i.bypass))
                            .ne(s.inserts.iter().map(|i| (i.id, &i.uri, i.bypass)))
                } else {
                    !s.inserts.is_empty()
                }
            })
        {
            return Ok(Reconfigure::Rebuild);
        }

        let mut effective = scene.clone();
        for s in &mut effective.strips {
            if suspended.contains(&s.id) {
                s.mute = true;
            }
        }
        let processor = Processor::new(effective, MAX_FRAMES)?;
        let transfer = processor.prepare_transfer(&self.scene);
        let controls: BTreeMap<_, _> = scene
            .strips
            .iter()
            .map(|s| {
                (
                    s.id,
                    self.controls.get(&s.id).cloned().unwrap_or_else(|| {
                        Arc::new(Controls {
                            gain: AtomicU32::new(s.gain.to_bits()),
                            mute: AtomicBool::new(s.mute),
                            solo: AtomicBool::new(s.solo),
                            peak: AtomicU32::new(0),
                            channels: s.channels.iter().map(|_| AtomicU32::new(0)).collect(),
                        })
                    }),
                )
            })
            .collect();
        let route_controls: BTreeMap<_, _> = scene
            .routes
            .iter()
            .map(|r| {
                (
                    r.id,
                    self.route_controls.get(&r.id).cloned().unwrap_or_else(|| {
                        Arc::new(RouteControls {
                            gain: AtomicU32::new(r.gain.to_bits()),
                            pre: AtomicBool::new(r.tap == Tap::Pre),
                        })
                    }),
                )
            })
            .collect();
        let mut inputs = vec![];
        let mut input_names = BTreeMap::new();
        for s in scene.strips.iter().filter(|s| {
            matches!(s.kind, StripKind::Stream | StripKind::Input) && !suspended.contains(&s.id)
        }) {
            for ch in 0..s.channels.len() {
                let key = (s.id, ch);
                let name = format!("in_{}_{}", s.id.simple(), ch);
                let port = if let Some(port) = self.input_ports.get(&key) {
                    *port
                } else {
                    let port = self.port(
                        &name,
                        false,
                        if s.kind == StripKind::Stream {
                            "follow"
                        } else {
                            "false"
                        },
                    )?;
                    self.input_ports.insert(key, port);
                    port
                };
                inputs.push((s.id, ch, port));
                input_names.insert(key, name);
            }
        }
        let mut destinations: BTreeSet<_> = scene
            .routes
            .iter()
            .filter_map(|r| {
                if let Destination::Output(key) = &r.destination {
                    Some((key.clone(), r.target_channel))
                } else {
                    None
                }
            })
            .collect();
        for s in &scene.strips {
            if let Some(key) = &s.output_key {
                for ch in 0..s.channels.len() {
                    destinations.insert((key.clone(), ch));
                }
            }
        }
        let mut outputs = vec![];
        let mut output_names = vec![];
        for (key, ch) in destinations {
            let (port, name) = if let Some(port) = self.output_ports.get(&(key.clone(), ch)) {
                port.clone()
            } else {
                let name = format!("out_{}", self.next_output);
                self.next_output += 1;
                let port = self.port(&name, true, "true")?;
                self.output_ports
                    .insert((key.clone(), ch), (port, name.clone()));
                (port, name)
            };
            outputs.push((key.clone(), ch, port));
            output_names.push((key, ch, name));
        }
        let reuse_effects = scene
            .strips
            .iter()
            .enumerate()
            .filter_map(|(new, s)| {
                self.scene
                    .strips
                    .iter()
                    .position(|old| old.id == s.id)
                    .map(|old| (new, old))
            })
            .collect();
        let plan = Box::new(Plan {
            processor,
            inputs,
            outputs,
            controls: controls.iter().map(|(id, c)| (*id, c.clone())).collect(),
            route_controls: route_controls
                .iter()
                .map(|(id, c)| (*id, c.clone()))
                .collect(),
            effects: scene.strips.iter().map(|s| (s.id, vec![])).collect(),
            transfer: Some(transfer),
            reuse_effects,
            rate: self.rate.clone(),
            expected_rate: sample_rate,
            overflow: self.overflow.clone(),
        });
        self.controls = controls;
        self.route_controls = route_controls;
        self.parameters
            .retain(|(id, _, _), _| scene.strip(*id).is_some());
        self.meters
            .retain(|(id, _, _), _| scene.strip(*id).is_some());
        self.input_names = input_names;
        self.outputs = output_names;
        self.scene = scene.clone();
        self.suspended = suspended.clone();
        self.update_controls(scene);
        self.in_flight = true;
        // Publish only after the plan and its native ports are fully prepared.
        self.handoff
            .pending
            .store(Box::into_raw(plan), Ordering::Release);
        Ok(Reconfigure::Applied)
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
                c.mute.store(
                    strip.mute || self.suspended.contains(&strip.id),
                    Ordering::Relaxed,
                );
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
    pub fn plugin_meters(&self) -> Vec<(Id, String, f32)> {
        self.meters
            .iter()
            .map(|((_, insert, symbol), value)| {
                (
                    *insert,
                    symbol.clone(),
                    f32::from_bits(value.load(Ordering::Relaxed)),
                )
            })
            .collect()
    }
    fn running(&self) -> bool {
        !self.filter.is_null()
            && unsafe {
                pw::sys::pw_filter_get_state(self.filter, ptr::null_mut())
                    == pw::sys::pw_filter_state_PW_FILTER_STATE_STREAMING
            }
    }
    pub fn peaks(&self) -> Vec<(Id, f32)> {
        self.controls
            .iter()
            .map(|(id, c)| {
                (
                    *id,
                    if self.running() {
                        f32::from_bits(c.peak.load(Ordering::Relaxed))
                    } else {
                        0.0
                    },
                )
            })
            .collect()
    }
    pub fn take_peak(&self) -> f32 {
        // Read held event peaks even if the filter has already gone idle.
        self.controls.values()
            .map(|c| f32::from_bits(c.peak.swap(0, Ordering::Relaxed)))
            .fold(0.0, f32::max)
    }
    pub fn channel_peaks(&self) -> Vec<(Id, Vec<f32>)> {
        self.controls
            .iter()
            .map(|(id, c)| {
                (
                    *id,
                    c.channels
                        .iter()
                        .map(|p| {
                            if self.running() {
                                f32::from_bits(p.load(Ordering::Relaxed))
                            } else {
                                0.0
                            }
                        })
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
        self.reclaim();
        self._data.activate_pending();
        self.reclaim();
        let mut result = vec![];
        for (strip, chain) in &mut self._data.active.effects {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handover_defers_destruction_and_bounds_pending_work() {
        let rate = Arc::new(AtomicU32::new(48000));
        let make_plan = |scene: &Scene, previous: Option<&Scene>| {
            let processor = Processor::new(scene.clone(), 64).unwrap();
            let transfer = previous.map(|old| processor.prepare_transfer(old));
            Box::new(Plan {
                processor,
                inputs: vec![],
                outputs: vec![],
                controls: vec![],
                route_controls: vec![],
                effects: scene.strips.iter().map(|s| (s.id, vec![])).collect(),
                transfer,
                reuse_effects: vec![],
                rate: rate.clone(),
                expected_rate: 48000,
                overflow: Arc::new(AtomicBool::new(false)),
            })
        };
        let mut scene = Scene::default();
        scene
            .strips
            .push(Strip::new("Music", StripKind::Stream, vec!["MONO".into()]));
        let handoff = Arc::new(Handoff::default());
        let mut data = Data {
            metering: Arc::new(crate::Metering::default()),
            hold_peaks: false,
            active: make_plan(&scene, None),
            handoff: handoff.clone(),
        };
        let mut next = scene.clone();
        next.strips.push(Strip::new(
            "Browser",
            StripKind::Stream,
            vec!["MONO".into()],
        ));
        handoff.pending.store(
            Box::into_raw(make_plan(&next, Some(&scene))),
            Ordering::Release,
        );
        data.activate_pending();
        assert_eq!(data.active.effects.len(), 2);
        assert_eq!(
            Arc::strong_count(&rate),
            3,
            "retired allocations remain alive after the RT handover"
        );
        let mut last = next.clone();
        last.strips.push(Strip::new(
            "Another browser",
            StripKind::Stream,
            vec!["MONO".into()],
        ));
        handoff.pending.store(
            Box::into_raw(make_plan(&last, Some(&next))),
            Ordering::Release,
        );
        data.activate_pending();
        assert_eq!(
            data.active.effects.len(),
            2,
            "do not overwrite an unreclaimed retired plan"
        );
        let retired = handoff.retired.swap(ptr::null_mut(), Ordering::AcqRel);
        assert!(!retired.is_null());
        unsafe {
            drop(Box::from_raw(retired));
        }
        data.activate_pending();
        assert_eq!(data.active.effects.len(), 3);
        drop(data);
        drop(handoff);
        assert_eq!(
            Arc::strong_count(&rate),
            1,
            "shutdown reclaims all remaining plans"
        );
    }
}
