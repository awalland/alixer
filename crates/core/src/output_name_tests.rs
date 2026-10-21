use crate::reconcile::{Checkpoint, reconcile};
use crate::*;

#[test]
fn output_rename_and_reset_survive_serialization_without_changing_routes() {
    let mut output = Strip::new(
        "USB speakers",
        StripKind::Output,
        vec!["FL".into(), "FR".into()],
    );
    output.output_key = Some("alsa_output.usb.speakers".into());
    let id = output.id;
    let source = Strip::new("Music", StripKind::Group, vec!["FL".into(), "FR".into()]);
    let route = Route {
        id: Id::new_v4(),
        source: source.id,
        destination: Destination::Output("alsa_output.usb.speakers".into()),
        source_channel: 0,
        target_channel: 0,
        gain: 1.0,
        tap: Tap::Post,
    };
    let mut scene = Scene {
        strips: vec![source, output],
        routes: vec![route],
        ..Default::default()
    };
    scene
        .edit(Edit::Rename {
            strip: id,
            name: "Desk".into(),
        })
        .unwrap();
    scene
        .edit(Edit::Rename {
            strip: id,
            name: "Studio".into(),
        })
        .unwrap();
    let mut restored: Scene =
        serde_json::from_str(&serde_json::to_string(&scene).unwrap()).unwrap();
    assert_eq!(
        restored.strip(id).unwrap().output_name_override.as_deref(),
        Some("Studio")
    );
    restored.edit(Edit::ResetOutputName { strip: id }).unwrap();
    let output = restored.strip(id).unwrap();
    assert_eq!(output.name, "USB speakers");
    assert_eq!(
        output.output_key.as_deref(),
        Some("alsa_output.usb.speakers")
    );
    assert!(output.output_name_override.is_none());
    assert_eq!(restored.routes, scene.routes);
}

#[test]
fn reconciliation_preserves_overrides_and_adopts_external_names_after_reset() {
    let output = Output {
        key: "speakers".into(),
        name: "Original".into(),
        channels: vec!["MONO".into()],
        volume: 1.0,
        mute: false,
        channel_volumes: vec![1.0],
        control_revision: 0,
    };
    let mut actual = Observation {
        outputs: vec![output],
        ..Default::default()
    };
    let first = reconcile(&Scene::default(), &Checkpoint::default(), &actual, &[]);
    let id = first.scene.strips[0].id;
    let mut scene = first.scene;
    scene
        .edit(Edit::Rename {
            strip: id,
            name: "Desk".into(),
        })
        .unwrap();
    let baseline = Checkpoint {
        observation: actual.clone(),
        bindings: Default::default(),
    };
    actual.outputs[0].name = "Desk".into();
    let mut scene = reconcile(&scene, &baseline, &actual, &[]).scene;
    assert_eq!(
        scene.strip(id).unwrap().output_original_name.as_deref(),
        Some("Original")
    );
    scene.edit(Edit::ResetOutputName { strip: id }).unwrap();
    let baseline = Checkpoint {
        observation: actual.clone(),
        bindings: Default::default(),
    };
    assert_eq!(
        reconcile(&scene, &baseline, &actual, &[])
            .scene
            .strip(id)
            .unwrap()
            .name,
        "Original"
    );
    actual.outputs[0].name = "External name".into();
    assert_eq!(
        reconcile(&scene, &baseline, &actual, &[])
            .scene
            .strip(id)
            .unwrap()
            .name,
        "External name"
    );
}
