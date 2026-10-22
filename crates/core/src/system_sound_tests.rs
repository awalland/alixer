use crate::{reconcile::*, *};

fn event(serial: &str, role: &str, output: &str) -> StreamObservation {
    StreamObservation {
        runtime: RuntimeId {
            epoch: "test".into(),
            serial: serial.into(),
        },
        identity: Identity {
            application: "test".into(),
            name: "audio-volume-change".into(),
            role: role.into(),
            channels: vec!["FL".into(), "FR".into()],
        },
        volume: 1.0,
        mute: false,
        managed: None,
        targets: vec![(output.into(), 0, 0), (output.into(), 1, 1)],
    }
}

#[test]
fn simultaneous_events_share_controls_without_merging_destinations() {
    let actual = Observation {
        streams: vec![
            event("1", "event", "left"),
            event("2", "Notification", "right"),
            event("3", "Music", "right"),
        ],
        ..Default::default()
    };
    let first = reconcile(&Scene::default(), &Checkpoint::default(), &actual, &[]);
    let system = first
        .scene
        .strips
        .iter()
        .find(|s| s.kind == StripKind::SystemSounds)
        .unwrap();
    assert_eq!(system.color, [100, 149, 237]);
    assert_eq!(
        first.bindings[&runtime_key(&actual.streams[0].runtime)],
        system.id
    );
    assert_eq!(
        first.bindings[&runtime_key(&actual.streams[1].runtime)],
        system.id
    );
    assert_ne!(
        first.bindings[&runtime_key(&actual.streams[2].runtime)],
        system.id
    );
    assert!(
        !first.external.contains_key(&system.id),
        "per-event volume must not overwrite shared controls"
    );
    assert!(!first.scene.routes.iter().any(|r| r.source == system.id));
    assert_eq!(
        first
            .scene
            .strips
            .iter()
            .filter(|s| s.kind == StripKind::Stream)
            .count(),
        1
    );
    assert!(first.conflicts.is_empty());
    first.scene.validate().unwrap();
    let mut saved = first.scene.clone();
    saved
        .edit(Edit::Gain {
            strip: system.id,
            value: 0.3,
        })
        .unwrap();
    saved
        .edit(Edit::Mute {
            strip: system.id,
            value: true,
        })
        .unwrap();
    let absent = reconcile(&saved, &Checkpoint::default(), &Observation::default(), &[]);
    assert_eq!(
        absent.scene, saved,
        "settings persist without any live events"
    );
    let mut actual = actual.clone();
    actual.streams[0].volume = 0.1;
    actual.streams[1].targets = vec![("headphones".into(), 0, 0)];
    let returned = reconcile(
        &saved,
        &Checkpoint {
            observation: Observation::default(),
            bindings: first.bindings,
        },
        &actual,
        &[],
    );
    assert_eq!(returned.scene.strip(system.id).unwrap().gain, 0.3);
    assert!(returned.scene.strip(system.id).unwrap().mute);
    assert!(!returned.scene.routes.iter().any(|r| r.source == system.id));
    assert!(returned.conflicts.is_empty());
}

#[test]
fn historical_event_strips_migrate_without_removing_ordinary_playback() {
    let mut saved = Scene::default();
    let mut old = Strip::new(
        "Old event",
        StripKind::Stream,
        vec!["FL".into(), "FR".into()],
    );
    old.identity = Some(event("1", "event", "left").identity);
    let music = Strip::new("Music", StripKind::Stream, old.channels.clone());
    saved.strips = vec![old.clone(), music.clone()];
    replace_outputs(&mut saved, old.id, &[("left".into(), 0, 0)]);
    let conflict = Conflict {
        id: Id::new_v4(),
        strip: Some(old.id),
        message: "Old conflict".into(),
        current: vec![],
    };
    let next = reconcile(
        &saved,
        &Checkpoint::default(),
        &Observation::default(),
        &[conflict],
    );
    assert!(next.scene.strip(old.id).is_none());
    assert_eq!(next.scene.strip(music.id), Some(&music));
    assert!(next.scene.routes.is_empty() && next.conflicts.is_empty());
    assert_eq!(
        next.scene
            .strips
            .iter()
            .filter(|s| s.kind == StripKind::SystemSounds)
            .count(),
        1
    );
    assert_eq!(
        reconcile(
            &next.scene,
            &Checkpoint::default(),
            &Observation::default(),
            &[]
        )
        .scene,
        next.scene
    );
}

#[test]
fn system_controls_reject_routing_sends_inserts_and_removal_atomically() {
    let mut scene = reconcile(
        &Scene::default(),
        &Checkpoint::default(),
        &Observation::default(),
        &[],
    )
    .scene;
    let id = scene.strips[0].id;
    let before = scene.clone();
    for edit in [
        Edit::RemoveStrip { strip: id },
        Edit::Solo {
            strip: id,
            value: true,
        },
        Edit::AddAuxSend {
            source: id,
            aux: None,
        },
        Edit::AddInsert {
            strip: id,
            uri: "test:plugin".into(),
        },
        Edit::PutRoute {
            route: Route {
                id: Id::new_v4(),
                source: id,
                destination: Destination::Output("left".into()),
                source_channel: 0,
                target_channel: 0,
                gain: 1.0,
                tap: Tap::Post,
            },
        },
    ] {
        assert!(scene.edit(edit).is_err());
        assert_eq!(scene, before);
    }
    scene
        .edit(Edit::Color {
            strip: id,
            color: [1, 2, 3],
        })
        .unwrap();
    scene.edit(Edit::ResetChannelColors).unwrap();
    assert_eq!(scene.strip(id).unwrap().color, [100, 149, 237]);
    let mut duplicate = scene.clone();
    duplicate.strips.push(Strip::new(
        "Duplicate",
        StripKind::SystemSounds,
        vec!["MONO".into()],
    ));
    assert!(duplicate.validate().is_err());
}
