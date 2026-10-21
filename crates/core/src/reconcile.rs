use crate::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Checkpoint {
    pub observation: Observation,
    /// Which logical strip each observed incarnation belonged to.
    pub bindings: BTreeMap<String, Id>,
}

#[derive(Clone, Debug)]
pub struct Reconciled {
    pub scene: Scene,
    pub conflicts: Vec<Conflict>,
    pub bindings: BTreeMap<String, Id>,
    pub external: BTreeMap<Id, (f32, bool)>,
}

pub fn runtime_key(id: &RuntimeId) -> String {
    format!("{}:{}", id.epoch, id.serial)
}

/// Absent nodes have no deletion semantics. Only comparable, surviving incarnations
/// can provide evidence that a route was removed while the controller was offline.
pub fn reconcile(
    saved: &Scene,
    baseline: &Checkpoint,
    actual: &Observation,
    existing: &[Conflict],
) -> Reconciled {
    let mut result = Reconciled {
        scene: saved.clone(),
        conflicts: existing.to_vec(),
        bindings: BTreeMap::new(),
        external: BTreeMap::new(),
    };
    for output in &actual.outputs {
        if output.channels.is_empty() {
            continue;
        }
        if let Some(strip) = result
            .scene
            .strips
            .iter_mut()
            .find(|s| s.output_key.as_ref() == Some(&output.key))
        {
            if strip.output_original_name.is_none() && strip.output_name_override.is_none() {
                strip.output_original_name = Some(if output.name.is_empty() {
                    output.key.clone()
                } else {
                    output.name.clone()
                });
            }
            let max = output
                .channel_volumes
                .iter()
                .copied()
                .fold(0.0_f32, f32::max);
            if max > 0.0 && output.channel_volumes.len() == strip.channels.len() {
                strip.output_balance = output.channel_volumes.iter().map(|v| v / max).collect();
            }
            let previous = baseline
                .observation
                .outputs
                .iter()
                .find(|o| o.key == output.key);
            if strip.output_name_override.is_none()
                && previous.is_some_and(|o| o.name != output.name)
                && !output.name.is_empty()
            {
                strip.name = output.name.clone();
                strip.output_original_name = Some(output.name.clone());
            }
            if previous.is_none_or(|o| {
                o.volume != output.volume || o.control_revision != output.control_revision
            }) {
                strip.gain = output.volume.clamp(0.0, 4.0);
            }
            if previous.is_none_or(|o| {
                o.mute != output.mute || o.control_revision != output.control_revision
            }) {
                strip.mute = output.mute;
            }
            // Retain the saved layout and inserts if the endpoint changes profile.
            // Port-name checks in the backend prevent routing a saved channel to another channel.
        } else {
            let mut strip = Strip::new(
                if output.name.is_empty() {
                    &output.key
                } else {
                    &output.name
                },
                StripKind::Output,
                output.channels.clone(),
            );
            strip.output_key = Some(output.key.clone());
            strip.output_original_name = Some(strip.name.clone());
            let max = output
                .channel_volumes
                .iter()
                .copied()
                .fold(0.0_f32, f32::max);
            strip.output_balance = if max > 0.0 {
                output.channel_volumes.iter().map(|v| v / max).collect()
            } else {
                vec![1.0; strip.channels.len()]
            };
            strip.gain = output.volume.clamp(0.0, 4.0);
            strip.mute = output.mute;
            result.scene.strips.push(strip);
        }
    }
    let mut claimed = BTreeSet::new();
    let reserved: BTreeMap<Id, String> = actual
        .streams
        .iter()
        .filter_map(|s| {
            let key = runtime_key(&s.runtime);
            baseline.bindings.get(&key).map(|id| (*id, key))
        })
        .collect();
    for stream in &actual.streams {
        let key = runtime_key(&stream.runtime);
        let exact = baseline
            .bindings
            .get(&key)
            .copied()
            .filter(|id| saved.strip(*id).is_some());
        let candidates: Vec<_> = saved
            .strips
            .iter()
            .filter(|s| {
                s.identity.as_ref() == Some(&stream.identity)
                    && !claimed.contains(&s.id)
                    && reserved.get(&s.id).is_none_or(|k| k == &key)
            })
            .map(|s| s.id)
            .collect();
        let count = actual
            .streams
            .iter()
            .filter(|s| s.identity == stream.identity)
            .count();
        let id = if let Some(id) = exact {
            id
        } else if candidates.len() == 1 && count == 1 {
            candidates[0]
        } else if candidates.is_empty() {
            let mut strip = Strip::new(
                &stream.identity.name,
                StripKind::Stream,
                stream.identity.channels.clone(),
            );
            strip.identity = Some(stream.identity.clone());
            let id = strip.id;
            result.scene.strips.push(strip);
            replace_outputs(&mut result.scene, id, &stream.targets);
            id
        } else {
            // Give every live stream a complete, independent strip without stealing
            // an indistinguishable saved strip's settings.
            let mut strip = Strip::new(
                &stream.identity.name,
                StripKind::Stream,
                stream.identity.channels.clone(),
            );
            strip.identity = Some(stream.identity.clone());
            let id = strip.id;
            result.scene.strips.push(strip);
            replace_outputs(&mut result.scene, id, &stream.targets);
            conflict(
                &mut result.conflicts,
                Some(id),
                "Returning streams are indistinguishable; saved settings were not assigned",
                &stream.targets,
            );
            id
        };
        if !claimed.insert(id) {
            conflict(
                &mut result.conflicts,
                Some(id),
                "Multiple streams matched the same strip",
                &stream.targets,
            );
            continue;
        }
        result.bindings.insert(key.clone(), id);
        result.external.insert(id, (stream.volume, stream.mute));
        let previous = baseline
            .observation
            .streams
            .iter()
            .find(|s| runtime_key(&s.runtime) == key);
        // A managed ingress, including shutdown parking, is not evidence of a user reroute.
        if stream.managed == Some(id)
            || stream
                .targets
                .iter()
                .any(|(name, _, _)| name.starts_with("alixer."))
        {
            continue;
        }
        let has_saved = saved.strip(id).is_some();
        if let Some(previous) = previous {
            if previous.targets != stream.targets && !stream.targets.is_empty() {
                // A change with all targets available and no old target loss is unambiguous
                // in topology. If a previous hardware target vanished, keep both choices.
                let old_output_lost = previous
                    .targets
                    .iter()
                    .filter(|(n, _, _)| !n.starts_with("alixer."))
                    .any(|(n, _, _)| !actual.outputs.iter().any(|o| &o.key == n));
                if old_output_lost {
                    conflict(
                        &mut result.conflicts,
                        Some(id),
                        "Output loss or outside routing edit: current routing is preserved",
                        &stream.targets,
                    );
                } else {
                    replace_outputs(&mut result.scene, id, &stream.targets);
                    result.conflicts.retain(|c| c.strip != Some(id));
                }
            } else if previous.targets != stream.targets && stream.targets.is_empty() {
                conflict(
                    &mut result.conflicts,
                    Some(id),
                    "Stream is disconnected; preserve current silence until resolved",
                    &stream.targets,
                );
            }
        } else if has_saved && !stream.targets.is_empty() {
            conflict(
                &mut result.conflicts,
                Some(id),
                "Stream was recreated; outside routing intent cannot be inferred",
                &stream.targets,
            );
        }
    }
    if result.scene != *saved {
        result.scene.revision = saved.revision + 1;
    }
    result
}

