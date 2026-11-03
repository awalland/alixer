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
    fn mix(&mut self, input: &[f32], output: &mut [f32], delay: usize, gain: &mut f32, target: f32, step: f32) {
        // Read the old history before appending this block. Once the delay has
        // elapsed, the remaining samples come directly from this block's input.
        // At zero delay this is just one vectorizable buffer addition.
        let past = delay.min(input.len());
        if past > 0 {
            let start = (self.position + self.samples.len() - delay) % self.samples.len();
            let first = past.min(self.samples.len() - start);
            add_gain(&self.samples[start..start + first], &mut output[..first], gain, target, step);
            add_gain(&self.samples[..past - first], &mut output[first..past], gain, target, step);
        }
        add_gain(&input[..input.len() - past], &mut output[past..], gain, target, step);

        // Keep history even at zero delay: a plugin may change latency next
        // block. Bulk copies preserve that history without per-sample wrapping.
        let mut remaining = input;
        while !remaining.is_empty() {
            let count = remaining.len().min(self.samples.len() - self.position);
            self.samples[self.position..self.position + count].copy_from_slice(&remaining[..count]);
            self.position += count;
            if self.position == self.samples.len() {
                self.position = 0;
            }
            remaining = &remaining[count..];
        }
    }
}

/// Slice loops expose independent samples to the compiler's SIMD vectorizer.
fn copy_gain(input: &[f32], output: &mut [f32], gain: f32) {
    if gain == 1.0 {
        output.copy_from_slice(input);
    } else {
        for (dst, &src) in output.iter_mut().zip(input) {
            *dst = src * gain;
        }
    }
}

fn add_gain(input: &[f32], output: &mut [f32], gain: &mut f32, target: f32, step: f32) {
    let mut ramp = 0;
    while ramp < input.len() && *gain != target {
        *gain += (target - *gain).clamp(-step, step);
        output[ramp] += input[ramp] * *gain;
        ramp += 1;
    }
    let (input, output) = (&input[ramp..], &mut output[ramp..]);
    if target == 1.0 {
        for (dst, &src) in output.iter_mut().zip(input) {
            *dst += src;
        }
    } else {
        for (dst, &src) in output.iter_mut().zip(input) {
            *dst += src * target;
        }
    }
}

fn source_and_target(buffers: &mut [Vec<Vec<f32>>], source: usize, target: usize) -> (&[Vec<f32>], &mut [Vec<f32>]) {
    // Scene validation forbids self-routes and cycles.
    if source < target {
        let (before, after) = buffers.split_at_mut(target);
        (&before[source], &mut after[0])
    } else {
        let (before, after) = buffers.split_at_mut(source);
        (&after[0], &mut before[target])
    }
}

