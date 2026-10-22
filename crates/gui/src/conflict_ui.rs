use super::*;

const AMBER: Color32 = Color32::from_rgb(224, 164, 62);

pub(super) fn has_strip_conflict(snapshot: &Snapshot, strip: Id) -> bool {
    snapshot.conflicts.iter().any(|c| c.strip == Some(strip))
}

fn output_name(snapshot: &Snapshot, key: &str) -> String {
    snapshot
        .scene
        .strips
        .iter()
        .find(|s| s.output_key.as_deref() == Some(key))
        .map(|s| s.name.clone())
        .or_else(|| {
            snapshot
                .observation
                .outputs
                .iter()
                .find(|o| o.key == key)
                .map(|o| o.name.clone())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| key.to_owned())
}

fn destination_name(snapshot: &Snapshot, destination: &Destination) -> String {
    match destination {
        Destination::Output(key) => output_name(snapshot, key),
        Destination::Bus(id) => snapshot
            .scene
            .strip(*id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Unavailable bus".into()),
    }
}

fn channel(channels: &[String], index: usize) -> String {
    channels
        .get(index)
        .cloned()
        .unwrap_or_else(|| format!("Channel {}", index + 1))
}

fn destination_summary(names: impl Iterator<Item = String>) -> String {
    let mut unique = Vec::new();
    for name in names {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    unique.join(" + ")
}

impl App {
    /// Orphaned notices remain reachable in the console header; all other
    /// notices belong to their strip, even when it is collapsed or offline.
    pub(super) fn conflict_badge(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        strip: Option<Id>,
    ) {
        let conflicts: Vec<_> = snapshot
            .conflicts
            .iter()
            .filter(|c| match strip {
                Some(id) => c.strip == Some(id),
                None => c.strip.is_none_or(|id| snapshot.scene.strip(id).is_none()),
            })
            .collect();
        if conflicts.is_empty() {
            return;
        }
        ui.push_id(("strip-attention", strip), |ui| {
            ui.menu_button(
                RichText::new(if strip.is_some() {
                    "!"
                } else {
                    "! Needs attention"
                })
                .strong()
                .color(AMBER),
                |ui| {
                    ui.set_width(320.0);
                    egui::ScrollArea::vertical()
                        .id_salt(("conflict-details", strip))
                        .max_height(420.0)
                        .show(ui, |ui| {
                            for (index, conflict) in conflicts.iter().enumerate() {
                                if index > 0 {
                                    ui.separator();
                                }
                                self.conflict_details(ui, snapshot, conflict);
                            }
                        });
                },
            )
            .response
            .on_hover_text("Needs attention: review this channel's routing");
        });
    }

    fn conflict_details(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, conflict: &Conflict) {
        let strip = conflict.strip.and_then(|id| snapshot.scene.strip(id));
        ui.label(RichText::new(strip.map_or("Unavailable channel", |s| s.name.as_str())).strong());
        ui.label(&conflict.message);
        ui.label(
            RichText::new("Detected connections stay in place until you choose.")
                .small()
                .color(DIM),
        );
        ui.add_space(4.0);
        ui.label(RichText::new("Detected routing").strong());
        if conflict.current.is_empty() {
            ui.label("Disconnected");
        }
        for (key, source, target) in &conflict.current {
            let channels = strip.map_or(&[][..], |s| s.channels.as_slice());
            let target_channels = snapshot
                .scene
                .strips
                .iter()
                .find(|s| s.output_key.as_ref() == Some(key))
                .map(|s| s.channels.as_slice())
                .or_else(|| {
                    snapshot
                        .observation
                        .outputs
                        .iter()
                        .find(|o| &o.key == key)
                        .map(|o| o.channels.as_slice())
                })
                .unwrap_or_default();
            ui.label(format!(
                "{} → {} / {}",
                channel(channels, *source),
                output_name(snapshot, key),
                channel(target_channels, *target)
            ));
        }
        let ambiguous = conflict.message.contains("indistinguishable")
            || conflict.message.contains("Multiple streams");
        let routes: Vec<_> = snapshot
            .scene
            .routes
            .iter()
            .filter(|r| Some(r.source) == conflict.strip)
            .collect();
        if let Some(strip) = strip {
            ui.add_space(4.0);
            ui.label(
                RichText::new(if ambiguous {
                    "Channel routing"
                } else {
                    "Saved routing"
                })
                .strong(),
            );
            if routes.is_empty() {
                ui.label("Disconnected");
            }
            for route in &routes {
                let target_channels = match &route.destination {
                    Destination::Bus(id) => {
                        snapshot.scene.strip(*id).map(|s| s.channels.as_slice())
                    }
                    Destination::Output(key) => snapshot
                        .scene
                        .strips
                        .iter()
                        .find(|s| s.output_key.as_ref() == Some(key))
                        .map(|s| s.channels.as_slice()),
                }
                .unwrap_or_default();
                ui.label(format!(
                    "{} → {} / {}",
                    channel(&strip.channels, route.source_channel),
                    destination_name(snapshot, &route.destination),
                    channel(target_channels, route.target_channel)
                ));
            }
        }
        if routes.iter().any(|route| {
            matches!(&route.destination, Destination::Output(key)
            if key != MICROPHONE_KEY && !snapshot.observation.outputs.iter().any(|o| &o.key == key))
        }) {
            ui.label(
                RichText::new(
                    "A saved output is offline. Audio to it resumes when the device returns.",
                )
                .small()
                .color(AMBER),
            );
        }
        ui.add_space(6.0);
        let keep = if strip.is_none() {
            "Dismiss notice".into()
        } else if conflict.current.is_empty() {
            "Keep disconnected".into()
        } else {
            format!(
                "Keep → {}",
                destination_summary(
                    conflict
                        .current
                        .iter()
                        .map(|(key, _, _)| output_name(snapshot, key))
                )
            )
        };
        if ui.button(keep).clicked() {
            self.send(Command::Resolve {
                conflict: conflict.id,
                restore: strip.is_none(),
            });
            ui.close();
        }
        if strip.is_some() {
            let restore = if routes.is_empty() {
                "Use saved silence".into()
            } else {
                format!(
                    "{} → {}",
                    if ambiguous {
                        "Use channel routing"
                    } else {
                        "Restore"
                    },
                    destination_summary(
                        routes
                            .iter()
                            .map(|r| destination_name(snapshot, &r.destination))
                    )
                )
            };
            if ui.button(restore).clicked() {
                self.send(Command::Resolve {
                    conflict: conflict.id,
                    restore: true,
                });
                ui.close();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        app: App,
        commands: mpsc::Receiver<Command>,
        snapshot: Snapshot,
        ctx: egui::Context,
        labels: Vec<(String, egui::Rect)>,
        time: f64,
        orphan: bool,
    }

    impl Harness {
        fn new(collapsed: bool) -> Self {
            let (mut app, commands) = test_app();
            let source = Strip::new(
                "Music player",
                StripKind::Stream,
                vec!["FL".into(), "FR".into()],
            );
            if collapsed {
                app.preferences.set_hidden(source.id, true);
            }
            let mut output = Strip::new(
                "Desk speakers",
                StripKind::Output,
                vec!["FL".into(), "FR".into()],
            );
            output.output_key = Some("test.speakers".into());
            let snapshot = Snapshot {
                active_streams: Some(vec![]), // Offline channels with notices must stay visible.
                scene: Scene {
                    strips: vec![source.clone(), output],
                    routes: vec![Route {
                        id: Id::new_v4(),
                        source: source.id,
                        destination: Destination::Output("test.speakers".into()),
                        source_channel: 0,
                        target_channel: 0,
                        gain: 1.0,
                        tap: Tap::Post,
                    }],
                    ..Default::default()
                },
                observation: Observation::default(),
                conflicts: vec![Conflict {
                    id: Id::new_v4(),
                    strip: Some(source.id),
                    message: "Stream is disconnected; preserve current silence until resolved"
                        .into(),
                    current: vec![],
                }],
                status: String::new(),
                peaks: vec![],
                channel_peaks: vec![],
                plugin_meters: vec![],
            };
            let mut h = Self {
                app,
                commands,
                snapshot,
                ctx: egui::Context::default(),
                labels: vec![],
                time: 0.0,
                orphan: false,
            };
            h.frame(vec![]);
            h.frame(vec![]);
            h
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.05;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    time: Some(self.time),
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1000.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    if self.orphan {
                        self.app.conflict_badge(ui, &self.snapshot, None);
                    } else {
                        self.app.strip_bank(ui, &self.snapshot, STRIP_MIN_HEIGHT);
                    }
                },
            );
            self.labels = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(t) => Some((
                        t.galley.text().to_owned(),
                        egui::Rect::from_min_size(t.pos, t.galley.size()),
                    )),
                    _ => None,
                })
                .collect();
            output.textures_delta.clear();
        }

        fn has(&self, label: &str) -> bool {
            self.labels.iter().any(|(text, _)| text == label)
        }

        fn click(&mut self, label: &str) {
            let pos = self
                .labels
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("Missing {label}: {:?}", self.labels))
                .1
                .center();
            for pressed in [true, false] {
                self.frame(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ]);
            }
            self.frame(vec![]);
            self.frame(vec![]);
        }
    }

    #[test]
    fn offline_channel_notice_opens_routing_choices_and_keeps_silence() {
        let mut h = Harness::new(false);
        assert!(
            h.has("!"),
            "offline channels with conflicts must not be filtered out"
        );
        h.click("!");
        assert!(h.has("Detected routing") && h.has("Saved routing"));
        assert!(h.has("FL → Desk speakers / FL"));
        assert!(
            h.commands.try_recv().is_err(),
            "opening a notice must not change routing"
        );
        h.click("Keep disconnected");
        assert!(
            matches!(h.commands.try_recv().unwrap(), Command::Resolve { conflict, restore: false }
            if conflict == h.snapshot.conflicts[0].id)
        );
    }

    #[test]
    fn collapsed_channel_notice_can_restore_saved_routing() {
        let mut h = Harness::new(true);
        h.click("!");
        h.click("Restore → Desk speakers");
        assert!(
            matches!(h.commands.try_recv().unwrap(), Command::Resolve { conflict, restore: true }
            if conflict == h.snapshot.conflicts[0].id)
        );
        assert!(
            h.app.preferences.is_hidden(h.snapshot.scene.strips[0].id),
            "resolving a notice should not expand the collapsed channel"
        );
    }

    #[test]
    fn external_destinations_are_named_and_ambiguity_does_not_offer_saved_settings() {
        let mut h = Harness::new(false);
        h.snapshot.conflicts[0].current = vec![("test.speakers".into(), 1, 1)];
        h.snapshot.conflicts[0].message =
            "Returning streams are indistinguishable; saved settings were not assigned".into();
        h.frame(vec![]);
        h.click("!");
        assert!(h.has("FR → Desk speakers / FR"));
        assert!(h.has("Use channel routing → Desk speakers"));
        assert!(!h.has("Saved routing"));
        h.click("Keep → Desk speakers");
        assert!(matches!(
            h.commands.try_recv().unwrap(),
            Command::Resolve { restore: false, .. }
        ));
    }

    #[test]
    fn deleted_channel_notice_can_be_dismissed_without_recreating_routes() {
        let mut h = Harness::new(false);
        h.snapshot.scene.strips.clear();
        h.snapshot.scene.routes.clear();
        h.orphan = true;
        h.frame(vec![]);
        h.click("! Needs attention");
        assert!(h.has("Unavailable channel"));
        h.click("Dismiss notice");
        assert!(
            matches!(
                h.commands.try_recv().unwrap(),
                Command::Resolve { restore: true, .. }
            ),
            "orphan dismissal must not import routes referencing a deleted channel"
        );
    }

    #[test]
    fn resolved_notices_disappear_and_offline_channels_hide_normally() {
        let mut h = Harness::new(false);
        h.snapshot.conflicts.clear();
        h.frame(vec![]);
        h.frame(vec![]);
        assert!(!h.has("!") && !h.has("Music player"));
        h.app.show_inactive = true;
        h.frame(vec![]);
        assert!(h.has("Music player") && !h.has("!"));
    }
}