fn conflict(
    conflicts: &mut Vec<Conflict>,
    strip: Option<Id>,
    message: &str,
    current: &[(String, usize, usize)],
) {
    if let Some(existing) = conflicts
        .iter_mut()
        .find(|c| c.strip == strip && c.message == message)
    {
        existing.current = current.to_vec();
    } else {
        conflicts.push(Conflict {
            id: Id::new_v4(),
            strip,
            message: message.into(),
            current: current.to_vec(),
        });
    }
}

pub fn replace_outputs(scene: &mut Scene, strip: Id, targets: &[(String, usize, usize)]) {
    // Outside moving a source replaces its whole outgoing mix; keeping aux paths would
    // silently resurrect audio the outside controller did not request.
    scene.routes.retain(|r| r.source != strip);
    for (name, source_channel, target_channel) in targets {
        scene.routes.push(Route {
            id: Id::new_v4(),
            source: strip,
            destination: Destination::Output(name.clone()),
            source_channel: *source_channel,
            target_channel: *target_channel,
            gain: 1.0,
            tap: Tap::Post,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_controls_are_adopted_without_erasing_master_settings() {
        let mut actual = Observation::default();
        actual.outputs.push(Output {
            key: "speakers".into(),
            name: "Speakers".into(),
            channels: vec!["FL".into(), "FR".into()],
            volume: 0.4,
            mute: true,
            channel_volumes: vec![0.4, 0.2],
            control_revision: 0,
        });
        let first = reconcile(&Scene::default(), &Checkpoint::default(), &actual, &[]);
        let mut saved = first.scene;
        let id = saved.strips[0].id;
        saved
            .edit(Edit::Rename {
                strip: id,
                name: "Desk".into(),
            })
            .unwrap();
        let baseline = Checkpoint {
            observation: actual.clone(),
            bindings: Default::default(),
        };
        saved
            .edit(Edit::Gain {
                strip: id,
                value: 0.7,
            })
            .unwrap();
        assert_eq!(
            reconcile(&saved, &baseline, &actual, &[])
                .scene
                .strip(id)
                .unwrap()
                .gain,
            0.7,
            "unchanged observations must not undo an in-flight edit"
        );
        let mut changed_back = actual.clone();
        changed_back.outputs[0].control_revision += 2;
        assert_eq!(
            reconcile(&saved, &baseline, &changed_back, &[])
                .scene
                .strip(id)
                .unwrap()
                .gain,
            0.4,
            "native edits that return to the same value must still be adopted"
        );
        let absent = reconcile(&saved, &baseline, &Observation::default(), &[]);
        assert_eq!(absent.scene, saved);
        actual.outputs[0].volume = 0.2;
        actual.outputs[0].mute = false;
        let returned = reconcile(&saved, &Checkpoint::default(), &actual, &[]);
        let master = returned.scene.strip(id).unwrap();
        assert_eq!(master.name, "Desk");
        assert_eq!(master.gain, 0.2);
        assert!(!master.mute);
        assert_eq!(returned.scene.strips.len(), 1);
    }
    fn stream() -> StreamObservation {
        StreamObservation {
            runtime: RuntimeId {
                epoch: "server".into(),
                serial: "42".into(),
            },
            identity: Identity {
                application: "browser".into(),
                name: "audio".into(),
                role: "music".into(),
                channels: vec!["MONO".into()],
            },
            volume: 0.5,
            mute: false,
            targets: vec![("speakers".into(), 0, 0)],
            managed: None,
        }
    }
    #[test]
    fn outside_move_and_volume_are_adopted_idempotently() {
        let mut actual = Observation {
            streams: vec![stream()],
            outputs: vec![],
        };
        let first = reconcile(&Scene::default(), &Checkpoint::default(), &actual, &[]);
        let baseline = Checkpoint {
            observation: actual.clone(),
            bindings: first.bindings.clone(),
        };
        actual.outputs.push(Output {
            key: "speakers".into(),
            name: "speakers".into(),
            channels: vec!["MONO".into()],
            volume: 1.0,
            mute: false,
            channel_volumes: vec![1.0],
            control_revision: 0,
        });
        actual.streams[0].targets = vec![("headphones".into(), 0, 0)];
        actual.streams[0].volume = 0.8;
        let changed = reconcile(&first.scene, &baseline, &actual, &[]);
        assert!(changed.conflicts.is_empty());
        assert_eq!(changed.external.values().next().unwrap().0, 0.8);
        assert_eq!(
            changed.scene.routes[0].destination,
            Destination::Output("headphones".into())
        );
        let next = reconcile(
            &changed.scene,
            &Checkpoint {
                observation: actual.clone(),
                bindings: changed.bindings.clone(),
            },
            &actual,
            &[],
        );
        assert_eq!(next.scene, changed.scene);
    }
    #[test]
    fn missing_stream_does_not_delete_intent() {
        let first = reconcile(
            &Scene::default(),
            &Checkpoint::default(),
            &Observation {
                streams: vec![stream()],
                outputs: vec![],
            },
            &[],
        );
        let next = reconcile(
            &first.scene,
            &Checkpoint::default(),
            &Observation::default(),
            &[],
        );
        assert_eq!(first.scene, next.scene);
    }
    #[test]
    fn recreated_stream_has_a_conflict_instead_of_duplicate_audio() {
        let actual = Observation {
            streams: vec![stream()],
            outputs: vec![],
        };
        let first = reconcile(&Scene::default(), &Checkpoint::default(), &actual, &[]);
        let next = reconcile(&first.scene, &Checkpoint::default(), &actual, &[]);
        assert_eq!(next.conflicts.len(), 1);
    }
}
