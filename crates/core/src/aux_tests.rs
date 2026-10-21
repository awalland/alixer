use crate::*;

fn scene() -> (Scene, Id, Id) {
    let a = Strip::new("A", StripKind::Stream, vec!["FL".into(), "FR".into()]);
    let b = Strip::new("B", StripKind::Stream, a.channels.clone());
    let ids = (a.id, b.id);
    (
        Scene {
            strips: vec![a, b],
            ..Default::default()
        },
        ids.0,
        ids.1,
    )
}

#[test]
fn aux_creation_reuse_and_send_deletion_keep_the_master() {
    let (mut scene, a, b) = scene();
    scene
        .edit(Edit::AddAuxSend {
            source: a,
            aux: None,
        })
        .unwrap();
    let master = scene
        .strips
        .iter()
        .find(|s| s.kind == StripKind::Aux)
        .unwrap()
        .clone();
    assert_eq!(master.name, "Aux1");
    assert_eq!(master.channels, vec!["FL", "FR"]);
    assert_eq!(scene.routes.len(), 2);
    assert!(
        scene
            .routes
            .iter()
            .all(|r| r.gain == 0.0 && r.tap == Tap::Post)
    );
    assert!(
        !scene.routes.iter().any(|r| r.source == master.id),
        "new masters must have no outgoing routes"
    );
    scene
        .edit(Edit::SetAuxSend {
            source: a,
            aux: master.id,
            gain: Some(0.4),
            tap: Some(Tap::Pre),
        })
        .unwrap();
    scene
        .edit(Edit::AddAuxSend {
            source: b,
            aux: Some(master.id),
        })
        .unwrap();
    assert_eq!(scene.strips.len(), 3);
    let before = scene.clone();
    assert!(
        scene
            .edit(Edit::AddAuxSend {
                source: b,
                aux: Some(master.id)
            })
            .is_err()
    );
    assert_eq!(scene, before);
    let restored: Scene = serde_json::from_str(&serde_json::to_string(&scene).unwrap()).unwrap();
    assert_eq!(restored, scene);
    scene
        .edit(Edit::RemoveAuxSend {
            source: a,
            aux: master.id,
        })
        .unwrap();
    assert!(scene.routes.iter().all(|r| r.source == b));
    scene
        .edit(Edit::RemoveAuxSend {
            source: b,
            aux: master.id,
        })
        .unwrap();
    assert!(scene.routes.is_empty());
    assert!(
        scene.strip(master.id).is_some(),
        "the final send must not own the master's lifetime"
    );
    scene
        .edit(Edit::AddAuxSend {
            source: a,
            aux: Some(master.id),
        })
        .unwrap();
    scene.edit(Edit::RemoveStrip { strip: master.id }).unwrap();
    assert!(scene.strip(master.id).is_none());
    assert!(scene.routes.is_empty());
}

#[test]
fn aux_cycles_and_invalid_edits_are_rejected_atomically() {
    let (mut scene, a, _) = scene();
    scene
        .edit(Edit::AddAuxSend {
            source: a,
            aux: None,
        })
        .unwrap();
    scene
        .edit(Edit::AddAuxSend {
            source: a,
            aux: None,
        })
        .unwrap();
    let masters: Vec<_> = scene
        .strips
        .iter()
        .filter(|s| s.kind == StripKind::Aux)
        .cloned()
        .collect();
    assert_eq!(
        masters.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Aux1", "Aux2"]
    );
    scene
        .edit(Edit::AddAuxSend {
            source: masters[0].id,
            aux: Some(masters[1].id),
        })
        .unwrap();
    for edit in [
        Edit::AddAuxSend {
            source: masters[1].id,
            aux: Some(masters[0].id),
        },
        Edit::AddAuxSend {
            source: masters[0].id,
            aux: Some(masters[0].id),
        },
        Edit::SetAuxSend {
            source: a,
            aux: masters[0].id,
            gain: Some(f32::NAN),
            tap: Some(Tap::Pre),
        },
    ] {
        let before = scene.clone();
        assert!(scene.edit(edit).is_err());
        assert_eq!(scene, before);
    }
    let mut output = Strip::new("Output", StripKind::Output, vec!["MONO".into()]);
    output.output_key = Some("out".into());
    let id = output.id;
    scene.strips.push(output);
    let before = scene.clone();
    assert!(
        scene
            .edit(Edit::AddAuxSend {
                source: id,
                aux: None
            })
            .is_err()
    );
    assert_eq!(scene, before);
}

