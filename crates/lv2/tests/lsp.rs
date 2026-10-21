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
    for name in ["LSP Compressor Stereo", "LSP Filter Stereo"] {
        let info = catalog.iter().find(|p| p.name == name).expect(name);
        assert!(info.compatible(2), "{}: {:?}", info.name, info.unsupported);
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
        let mut instance = alixer_lv2::Instance::new(&slot, 2, 48000.0, 512).unwrap();
        let mut audio = vec![vec![0.0; 512]; 2];
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
        assert!(peak > 0.001, "{name} produced no audio");
        assert_eq!(
            instance.parameters.len(),
            info.ports.iter().filter(|p| p.control && p.input).count()
        );
        eprintln!("PASS: {name}, output peak {peak}");
    }
}
