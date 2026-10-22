//! Opt-in check against a real LSP LV2 installation, isolated from desktop audio.
use alixer_core::{Id, Insert};
use std::collections::BTreeMap;

#[test]
fn lsp_audio_effects_work_with_optional_messaging_ports_disconnected() {
    let Some(path) = std::env::var_os("ALIXER_LSP_LV2_PATH") else {
        eprintln!("Set ALIXER_LSP_LV2_PATH and ALIXER_LILV_LIBRARY to test real LSP plugins");
        return;
    };
    // This integration test runs alone in its own process.
    unsafe {
        std::env::set_var("LV2_PATH", path);
    }
    let catalog = alixer_lv2::catalog().unwrap();
    let supported = catalog.iter().filter(|p| p.compatible(2)).count();
    assert!(
        supported > 10,
        "Expected stereo LSP effects, got {supported}"
    );
    eprintln!(
        "{supported} compatible stereo effects out of {} LSP plugins",
        catalog.len()
    );
    for (channels, effect) in [1, 2].into_iter().flat_map(|channels| {
        alixer_core::effects::Effect::ALL
            .into_iter()
            .map(move |effect| (channels, effect))
    }) {
        let uri = effect.uri(channels).unwrap();
        let info = catalog.iter().find(|p| p.uri == uri).expect(&uri);
        let name = &info.name;
        assert!(
            info.compatible(channels),
            "{}: {:?}",
            info.name,
            info.unsupported
        );
        let input = info.ports.iter().find(|p| p.symbol == "g_in").unwrap();
        assert_eq!(input.unit, "G");
        assert!(input.logarithmic);
        let mode = info
            .ports
            .iter()
            .find(|p| {
                p.symbol
                    == if effect == alixer_core::effects::Effect::Compressor {
                        "cm"
                    } else {
                        "mode"
                    }
            })
            .unwrap();
        assert!(mode.choices.len() > 1);
        if effect == alixer_core::effects::Effect::Equalizer {
            assert!(
                info.ports
                    .iter()
                    .any(|p| p.symbol == "f_7" && p.logarithmic)
            );
        }
        assert!(
            info.ports
                .iter()
                .any(|p| p.optional && !p.audio && !p.control)
        );
        let slot = Insert {
            id: Id::new_v4(),
            uri: info.uri.clone(),
            bypass: false,
            controls: BTreeMap::new(),
            state: None,
        };
        let mut instance = alixer_lv2::Instance::new(&slot, channels, 48000.0, 512).unwrap();
        let mut audio = vec![vec![0.0; 512]; channels];
        let mut peak = 0.0_f32;
        for block in 0..32 {
            for channel in &mut audio {
                for (i, sample) in channel.iter_mut().enumerate() {
                    *sample = (((block * 512 + i) as f32) * 440.0 * std::f32::consts::TAU
                        / 48000.0)
                        .sin()
                        * 0.1;
                }
            }
            instance.run(&mut audio, 512);
            for sample in audio.iter().flatten() {
                assert!(sample.is_finite(), "{name} produced a non-finite sample");
                peak = peak.max(sample.abs());
            }
        }
        assert!(!instance.meters.is_empty());
        assert!(instance.meters.values().all(|value| {
            f32::from_bits(value.load(std::sync::atomic::Ordering::Relaxed)).is_finite()
        }));
        let in_symbol = if effect == alixer_core::effects::Effect::Equalizer {
            if channels == 1 { "im" } else { "iml" }
        } else if channels == 1 {
            "ilm"
        } else {
            "ilm_l"
        };
        assert!(
            f32::from_bits(instance.meters[in_symbol].load(std::sync::atomic::Ordering::Relaxed))
                > 0.01,
            "{name} input meter must be live"
        );
        assert!(peak > 0.001, "{name} produced no audio");
        assert_eq!(
            instance.parameters.len(),
            info.ports.iter().filter(|p| p.control && p.input).count()
        );
        eprintln!("PASS: {name}, output peak {peak}");
    }
}