#[test]
fn aux_mappings_cover_mono_reversed_stereo_and_multichannel_streams() {
    for channels in [
        vec!["MONO".into()],
        vec!["FR".into(), "FL".into()],
        (0..8).map(|i| format!("AUX{i}")).collect(),
    ] {
        let source = Strip::new("Source", StripKind::Stream, channels.clone());
        let id = source.id;
        let mut scene = Scene {
            strips: vec![source],
            ..Default::default()
        };
        scene
            .edit(Edit::AddAuxSend {
                source: id,
                aux: None,
            })
            .unwrap();
        assert_eq!(scene.routes.len(), channels.len().max(2));
        if channels.len() == 1 {
            assert!(scene.routes.iter().all(|r| r.source_channel == 0));
        }
        if channels == vec!["FR", "FL"] {
            assert_eq!(
                (
                    scene.routes[0].source_channel,
                    scene.routes[0].target_channel
                ),
                (1, 0)
            );
        }
        if channels.len() == 8 {
            assert_eq!(scene.strips[1].channels, channels);
        }
    }
}

#[test]
fn aux_level_and_tap_changes_process_live_and_gain_changes_ramp() {
    let (mut scene, a, _) = scene();
    scene
        .edit(Edit::Gain {
            strip: a,
            value: 0.5,
        })
        .unwrap();
    scene
        .edit(Edit::AddAuxSend {
            source: a,
            aux: None,
        })
        .unwrap();
    let aux = scene.strips.last().unwrap().id;
    scene
        .edit(Edit::SetAuxSend {
            source: a,
            aux,
            gain: Some(1.0),
            tap: None,
        })
        .unwrap();
    let sends: Vec<_> = scene.routes.iter().map(|r| r.id).collect();
    scene.routes.push(Route {
        id: Id::new_v4(),
        source: aux,
        destination: Destination::Output("out".into()),
        source_channel: 0,
        target_channel: 0,
        gain: 1.0,
        tap: Tap::Post,
    });
    let mut dsp = dsp::Processor::new(scene, 512).unwrap();
    let mut output = [0.0; 512];
    dsp.input(a, 0).unwrap().fill(1.0);
    dsp.process(512, 48000.0);
    dsp.output("out", 0, &mut output);
    assert!((output[511] - 0.5).abs() < 0.001);
    for id in &sends {
        dsp.set_route_controls(*id, 1.0, Tap::Pre);
    }
    dsp.clear();
    dsp.input(a, 0).unwrap().fill(1.0);
    dsp.process(512, 48000.0);
    dsp.output("out", 0, &mut output);
    assert!((output[511] - 1.0).abs() < 0.001);
    for id in &sends {
        dsp.set_route_controls(*id, 0.25, Tap::Pre);
    }
    dsp.clear();
    dsp.input(a, 0).unwrap().fill(1.0);
    dsp.process(512, 48000.0);
    dsp.output("out", 0, &mut output);
    assert!(output[0] > 0.25 && output[0] < 1.0);
    assert!((output[511] - 0.25).abs() < 0.001);
    dsp.set_controls(a, 0.5, true, false);
    dsp.clear();
    dsp.input(a, 0).unwrap().fill(1.0);
    dsp.process(512, 48000.0);
    dsp.output("out", 0, &mut output);
    assert_eq!(output[511], 0.0);
}