pub struct Processor {
    scene: Scene,
    order: Vec<usize>,
    routes: Vec<(usize, Option<usize>, Route)>,
    active_routes: Vec<usize>,
    outgoing: Vec<Vec<usize>>,
    incoming: Vec<Vec<usize>>,
    strip_indices: BTreeMap<Id, usize>,
    pre: Vec<Vec<Vec<f32>>>,
    post: Vec<Vec<Vec<f32>>>,
    gains: Vec<f32>,
    max_frames: usize,
    latencies: Vec<usize>,
    arrival: Vec<usize>,
    external_groups: Vec<Vec<usize>>,
    latency_dirty: bool,
    latency_overflow: bool,
    gain_envelope: Vec<f32>,
    delays: Vec<Option<Delay>>,
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
                .filter_map(|(new, s)| previous.strips.iter().position(|old| old.id == s.id).map(|old| (new, old)))
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
            if self.delays[new].is_some() && previous.delays[old].is_some() {
                std::mem::swap(&mut self.delays[new], &mut previous.delays[old]);
            }
        }
        self.latency_dirty = true;
    }

    pub fn new(scene: Scene, max_frames: usize) -> Result<Self, ModelError> {
        Self::compile(scene, max_frames, None)
    }
    /// Keep saved indices stable for handovers, but allocate and process only
    /// present playback streams. Buses, hardware inputs and outputs stay live.
    pub fn new_active(scene: Scene, max_frames: usize, streams: &BTreeSet<Id>) -> Result<Self, ModelError> {
        Self::compile(scene, max_frames, Some(streams))
    }
    fn compile(scene: Scene, max_frames: usize, streams: Option<&BTreeSet<Id>>) -> Result<Self, ModelError> {
        scene.validate()?;
        let enabled = |s: &Strip| s.kind != StripKind::Stream || streams.is_none_or(|ids| ids.contains(&s.id));
        let order = scene
            .order()?
            .iter()
            .map(|id| scene.strips.iter().position(|s| s.id == *id).unwrap())
            .filter(|&i| enabled(&scene.strips[i]))
            .collect();
        let routes: Vec<_> = scene
            .routes
            .iter()
            .map(|r| {
                (
                    scene.strips.iter().position(|s| s.id == r.source).unwrap(),
                    scene.route_target(&r.destination).and_then(|id| scene.strips.iter().position(|s| s.id == id)),
                    r.clone(),
                )
            })
            .collect();
        let active_routes: Vec<_> = routes
            .iter()
            .enumerate()
            .filter(|(_, (source, target, _))| enabled(&scene.strips[*source]) && target.is_none_or(|i| enabled(&scene.strips[i])))
            .map(|(i, _)| i)
            .collect();
        let mut outgoing = vec![vec![]; scene.strips.len()];
        let mut incoming = vec![vec![]; scene.strips.len()];
        for &index in &active_routes {
            outgoing[routes[index].0].push(index);
            if let Some(target) = routes[index].1 {
                incoming[target].push(index);
            }
        }
        let mut external_groups: BTreeMap<Destination, Vec<usize>> = BTreeMap::new();
        for &index in &active_routes {
            let (_, target, route) = &routes[index];
            if target.is_none() {
                external_groups.entry(route.destination.clone()).or_default().push(index);
            }
        }
        let buffers = || {
            scene
                .strips
                .iter()
                .map(|s| {
                    if enabled(s) {
                        s.channels.iter().map(|_| vec![0.0; max_frames]).collect()
                    } else {
                        vec![]
                    }
                })
                .collect()
        };
        let mut outputs: Vec<(String, usize, Vec<f32>)> = vec![];
        let route_outputs = scene
            .routes
            .iter()
            .map(|r| {
                if let Destination::Output(name) = &r.destination {
                    let index = outputs.iter().position(|(key, ch, _)| key == name && *ch == r.target_channel).unwrap_or_else(|| {
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
            external_groups: external_groups.into_values().collect(),
            latency_dirty: true,
            latency_overflow: false,
            gain_envelope: vec![0.0; max_frames],
            delays: routes
                .iter()
                .map(|(source, target, _)| (enabled(&scene.strips[*source]) && target.is_none_or(|i| enabled(&scene.strips[i]))).then(Delay::new))
                .collect(),
            route_delays: vec![0; scene.routes.len()],
            route_gains: scene.routes.iter().map(|r| r.gain).collect(),
            route_indices: scene.routes.iter().enumerate().map(|(i, r)| (r.id, i)).collect(),
            outputs,
            route_outputs,
            strip_indices: scene.strips.iter().enumerate().map(|(i, s)| (s.id, i)).collect(),
            scene,
            order,
            routes,
            active_routes,
            outgoing,
            incoming,
            max_frames,
        })
    }
    pub fn clear(&mut self) {
        self.clear_frames(self.max_frames);
    }
    pub fn clear_frames(&mut self, frames: usize) {
        assert!(frames <= self.max_frames);
        for &i in &self.order {
            for channel in &mut self.pre[i] {
                channel[..frames].fill(0.0);
            }
        }
    }
    pub fn input(&mut self, strip: Id, channel: usize) -> Option<&mut [f32]> {
        let index = *self.strip_indices.get(&strip)?;
        self.pre.get_mut(index)?.get_mut(channel).map(Vec::as_mut_slice)
    }
    pub fn set_controls(&mut self, id: Id, gain: f32, mute: bool, solo: bool) {
        if let Some(&i) = self.strip_indices.get(&id) {
            let s = &mut self.scene.strips[i];
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
        if let Some(&i) = self.strip_indices.get(&id) {
            if self.latencies[i] != samples {
                self.latencies[i] = samples;
                self.latency_dirty = true;
            }
        }
    }
    pub fn latency_overflow(&self) -> bool {
        self.latency_overflow
    }
    pub fn process(&mut self, frames: usize, rate: f32) {
        self.process_with(frames, rate, |_, _, _| {});
    }
    pub fn process_with(&mut self, frames: usize, rate: f32, mut effect: impl FnMut(Id, &mut [Vec<f32>], usize)) {
        assert!(frames <= self.max_frames);
        for (_, _, buffer) in &mut self.outputs {
            buffer[..frames].fill(0.0);
        }
        if self.latency_dirty {
            self.update_latency();
        }
        let any_solo = self.scene.strips.iter().any(|s| s.solo);
        let step = 1.0 / (rate * 0.005).max(1.0);
        for &i in &self.order {
            let strip = &self.scene.strips[i];
            effect(strip.id, &mut self.pre[i], frames);
            // Output gain/mute belong to the device. Keep DSP and its meters pre-device.
            let audible = strip.kind == StripKind::Output || (!strip.mute && (!matches!(strip.kind, StripKind::Stream | StripKind::Input) || !any_solo || strip.solo));
            let target = if strip.kind == StripKind::Output {
                1.0
            } else if audible {
                strip.gain
            } else {
                0.0
            };
            let mut ramp = 0;
            while ramp < frames && self.gains[i] != target {
                self.gains[i] += (target - self.gains[i]).clamp(-step, step);
                self.gain_envelope[ramp] = self.gains[i];
                ramp += 1;
            }
            for (input, output) in self.pre[i].iter_mut().zip(&mut self.post[i]) {
                if !audible {
                    input[..frames].fill(0.0);
                }
                for ((dst, &src), &gain) in output[..ramp].iter_mut().zip(input[..ramp].iter()).zip(&self.gain_envelope[..ramp]) {
                    *dst = src * gain;
                }
                copy_gain(&input[ramp..frames], &mut output[ramp..frames], target);
            }
            for &index in &self.outgoing[i] {
                let (_, target, route) = &self.routes[index];
                let (pre, output) = if let Some(target) = *target {
                    let (pre, destination) = source_and_target(&mut self.pre, i, target);
                    (pre, &mut destination[route.target_channel][..frames])
                } else {
                    let target = self.route_outputs[index].expect("external route has output storage");
                    (&self.pre[i][..], &mut self.outputs[target].2[..frames])
                };
                let input = match route.tap {
                    Tap::Pre => &pre[route.source_channel][..frames],
                    Tap::Post => &self.post[i][route.source_channel][..frames],
                };
                self.delays[index].as_mut().expect("active route has delay storage").mix(
                    input,
                    output,
                    self.route_delays[index].min(MAX_DELAY),
                    &mut self.route_gains[index],
                    route.gain,
                    step,
                );
            }
        }
    }
    fn update_latency(&mut self) {
        self.arrival.fill(0);
        for &i in &self.order {
            let input = self.incoming[i].iter().map(|&route| self.arrival[self.routes[route].0]).max().unwrap_or(0);
            self.arrival[i] = input.saturating_add(self.latencies[i]);
        }
        for &index in &self.active_routes {
            let (source, target, _) = &self.routes[index];
            if let Some(target) = target {
                let target_time = self.arrival[*target].saturating_sub(self.latencies[*target]);
                self.route_delays[index] = target_time.saturating_sub(self.arrival[*source]);
            }
        }
        for group in &self.external_groups {
            let target_time = group.iter().map(|&index| self.arrival[self.routes[index].0]).max().unwrap_or(0);
            for &index in group {
                self.route_delays[index] = target_time.saturating_sub(self.arrival[self.routes[index].0]);
            }
        }
        self.latency_overflow = self.active_routes.iter().any(|&i| self.route_delays[i] > MAX_DELAY);
        self.latency_dirty = false;
    }
    pub fn output(&self, name: &str, channel: usize, output: &mut [f32]) {
        output.fill(0.0);
        if self.latency_overflow() {
            return;
        }
        if let Some(index) = self.scene.strips.iter().position(|s| s.output_key.as_deref() == Some(name)) {
            if let Some(buffer) = self.post[index].get(channel) {
                output.copy_from_slice(&buffer[..output.len()]);
            }
            return;
        }
        if let Some((_, _, buffer)) = self.outputs.iter().find(|(key, ch, _)| key == name && *ch == channel) {
            output.copy_from_slice(&buffer[..output.len()]);
        }
    }
    pub fn peak(&self, id: Id, frames: usize) -> f32 {
        self.strip_indices
            .get(&id)
            .map(|&i| self.post[i].iter().flat_map(|c| c[..frames].iter()).fold(0.0_f32, |p, v| p.max(v.abs())))
            .unwrap_or(0.0)
    }
    pub fn channel_peak(&self, id: Id, channel: usize, frames: usize) -> f32 {
        self.strip_indices
            .get(&id)
            .and_then(|&i| self.post[i].get(channel))
            .map(|b| b[..frames].iter().fold(0.0_f32, |p, v| p.max(v.abs())))
            .unwrap_or(0.0)
    }
    pub fn peaks(&self) -> Vec<(Id, f32)> {
        self.scene
            .strips
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, self.post[i].iter().flatten().fold(0.0_f32, |p, v| p.max(v.abs()))))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn block_delay_and_gain_match_scalar_reference_through_wraps_and_latency_changes() {
        let mut delay = Delay::new();
        let mut history = vec![0.0; MAX_DELAY + 1];
        let mut position = 0;
        let (mut gain, mut reference_gain) = (0.0_f32, 0.0_f32);
        let step = 1.0 / 240.0;
        let mut sequence = 0;
        for round in 0..80 {
            // Include blocks larger than the history, fractional gains, ramps
            // spanning blocks, and zero/nonzero latency transitions mid-stream.
            let frames = [0, 1, 17, 128, 511, 8192, 40000][round % 7];
            let latency = [0, 1, MAX_DELAY, 5, 0, 511][round % 6];
            let target = [1.0, 0.75, 0.0, 2.5, 0.01][round % 5];
            let input: Vec<_> = (0..frames).map(|i| ((sequence + i * 13) % 257) as f32 / 128.0 - 1.0).collect();
            sequence += frames;
            let mut actual = vec![0.125; frames];
            let mut expected = actual.clone();
            for (dst, &src) in expected.iter_mut().zip(&input) {
                reference_gain += (target - reference_gain).clamp(-step, step);
                history[position] = src;
                *dst += history[(position + history.len() - latency) % history.len()] * reference_gain;
                position = (position + 1) % history.len();
            }
            delay.mix(&input, &mut actual, latency, &mut gain, target, step);
            assert_eq!(actual, expected, "round {round}: delay {latency}, frames {frames}");
            assert_eq!(gain, reference_gain);
            assert_eq!(delay.position, position);
            assert_eq!(delay.samples, history);
        }
    }

    #[test]
    fn block_faders_match_scalar_samples_across_gain_mute_and_tap_changes() {
        let mut source = Strip::new("source", StripKind::Stream, vec!["FL".into(), "FR".into()]);
        source.gain = 0.75;
        let id = source.id;
        let mut scene = Scene {
            strips: vec![source],
            ..Default::default()
        };
        for ch in 0..2 {
            scene.routes.push(Route {
                id: Id::new_v4(),
                source: id,
                destination: Destination::Output("out".into()),
                source_channel: ch,
                target_channel: ch,
                gain: 1.0,
                tap: Tap::Post,
            });
        }
        let routes: Vec<_> = scene.routes.iter().map(|r| r.id).collect();
        let mut dsp = Processor::new(scene, 511).unwrap();
        let mut fader = 0.0_f32;
        let mut route_gains = [1.0_f32; 2];
        for round in 0..40 {
            let frames = [17, 128, 511][round % 3];
            let target = [0.75, 1.0, 0.0, 2.0][round % 4];
            let muted = round % 7 == 0;
            let tap = if round % 2 == 0 { Tap::Pre } else { Tap::Post };
            let route_target = if round % 3 == 0 { 0.6 } else { 1.0 };
            let step = 1.0 / 240.0;
            dsp.clear_frames(frames);
            dsp.set_controls(id, target, muted, false);
            for (ch, route) in routes.iter().enumerate() {
                dsp.set_route_controls(*route, route_target, tap);
                for (i, value) in dsp.input(id, ch).unwrap()[..frames].iter_mut().enumerate() {
                    *value = ((round + i * 13 + ch * 7) % 257) as f32 / 128.0 - 1.0;
                }
            }
            let mut expected = vec![vec![0.0; frames]; 2];
            for i in 0..frames {
                let audible_target = if muted { 0.0 } else { target };
                fader += (audible_target - fader).clamp(-step, step);
                for ch in 0..2 {
                    let pre = if muted { 0.0 } else { dsp.input(id, ch).unwrap()[i] };
                    let sample = if tap == Tap::Pre { pre } else { pre * fader };
                    route_gains[ch] += (route_target - route_gains[ch]).clamp(-step, step);
                    expected[ch][i] += sample * route_gains[ch];
                }
            }
            dsp.process(frames, 48000.0);
            for (ch, expected) in expected.iter().enumerate() {
                let mut output = vec![0.0; frames];
                dsp.output("out", ch, &mut output);
                assert_eq!(&output, expected, "round {round}, channel {ch}");
            }
        }
    }

    #[test]
    fn compensation_updates_and_recovers_from_overflow_with_zero_delay_history() {
        let mut scene = Scene::default();
        for name in ["slow", "dry"] {
            let strip = Strip::new(name, StripKind::Stream, vec!["MONO".into()]);
            scene.routes.push(Route {
                id: Id::new_v4(),
                source: strip.id,
                destination: Destination::Output("out".into()),
                source_channel: 0,
                target_channel: 0,
                gain: 1.0,
                tap: Tap::Post,
            });
            scene.strips.push(strip);
        }
        let (slow, dry) = (scene.strips[0].id, scene.strips[1].id);
        let mut dsp = Processor::new(scene, 8).unwrap();
        let mut samples = vec![];
        for latency in [0, 0, 3, 3, 17, MAX_DELAY + 1, 0, 5] {
            let start = samples.len();
            dsp.clear();
            dsp.set_latency(slow, latency);
            for i in 0..8 {
                let value = (start + i + 1) as f32;
                samples.push(value);
                dsp.input(dry, 0).unwrap()[i] = value;
            }
            dsp.process(8, 100.0);
            let mut output = [0.0; 8];
            dsp.output("out", 0, &mut output);
            assert_eq!(dsp.latency_overflow(), latency > MAX_DELAY);
            for (i, &value) in output.iter().enumerate() {
                let expected = if latency > MAX_DELAY {
                    0.0
                } else {
                    (start + i).checked_sub(latency).map(|i| samples[i]).unwrap_or(0.0)
                };
                assert_eq!(value, expected, "latency {latency}, sample {i}");
            }
        }
    }

    #[test]
    fn absent_playback_has_no_buffers_delays_or_effect_calls_and_can_return() {
        let mut scene = Scene::default();
        for _ in 0..600 {
            let strip = Strip::new("browser", StripKind::Stream, vec!["MONO".into()]);
            scene.routes.push(Route {
                id: Id::new_v4(),
                source: strip.id,
                destination: Destination::Output("speakers".into()),
                source_channel: 0,
                target_channel: 0,
                gain: 1.0,
                tap: Tap::Post,
            });
            scene.strips.push(strip);
        }
        let active = scene.strips[0].id;
        let absent = scene.strips[1].id;
        let mut dsp = Processor::new_active(scene.clone(), 512, &BTreeSet::from([active])).unwrap();
        assert_eq!(dsp.pre.iter().filter(|b| !b.is_empty()).count(), 1);
        assert_eq!(dsp.delays.iter().filter(|d| d.is_some()).count(), 1);
        assert!(dsp.input(absent, 0).is_none());
        dsp.input(active, 0).unwrap().fill(1.0);
        // An offline effect's old latency must not delay or silence the live mix.
        dsp.set_latency(absent, MAX_DELAY * 2);
        let mut calls = vec![];
        dsp.process_with(512, 48000.0, |id, _, _| calls.push(id));
        assert_eq!(calls, vec![active]);
        assert!(!dsp.latency_overflow());
        assert_eq!(dsp.peak(absent, 512), 0.0);
        let mut output = [0.0; 512];
        dsp.output("speakers", 0, &mut output);
        assert_eq!(output[511], 1.0);

        let mut next = Processor::new_active(scene.clone(), 512, &BTreeSet::from([absent])).unwrap();
        next.inherit_runtime(&mut dsp, &next.prepare_transfer(&scene));
        next.input(absent, 0).unwrap().fill(0.5);
        next.process(512, 48000.0);
        next.output("speakers", 0, &mut output);
        assert_eq!(output[511], 0.5, "returning routes need fresh delay storage");
        assert_eq!(next.peak(active, 512), 0.0);
    }
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
        scene.strips.push(Strip::new("browser", StripKind::Stream, vec!["MONO".into()]));
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
            assert_eq!(actual, expected, "block {block}: a graph edit must not reset live state");
        }
    }
    #[test]
    fn output_inserts_process_the_sum_once_and_device_controls_do_not_attenuate_dsp() {
        let mut scene = Scene::default();
        let mut master = Strip::new("Speakers", StripKind::Output, vec!["FL".into(), "FR".into()]);
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
