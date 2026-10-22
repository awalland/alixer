//! Allocations happen during compilation, not while rendering a prepared graph.
use crate::*;
pub const MAX_DELAY: usize = 16384;
struct Delay {
    samples: Vec<f32>,
    position: usize,
}
impl Delay {
    fn new() -> Self {
        Self {
            samples: vec![0.0; MAX_DELAY + 1],
            position: 0,
        }
    }
    fn sample(&mut self, input: f32, delay: usize) -> f32 {
        self.samples[self.position] = input;
        let value = self.samples[(self.position + self.samples.len() - delay) % self.samples.len()];
        self.position = (self.position + 1) % self.samples.len();
        value
    }
}

pub struct Processor {
    scene: Scene,
    order: Vec<usize>,
    routes: Vec<(usize, Option<usize>, Route)>,
    pre: Vec<Vec<Vec<f32>>>,
    post: Vec<Vec<Vec<f32>>>,
    gains: Vec<f32>,
    max_frames: usize,
    latencies: Vec<usize>,
    arrival: Vec<usize>,
    delays: Vec<Delay>,
    route_delays: Vec<usize>,
    route_gains: Vec<f32>,
    route_indices: BTreeMap<Id, usize>,
    outputs: Vec<(String, usize, Vec<f32>)>,
    route_outputs: Vec<Option<usize>>,
}

/// Index mappings prepared off the audio thread for a block-boundary handover.
pub struct RuntimeTransfer {
    strips: Vec<(usize, usize)>,
    routes: Vec<(usize, usize)>,
}

impl Processor {
    pub fn prepare_transfer(&self, previous: &Scene) -> RuntimeTransfer {
        RuntimeTransfer {
            strips: self
                .scene
                .strips
                .iter()
                .enumerate()
                .filter_map(|(new, s)| {
                    previous
                        .strips
                        .iter()
                        .position(|old| old.id == s.id)
                        .map(|old| (new, old))
                })
                .collect(),
            routes: self
                .scene
                .routes
                .iter()
                .enumerate()
                .filter_map(|(new, r)| {
                    previous
                        .routes
                        .iter()
                        .position(|old| {
                            old.id == r.id
                                && old.source == r.source
                                && old.destination == r.destination
                                && old.source_channel == r.source_channel
                                && old.target_channel == r.target_channel
                        })
                        .map(|old| (new, old))
                })
                .collect(),
        }
    }

    /// No allocation or destruction: preserve gain ramps and delay-line history.
    pub fn inherit_runtime(&mut self, previous: &mut Self, transfer: &RuntimeTransfer) {
        for &(new, old) in &transfer.strips {
            self.gains[new] = previous.gains[old];
            self.latencies[new] = previous.latencies[old];
        }
        for &(new, old) in &transfer.routes {
            self.route_gains[new] = previous.route_gains[old];
            std::mem::swap(&mut self.delays[new], &mut previous.delays[old]);
        }
    }

