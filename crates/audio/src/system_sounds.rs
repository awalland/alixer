use super::*;

pub(super) fn is_event(props: &BTreeMap<String, String>) -> bool {
    props.get("media.class").map(String::as_str) == Some("Stream/Output/Audio")
        && props.get("stream.monitor").map(String::as_str) != Some("true")
        && (props
            .get("media.role")
            .is_some_and(|role| system_sound_role(role))
            || props.get("event.id").is_some_and(|id| !id.is_empty()))
}

// A separate filter for each live event preserves independent destinations and
// applies shared controls in DSP before the first buffer can reach the device.
struct Event {
    _ingress: Option<pw::node::Node>,
    ingress_name: String,
    render: render::Render,
    scene: Scene,
    signature: String,
    rate: u32,
    retire_after: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Sounds {
    events: BTreeMap<(u32, String), Event>,
}

impl Sounds {
    pub(super) fn sync(
        &mut self,
        core: &pw::core::CoreRc,
        graph: &Graph,
        scene: &Scene,
        metering: &Arc<Metering>,
        metadata: Option<&pw::metadata::Metadata>,
        links: &mut BTreeMap<(u32, u32), pw::link::Link>,
        linked: &mut BTreeSet<(u32, u32)>,
    ) -> Result<()> {
        let group = scene
            .strips
            .iter()
            .find(|s| s.kind == StripKind::SystemSounds);
        let mut wanted: BTreeSet<_> = graph
            .objects
            .iter()
            .filter_map(|(&id, raw)| {
                (group.is_some() && raw.kind == "Node" && is_event(&raw.props)).then(|| {
                    (
                        id,
                        raw.props.get("object.serial").cloned().unwrap_or_default(),
                    )
                })
            })
            .collect();
        let now = Instant::now();
        self.events.retain(|key, event| {
            if wanted.contains(key) {
                event.retire_after = None;
                return true;
            }
            let deadline = *event
                .retire_after
                .get_or_insert(now + Duration::from_millis(500));
            if now < deadline {
                wanted.insert(key.clone());
                return true;
            }
            disconnect_filter(graph, links, &event.render.name);
            false
        });
        let Some(group) = group else { return Ok(()) };
        for key in wanted {
            let (id, _) = &key;
            if let Some(event) = self.events.get(&key).filter(|e| e.retire_after.is_some()) {
                // Keep only the existing tail links. PipeWire can reuse the
                // departed client's node ID for an unrelated recorder or player.
                let ports: BTreeSet<_> = graph
                    .objects
                    .iter()
                    .filter(|(_, raw)| {
                        raw.kind == "Node"
                            && (raw.props.get("node.name") == Some(&event.render.name)
                                || raw.props.get("node.name") == Some(&event.ingress_name))
                    })
                    .flat_map(|(&id, _)| {
                        graph
                            .ports(id, true)
                            .into_iter()
                            .chain(graph.ports(id, false))
                    })
                    .map(|p| p.0)
                    .collect();
                linked.extend(
                    links
                        .keys()
                        .filter(|(source, target)| ports.contains(source) || ports.contains(target))
                        .copied(),
                );
                continue;
            }
            let Some(target_name) = graph.system_targets.get(id).cloned() else {
                continue;
            };
            let Some((&target_id, _)) = graph.objects.iter().find(|(_, raw)| {
                raw.kind == "Node" && raw.props.get("node.name") == Some(&target_name)
            }) else {
                continue;
            };
            let target_ports = graph.ports(target_id, false);
            if target_ports.is_empty() {
                continue;
            }
            let pulse = graph
                .objects
                .get(id)
                .is_some_and(|n| n.props.contains_key("pulse.corked"));
            let target_channels: Vec<_> = target_ports.iter().map(|p| p.2.clone()).collect();
            let channels: Vec<_> = if pulse {
                target_channels.clone()
            } else {
                graph.ports(*id, true).iter().map(|p| p.2.clone()).collect()
            };
            if channels.is_empty() {
                continue;
            }
            let signature = serde_json::to_string(&(&channels, &target_name, &target_channels))?;
            let rate = self.events.get(&key).map_or(48000, |e| e.render.rate());
            if self
                .events
                .get(&key)
                .is_none_or(|e| e.signature != signature || e.rate != rate)
            {
                if let Some(old) = self.events.remove(&key) {
                    disconnect_filter(graph, links, &old.render.name);
                }
                let mut strip = Strip::new("System sound", StripKind::Stream, channels.clone());
                strip.id = group.id;
                strip.gain = group.gain;
                strip.mute = group.mute;
                let mut scene = Scene {
                    strips: vec![strip],
                    ..Default::default()
                };
                for (target_channel, (_, _, target)) in target_ports.iter().enumerate() {
                    for (source_channel, source) in channels.iter().enumerate() {
                        let mono_to_front =
                            channels.len() == 1 && matches!(target.as_str(), "MONO" | "FL" | "FR");
                        let front_to_mono =
                            target_ports.len() == 1 && matches!(source.as_str(), "FL" | "FR");
                        if source == target || mono_to_front || front_to_mono {
                            scene.routes.push(Route {
                                id: Id::new_v4(),
                                source: group.id,
                                destination: Destination::Output(target_name.clone()),
                                source_channel,
                                target_channel,
                                gain: if front_to_mono { 0.5 } else { 1.0 },
                                tap: Tap::Post,
                            });
                        }
                    }
                }
                let ingress_name = format!("alixer.event.{}", key.1);
                // PulseAudio needs a sink peer for its timing/drain protocol.
                let ingress = if pulse {
                    Some(core.create_object::<pw::node::Node>(
                        "adapter",
                        &properties! {
                            "factory.name" => "support.null-audio-sink",
                            "node.name" => ingress_name.clone(),
                            "node.description" => "Alixer system sound",
                            "media.class" => "Audio/Sink",
                            "audio.position" => format!("[ {} ]", channels.join(" ")),
                            "node.virtual" => "true",
                            "node.driver" => "false",
                            "priority.session" => "-1000",
                            "monitor.channel-volumes" => "false",
                            "monitor.passthrough" => "true",
                        },
                    )?)
                } else {
                    None
                };
                let render = render::Render::new_event(core, &scene, rate, metering.clone())?;
                self.events.insert(
                    key.clone(),
                    Event {
                        _ingress: ingress,
                        ingress_name,
                        render,
                        scene,
                        signature,
                        rate,
                        retire_after: None,
                    },
                );
            }
            let event = self.events.get_mut(&key).expect("event filter created");
            event.scene.strips[0].gain = group.gain;
            event.scene.strips[0].mute = group.mute;
            event.render.update_controls(&event.scene);
            let Some((&filter, _)) = graph.objects.iter().find(|(_, raw)| {
                raw.kind == "Node" && raw.props.get("node.name") == Some(&event.render.name)
            }) else {
                continue;
            };
            let source_id = if pulse {
                let Some((&ingress_id, _)) = graph.objects.iter().find(|(_, raw)| {
                    raw.kind == "Node" && raw.props.get("node.name") == Some(&event.ingress_name)
                }) else {
                    continue;
                };
                ingress_id
            } else {
                *id
            };
            let source_ports = graph.ports(source_id, true);
            let inputs = graph.ports(filter, false);
            for (ch, (source, _, _)) in source_ports.iter().enumerate() {
                if let Some(name) = event.render.input_name(group.id, ch) {
                    if let Some((target, _, _)) = inputs.iter().find(|p| p.1 == name) {
                        wire_ports(core, linked, links, *source, *target);
                    }
                }
            }
            if pulse
                && event.retire_after.is_none()
                && graph.has_link(source_id, filter)
                && graph.has_link(filter, target_id)
                && graph.system_ready.get(id) != Some(&event.ingress_name)
            {
                if let Some(metadata) = metadata {
                    metadata.set_property(
                        *id,
                        "alixer.system-sounds.ready",
                        Some("Spa:String"),
                        Some(&event.ingress_name),
                    );
                }
            }
            for (output, ch, port) in &event.render.outputs {
                if let Some((_, target, _)) = target_ports.get(*ch) {
                    wire(
                        core,
                        graph,
                        linked,
                        links,
                        &event.render.name,
                        port,
                        output,
                        target,
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn rate_changed(&self) -> bool {
        self.events
            .values()
            .any(|e| e.rate != e.render.rate() || e.retire_after.is_some())
    }

    /// Events play independently; display the loudest active event.
    pub(super) fn peak(&self) -> f32 {
        self.events
            .values()
            .map(|e| e.render.take_peak())
            .fold(0.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_only_explicitly_tagged_playback_events() {
        let mut props = BTreeMap::from([
            ("media.class".into(), "Stream/Output/Audio".into()),
            ("media.name".into(), "audio-volume-change".into()),
        ]);
        assert!(!is_event(&props), "names alone must not classify a stream");
        for role in ["event", "Notification", "notification", "EVENT"] {
            props.insert("media.role".into(), role.into());
            assert!(is_event(&props));
        }
        props.insert("media.role".into(), "Music".into());
        assert!(!is_event(&props));
        props.insert("event.id".into(), "custom-alert".into());
        assert!(is_event(&props));
        props.insert("media.class".into(), "Stream/Input/Audio".into());
        assert!(
            !is_event(&props),
            "event-tagged capture streams are not playback"
        );
    }
}
