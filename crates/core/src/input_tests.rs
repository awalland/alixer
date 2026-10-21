use crate::{reconcile::*, *};

#[test]
fn inputs_are_unrouted_persistent_and_reconnect_without_duplicates() {
    let actual = Observation {
        inputs: vec![Input {
            key: "headset.mic".into(),
            name: "Headset microphone".into(),
            channels: vec!["MONO".into()],
        }],
        ..Default::default()
    };
    let first = reconcile(&Scene::default(), &Checkpoint::default(), &actual, &[]);
    first.scene.validate().unwrap();
    assert!(first.scene.routes.is_empty());
    let mut scene = first.scene;
    let input = scene
        .strips
        .iter()
        .find(|s| s.kind == StripKind::Input)
        .unwrap()
        .id;
    let mic = scene
        .strips
        .iter()
        .find(|s| s.kind == StripKind::VirtualMic)
        .unwrap()
        .id;
    assert_eq!(scene.strip(input).unwrap().color, [224, 164, 62]);
    scene
        .edit(Edit::PutRoute {
            route: Route {
                id: Id::new_v4(),
                source: input,
                destination: Destination::Output(MICROPHONE_KEY.into()),
                source_channel: 0,
                target_channel: 0,
                gain: 1.0,
                tap: Tap::Post,
            },
        })
        .unwrap();
    scene
        .edit(Edit::Gain {
            strip: input,
            value: 0.5,
        })
        .unwrap();
    let absent = reconcile(&scene, &Checkpoint::default(), &Observation::default(), &[]);
    let returned = reconcile(&absent.scene, &Checkpoint::default(), &actual, &[]);
    assert_eq!(returned.scene, scene);
    assert!(returned.conflicts.is_empty());
    assert_eq!(
        scene.route_target(&Destination::Output(MICROPHONE_KEY.into())),
        Some(mic)
    );
    assert!(
        scene
            .edit(Edit::Solo {
                strip: mic,
                value: true
            })
            .is_err()
    );
    assert!(
        scene
            .edit(Edit::AddAuxSend {
                source: mic,
                aux: None
            })
            .is_err()
    );
}

#[test]
fn microphone_fader_mute_and_virtual_master_control_recorded_audio() {
    let actual = Observation {
        inputs: vec![Input {
            key: "mic".into(),
            name: "Mic".into(),
            channels: vec!["MONO".into()],
        }],
        ..Default::default()
    };
    let mut scene = reconcile(&Scene::default(), &Checkpoint::default(), &actual, &[]).scene;
    let input = scene
        .strips
        .iter()
        .find(|s| s.kind == StripKind::Input)
        .unwrap()
        .id;
    let master = scene
        .strips
        .iter()
        .find(|s| s.kind == StripKind::VirtualMic)
        .unwrap()
        .id;
    for ch in 0..2 {
        scene
            .edit(Edit::PutRoute {
                route: Route {
                    id: Id::new_v4(),
                    source: input,
                    destination: Destination::Output(MICROPHONE_KEY.into()),
                    source_channel: 0,
                    target_channel: ch,
                    gain: 1.0,
                    tap: Tap::Post,
                },
            })
            .unwrap();
    }
    let mut processor = dsp::Processor::new(scene, 1024).unwrap();
    processor.set_controls(input, 0.5, false, false);
    processor.set_controls(master, 0.5, false, false);
    let mut out = [0.0; 1024];
    for _ in 0..4 {
        processor.clear();
        processor.input(input, 0).unwrap().fill(1.0);
        processor.process(1024, 48000.0);
    }
    processor.output(MICROPHONE_KEY, 0, &mut out);
    assert!((out[1023] - 0.25).abs() < 0.001);
    processor.set_controls(master, 0.5, true, false);
    processor.process(1024, 48000.0);
    processor.output(MICROPHONE_KEY, 1, &mut out);
    assert!(out.iter().all(|v| *v == 0.0));
}