    pub fn new(scene: Scene, max_frames: usize) -> Result<Self, ModelError> {
        scene.validate()?;
        let order = scene
            .order()?
            .iter()
            .map(|id| scene.strips.iter().position(|s| s.id == *id).unwrap())
            .collect();
        let routes = scene
            .routes
            .iter()
            .map(|r| {
                (
                    scene.strips.iter().position(|s| s.id == r.source).unwrap(),
                    scene
                        .route_target(&r.destination)
                        .and_then(|id| scene.strips.iter().position(|s| s.id == id)),
                    r.clone(),
                )
            })
            .collect();
        let buffers = || {
            scene
                .strips
                .iter()
                .map(|s| s.channels.iter().map(|_| vec![0.0; max_frames]).collect())
                .collect()
        };
        let mut outputs: Vec<(String, usize, Vec<f32>)> = vec![];
        let route_outputs = scene
            .routes
            .iter()
            .map(|r| {
                if let Destination::Output(name) = &r.destination {
                    let index = outputs
                        .iter()
                        .position(|(key, ch, _)| key == name && *ch == r.target_channel)
                        .unwrap_or_else(|| {
                            outputs.push((name.clone(), r.target_channel, vec![0.0; max_frames]));
                            outputs.len() - 1
                        });
                    Some(index)
                } else {
                    None
                }
            })
            .collect();
        Ok(Self {
            pre: buffers(),
            post: buffers(),
            gains: vec![0.0; scene.strips.len()],
            latencies: vec![0; scene.strips.len()],
            arrival: vec![0; scene.strips.len()],
            delays: scene.routes.iter().map(|_| Delay::new()).collect(),
            route_delays: vec![0; scene.routes.len()],
            route_gains: scene.routes.iter().map(|r| r.gain).collect(),
            route_indices: scene
                .routes
                .iter()
                .enumerate()
                .map(|(i, r)| (r.id, i))
                .collect(),
            outputs,
            route_outputs,
            scene,
            order,
            routes,
            max_frames,
        })
    }
    pub fn clear(&mut self) {
        self.clear_frames(self.max_frames);
    }
    pub fn clear_frames(&mut self, frames: usize) {
        assert!(frames <= self.max_frames);
        for strip in &mut self.pre {
            for channel in strip {
                channel[..frames].fill(0.0);
            }
        }
    }
    pub fn input(&mut self, strip: Id, channel: usize) -> Option<&mut [f32]> {
        let index = self.scene.strips.iter().position(|s| s.id == strip)?;
        self.pre
            .get_mut(index)?
            .get_mut(channel)
            .map(Vec::as_mut_slice)
    }
    pub fn set_controls(&mut self, id: Id, gain: f32, mute: bool, solo: bool) {
        if let Some(s) = self.scene.strips.iter_mut().find(|s| s.id == id) {
            s.gain = gain;
            s.mute = mute;
            s.solo = solo;
        }
    }
    pub fn set_route_controls(&mut self, id: Id, gain: f32, tap: Tap) {
        if let Some(index) = self.route_indices.get(&id) {
            let route = &mut self.routes[*index].2;
            route.gain = gain;
            route.tap = tap;
        }
    }
    pub fn set_latency(&mut self, id: Id, samples: usize) {
        if let Some(i) = self.scene.strips.iter().position(|s| s.id == id) {
            self.latencies[i] = samples;
        }
    }
    pub fn latency_overflow(&self) -> bool {
        self.route_delays.iter().any(|d| *d > MAX_DELAY)
    }
    pub fn process(&mut self, frames: usize, rate: f32) {
        self.process_with(frames, rate, |_, _, _| {});
    }
    pub fn process_with(
        &mut self,
        frames: usize,
        rate: f32,
        mut effect: impl FnMut(Id, &mut [Vec<f32>], usize),
    ) {
        assert!(frames <= self.max_frames);
        for (_, _, buffer) in &mut self.outputs {
            buffer[..frames].fill(0.0);
        }
        self.arrival.fill(0);
        for &i in &self.order {
            let input = self
                .routes
                .iter()
                .filter(|(_, target, _)| *target == Some(i))
                .map(|(source, _, _)| self.arrival[*source])
                .max()
                .unwrap_or(0);
            self.arrival[i] = input.saturating_add(self.latencies[i]);
        }
        for (index, (source, target, route)) in self.routes.iter().enumerate() {
            let target_time = if let Some(target) = target {
                self.arrival[*target].saturating_sub(self.latencies[*target])
            } else {
                self.routes
                    .iter()
                    .filter(|(_, _, r)| r.destination == route.destination)
                    .map(|(s, _, _)| self.arrival[*s])
                    .max()
                    .unwrap_or(0)
            };
            self.route_delays[index] = target_time.saturating_sub(self.arrival[*source]);
        }
        for &i in &self.order {
            let strip = &self.scene.strips[i];
            effect(strip.id, &mut self.pre[i], frames);
            // Output gain/mute belong to the device. Keep DSP and its meters pre-device.
            let audible = strip.kind == StripKind::Output || self.scene.audible(strip);
            let target = if strip.kind == StripKind::Output {
                1.0
            } else if audible {
                strip.gain
            } else {
                0.0
            };
            let step = 1.0 / (rate * 0.005).max(1.0);
            for frame in 0..frames {
                self.gains[i] += (target - self.gains[i]).clamp(-step, step);
                for ch in 0..strip.channels.len() {
                    if !audible {
                        self.pre[i][ch][frame] = 0.0;
                    }
                    self.post[i][ch][frame] = self.pre[i][ch][frame] * self.gains[i];
                }
            }
            for (index, (source, target, route)) in self.routes.iter().enumerate() {
                if *source != i {
                    continue;
                }
                for f in 0..frames {
                    let value = if route.tap == Tap::Pre {
                        self.pre[i][route.source_channel][f]
                    } else {
                        self.post[i][route.source_channel][f]
                    };
                    self.route_gains[index] +=
                        (route.gain - self.route_gains[index]).clamp(-step, step);
                    let value = self.delays[index]
                        .sample(value, self.route_delays[index].min(MAX_DELAY))
                        * self.route_gains[index];
                    if let Some(target) = *target {
                        if let Some(buffer) = self.pre[target].get_mut(route.target_channel) {
                            buffer[f] += value;
                        }
                    } else if let Some(target) = self.route_outputs[index] {
                        self.outputs[target].2[f] += value;
                    }
                }
            }
        }
    }
    pub fn output(&self, name: &str, channel: usize, output: &mut [f32]) {
        output.fill(0.0);
        if self.latency_overflow() {
            return;
        }
        if let Some(index) = self
            .scene
            .strips
            .iter()
            .position(|s| s.output_key.as_deref() == Some(name))
        {
            if let Some(buffer) = self.post[index].get(channel) {
                output.copy_from_slice(&buffer[..output.len()]);
            }
            return;
        }
        if let Some((_, _, buffer)) = self
            .outputs
            .iter()
            .find(|(key, ch, _)| key == name && *ch == channel)
        {
            output.copy_from_slice(&buffer[..output.len()]);
        }
    }
    pub fn peak(&self, id: Id, frames: usize) -> f32 {
        self.scene
            .strips
            .iter()
            .position(|s| s.id == id)
            .map(|i| {
                self.post[i]
                    .iter()
                    .flat_map(|c| c[..frames].iter())
                    .fold(0.0_f32, |p, v| p.max(v.abs()))
            })
            .unwrap_or(0.0)
    }
    pub fn channel_peak(&self, id: Id, channel: usize, frames: usize) -> f32 {
        self.scene
            .strips
            .iter()
            .position(|s| s.id == id)
            .and_then(|i| self.post[i].get(channel))
            .map(|b| b[..frames].iter().fold(0.0_f32, |p, v| p.max(v.abs())))
            .unwrap_or(0.0)
    }
    pub fn peaks(&self) -> Vec<(Id, f32)> {
        self.scene
            .strips
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    s.id,
                    self.post[i]
                        .iter()
                        .flatten()
                        .fold(0.0_f32, |p, v| p.max(v.abs())),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clearing_variable_blocks_does_not_replay_stale_input() {
        let source = Strip::new("input", StripKind::Stream, vec!["MONO".into()]);
        let id = source.id;
        let mut scene = Scene::default();
        scene.strips.push(source);
        scene.routes.push(Route {
            id: Id::new_v4(),
            source: id,
            destination: Destination::Output("out".into()),
            source_channel: 0,
            target_channel: 0,
            gain: 1.0,
            tap: Tap::Post,
        });
        let mut dsp = Processor::new(scene, 512).unwrap();
        dsp.input(id, 0).unwrap().fill(1.0);
        dsp.process(512, 48000.0);
        // A disconnected input, first at a small quantum and then a larger one.
        for frames in [32, 512, 64] {
            dsp.clear_frames(frames);
            dsp.process(frames, 48000.0);
            let mut output = vec![1.0; frames];
            dsp.output("out", 0, &mut output);
            assert!(output.iter().all(|&v| v == 0.0));
        }
    }
    #[test]
    fn plan_handover_preserves_ramps_and_latency_history() {
        let mut scene = Scene::default();
        for name in ["slow", "dry"] {
            let source = Strip::new(name, StripKind::Stream, vec!["MONO".into()]);
            scene.routes.push(Route {
                id: Id::new_v4(),
                source: source.id,
                destination: Destination::Output("out".into()),
                source_channel: 0,
                target_channel: 0,
                gain: 0.5,
                tap: Tap::Post,
            });
            scene.strips.push(source);
        }
        let ids: Vec<_> = scene.strips.iter().map(|s| s.id).collect();
        let mut original = Processor::new(scene.clone(), 8).unwrap();
        let mut reference = Processor::new(scene.clone(), 8).unwrap();
        let feed = |dsp: &mut Processor, block: usize| {
            dsp.clear();
            dsp.set_latency(ids[0], 21);
            for (channel, id) in ids.iter().enumerate() {
                for (i, value) in dsp.input(*id, 0).unwrap().iter_mut().enumerate() {
                    *value = ((block * 8 + i + channel * 7) % 13) as f32 / 13.0;
                }
            }
            dsp.process(8, 48000.0);
        };
        for block in 0..12 {
            feed(&mut original, block);
            feed(&mut reference, block);
        }
        scene.strips.push(Strip::new(
            "browser",
            StripKind::Stream,
            vec!["MONO".into()],
        ));
        scene.strips.reverse();
        scene.routes.reverse();
        let mut next = Processor::new(scene.clone(), 8).unwrap();
        let transfer = next.prepare_transfer(&original.scene);
        next.inherit_runtime(&mut original, &transfer);
        for block in 12..40 {
            feed(&mut next, block);
            feed(&mut reference, block);
            let (mut actual, mut expected) = ([0.0; 8], [0.0; 8]);
            next.output("out", 0, &mut actual);
            reference.output("out", 0, &mut expected);
            assert_eq!(
                actual, expected,
                "block {block}: a graph edit must not reset live state"
            );
        }
    }
    #[test]
    fn output_inserts_process_the_sum_once_and_device_controls_do_not_attenuate_dsp() {
        let mut scene = Scene::default();
        let mut master = Strip::new(
            "Speakers",
            StripKind::Output,
            vec!["FL".into(), "FR".into()],
        );
        master.output_key = Some("speakers".into());
        master.gain = 0.1;
        master.mute = true;
        let output_id = master.id;
        scene.strips.push(master);
        let mut inputs = vec![];
        for name in ["A", "B"] {
            let s = Strip::new(name, StripKind::Stream, vec!["MONO".into()]);
            inputs.push(s.id);
            scene.routes.push(Route {
                id: Id::new_v4(),
                source: s.id,
                destination: Destination::Output("speakers".into()),
                source_channel: 0,
                target_channel: 0,
                gain: 1.0,
                tap: Tap::Post,
            });
            scene.strips.push(s);
        }
        let mut dsp = Processor::new(scene, 512).unwrap();
        for id in inputs {
            dsp.input(id, 0).unwrap().fill(0.25);
        }
        let mut calls = 0;
        dsp.process_with(512, 48000.0, |id, channels, frames| {
            if id == output_id {
                calls += 1;
                assert!((channels[0][511] - 0.5).abs() < 0.001);
                for channel in channels {
                    for sample in &mut channel[..frames] {
                        *sample *= 0.5;
                    }
                }
            }
        });
        let mut output = [0.0; 512];
        dsp.output("speakers", 0, &mut output);
        assert_eq!(calls, 1);
        assert!((output[511] - 0.25).abs() < 0.001);
        assert!((dsp.channel_peak(output_id, 0, 512) - 0.25).abs() < 0.001);
        assert_eq!(dsp.channel_peak(output_id, 1, 512), 0.0);
    }
    #[test]
    fn parallel_paths_are_latency_aligned() {
        let mut scene = Scene::default();
        for name in ["slow", "dry"] {
            let s = Strip::new(name, StripKind::Stream, vec!["MONO".into()]);
            scene.routes.push(Route {
                id: Id::new_v4(),
                source: s.id,
                destination: Destination::Output("out".into()),
                source_channel: 0,
                target_channel: 0,
                gain: 1.0,
                tap: Tap::Post,
            });
            scene.strips.push(s);
        }
        let (a, b) = (scene.strips[0].id, scene.strips[1].id);
        let mut dsp = Processor::new(scene, 16).unwrap();
        dsp.set_latency(a, 2);
        dsp.input(a, 0).unwrap()[2] = 1.0;
        dsp.input(b, 0).unwrap()[0] = 1.0;
        dsp.process(16, 100.0);
        let mut output = [0.0; 16];
        dsp.output("out", 0, &mut output);
        assert_eq!(&output[..3], &[0.0, 0.0, 2.0]);
    }
    #[test]
    fn prefader_and_postfader_are_independent_and_solo_cuts_both() {
        let mut scene = Scene::default();
        let mut source = Strip::new("source", StripKind::Stream, vec!["MONO".into()]);
        source.gain = 0.5;
        let id = source.id;
        scene.strips.push(source);
        for (name, tap) in [("pre", Tap::Pre), ("post", Tap::Post)] {
            scene.routes.push(Route {
                id: Id::new_v4(),
                source: id,
                destination: Destination::Output(name.into()),
                source_channel: 0,
                target_channel: 0,
                gain: 1.0,
                tap,
            });
        }
        let mut dsp = Processor::new(scene.clone(), 512).unwrap();
        dsp.input(id, 0).unwrap().fill(1.0);
        dsp.process(512, 48000.0);
        let mut out = vec![0.0; 512];
        dsp.output("pre", 0, &mut out);
        assert_eq!(out[511], 1.0);
        dsp.output("post", 0, &mut out);
        assert!((out[511] - 0.5).abs() < 0.001);
        let mut solo = Strip::new("solo", StripKind::Stream, vec!["MONO".into()]);
        solo.solo = true;
        scene.strips.push(solo);
        let mut dsp = Processor::new(scene, 512).unwrap();
        dsp.input(id, 0).unwrap().fill(1.0);
        dsp.process(512, 48000.0);
        dsp.output("pre", 0, &mut out);
        assert_eq!(out[511], 0.0);
    }
}
