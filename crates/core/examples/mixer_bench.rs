//! Run with: cargo run --release -p alixer-core --example mixer_bench
//! Includes buffer clearing, input copies, control updates, mixing and output copy.
//! Excludes plugins, PipeWire, metering, graph compilation and service/UI work.
use alixer_core::{dsp::Processor, *};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn main() {
    println!("48 kHz stereo, all streams active, no effects; wall time as % of one core");
    for frames in [128, 2048] {
        for (count, gain) in [(1, 1.0), (100, 1.0), (600, 1.0), (100, 0.75)] {
            let mut scene = Scene::default();
            let mut output = Strip::new("out", StripKind::Output, vec!["FL".into(), "FR".into()]);
            output.output_key = Some("out".into());
            scene.strips.push(output);
            let mut ids = vec![];
            for _ in 0..count {
                let mut strip = Strip::new("input", StripKind::Stream, vec!["FL".into(), "FR".into()]);
                strip.gain = gain;
                ids.push(strip.id);
                for ch in 0..2 {
                    scene.routes.push(Route {
                        id: Id::new_v4(),
                        source: strip.id,
                        destination: Destination::Output("out".into()),
                        source_channel: ch,
                        target_channel: ch,
                        gain,
                        tap: Tap::Post,
                    });
                }
                scene.strips.push(strip);
            }
            let routes: Vec<_> = scene.routes.iter().map(|r| r.id).collect();
            let mut processor = Processor::new(scene, frames).unwrap();
            let input: Vec<f32> = (0..frames).map(|i| ((i * 13 % 257) as f32 - 128.0) / (128.0 * count as f32)).collect();
            let mut output = vec![0.0; frames];
            let mut block = || {
                processor.clear_frames(frames);
                for &id in &ids {
                    processor.set_controls(id, gain, false, false);
                    for ch in 0..2 {
                        processor.input(id, ch).unwrap().copy_from_slice(black_box(&input));
                    }
                }
                for &id in &routes {
                    processor.set_route_controls(id, gain, Tap::Post);
                }
                processor.process(frames, 48000.0);
                for ch in 0..2 {
                    processor.output("out", ch, &mut output);
                    black_box(&output);
                }
            };
            for _ in 0..16 {
                block();
            }
            let started = Instant::now();
            let mut blocks = 0;
            while started.elapsed() < Duration::from_millis(200) {
                block();
                blocks += 1;
            }
            let seconds = started.elapsed().as_secs_f64() / blocks as f64;
            println!(
                "{count:3} streams, {frames:4} frames, gain {gain:.2}: {:8.2} us/block, {:6.3}% core",
                seconds * 1e6,
                seconds * 48000.0 / frames as f64 * 100.0
            );
        }
    }
}
