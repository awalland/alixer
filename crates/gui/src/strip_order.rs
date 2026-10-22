use super::*;

#[derive(Clone, Copy)]
struct StripDrag(Id);

fn section(kind: &StripKind) -> u8 {
    match kind {
        StripKind::Stream => 0,
        StripKind::Input => 1,
        StripKind::Group | StripKind::Aux => 2,
        StripKind::SystemSounds => 3,
        StripKind::Output | StripKind::VirtualMic => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_sounds_sits_before_outputs_with_no_routing_or_insert_controls() {
        let h = Harness::new(vec![StripKind::Output, StripKind::SystemSounds, StripKind::Group,
            StripKind::Stream, StripKind::Input], false);
        assert!(h.labels["Channel 3"].x < h.labels["Channel 4"].x);
        assert!(h.labels["Channel 4"].x < h.labels["Channel 2"].x);
        assert!(h.labels["Channel 2"].x < h.labels["Channel 1"].x);
        assert!(h.labels["Channel 1"].x < h.labels["Channel 0"].x);
        assert_eq!(h.snapshot.scene.strips[1].color, [100, 149, 237]);
        assert!(h.commands.try_recv().is_err());
        let (mut app, commands) = test_app();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.strip(ui, &h.snapshot.scene.strips[1], &h.snapshot, STRIP_MIN_HEIGHT);
        });
        let text: Vec<_> = output.shapes.iter().filter_map(|s| match &s.shape {
            egui::epaint::Shape::Text(t) => Some(t.galley.text()), _ => None,
        }).collect();
        assert!(text.contains(&"Follows each sound’s destination"));
        assert!(!text.iter().any(|t| t.contains("Right-click to add") || t.contains("AUX SENDS")));
        assert!(commands.try_recv().is_err());
        output.textures_delta.clear();
    }

    #[test]
    fn moves_preserve_other_sections_and_include_offline_strips() {
        let kinds = [
            StripKind::Input,
            StripKind::Stream,
            StripKind::Output,
            StripKind::Stream,
            StripKind::Aux,
            StripKind::Group,
            StripKind::Stream,
        ];
        let scene = Scene {
            strips: kinds
                .into_iter()
                .enumerate()
                .map(|(n, kind)| Strip::new(format!("Channel {n}"), kind, vec!["MONO".into()]))
                .collect(),
            ..Default::default()
        };
        let ids: Vec<_> = scene.strips.iter().map(|s| s.id).collect();
        assert_eq!(
            reordered(&scene, ids[6], ids[1], false).unwrap(),
            vec![ids[0], ids[6], ids[2], ids[1], ids[4], ids[5], ids[3]]
        );
        assert_eq!(
            reordered(&scene, ids[1], ids[6], true).unwrap(),
            vec![ids[0], ids[3], ids[2], ids[6], ids[4], ids[5], ids[1]]
        );
        assert!(reordered(&scene, ids[0], ids[1], true).is_none());
        assert!(reordered(&scene, ids[1], ids[3], false).is_none());
        assert!(reordered(&scene, ids[1], ids[1], true).is_none());
        assert!(
            reordered(&scene, ids[4], ids[5], true).is_some(),
            "groups and auxes share a section"
        );
    }

    struct Harness {
        app: App,
        commands: mpsc::Receiver<Command>,
        ctx: egui::Context,
        snapshot: Snapshot,
        panels: Vec<egui::Rect>,
        labels: BTreeMap<String, egui::Pos2>,
        time: f64,
        marker: bool,
    }

    impl Harness {
        fn new(kinds: Vec<StripKind>, collapsed: bool) -> Self {
            let (mut app, commands) = test_app();
            app.show_inactive = true;
            let mut strips: Vec<_> = kinds
                .into_iter()
                .enumerate()
                .map(|(n, kind)| {
                    let mut strip =
                        Strip::new(format!("Channel {n}"), kind.clone(), vec!["MONO".into()]);
                    if kind == StripKind::Output {
                        strip.output_key = Some(format!("output{n}"));
                    }
                    strip
                })
                .collect();
            // Different applications must still obey the saved playback order.
            for (n, strip) in strips.iter_mut().enumerate() {
                if strip.kind == StripKind::Stream {
                    strip.identity = Some(Identity {
                        application: if n % 2 == 0 { "A" } else { "B" }.into(),
                        channels: strip.channels.clone(),
                        ..Default::default()
                    });
                }
            }
            if collapsed {
                app.preferences.set_hidden(strips[0].id, true);
            }
            let mut this = Self {
                app,
                commands,
                ctx: egui::Context::default(),
                snapshot: Snapshot {
                    active_streams: None,
                    scene: Scene {
                        strips,
                        ..Default::default()
                    },
                    observation: Observation::default(),
                    conflicts: vec![],
                    status: String::new(),
                    peaks: vec![],
                    channel_peaks: vec![],
                    plugin_meters: vec![],
                },
                panels: vec![],
                labels: BTreeMap::new(),
                time: 0.0,
                marker: false,
            };
            this.frame(vec![]);
            this
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.05;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    time: Some(self.time),
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(2400.0, 850.0),
                    )),
                    ..Default::default()
                },
                |ui| self.app.strip_bank(ui, &self.snapshot, STRIP_MIN_HEIGHT),
            );
            self.panels = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Rect(r) if r.fill == PANEL && r.rect.height() > 600.0 => {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .collect();
            self.panels.sort_by(|a, b| a.left().total_cmp(&b.left()));
            self.labels = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) if t.galley.text().starts_with("Channel ") => {
                        Some((t.galley.text().into(), t.pos + t.galley.size() * 0.5))
                    }
                    _ => None,
                })
                .collect();
            self.marker = output.shapes.iter().any(|s| matches!(&s.shape,
                egui::epaint::Shape::LineSegment {stroke, ..} if stroke.width == 3.0 && stroke.color == MINT));
            output.textures_delta.clear();
        }

        fn drag(&mut self, source: egui::Pos2, target: egui::Pos2) {
            self.frame(vec![
                egui::Event::PointerMoved(source),
                egui::Event::PointerButton {
                    pos: source,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ]);
            self.frame(vec![egui::Event::PointerMoved(
                source + egui::vec2(12.0, 0.0),
            )]);
            self.frame(vec![egui::Event::PointerMoved(target)]);
        }

        fn release(&mut self, pos: egui::Pos2) {
            self.frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }]);
        }
    }

    #[test]
    fn dragging_name_draws_marker_and_sends_one_reorder_on_release() {
        let mut h = Harness::new(vec![StripKind::Stream; 3], false);
        let ids: Vec<_> = h.snapshot.scene.strips.iter().map(|s| s.id).collect();
        let source = h.labels["Channel 0"];
        let target = h.panels[2].center() + egui::vec2(30.0, 0.0);
        h.drag(source, target);
        assert!(h.marker);
        assert!(
            h.commands.try_recv().is_err(),
            "dragging must not edit until release"
        );
        h.release(target);
        let Command::Edit(Edit::Reorder { strips }) = h.commands.try_recv().unwrap() else {
            panic!("expected reorder")
        };
        assert_eq!(strips, vec![ids[1], ids[2], ids[0]]);
        assert!(h.commands.try_recv().is_err());
        h.snapshot.scene.edit(Edit::Reorder { strips }).unwrap();
        h.frame(vec![]);
        assert!(
            h.labels["Channel 1"].x < h.labels["Channel 2"].x
                && h.labels["Channel 2"].x < h.labels["Channel 0"].x
        );
    }

    #[test]
    fn identical_playback_names_do_not_make_saved_strips_look_active() {
        let mut h = Harness::new(vec![StripKind::Stream; 3], false);
        let identity = Identity {
            application: "Unknown application".into(),
            name: "Playback".into(),
            channels: vec!["MONO".into()],
            ..Default::default()
        };
        for strip in &mut h.snapshot.scene.strips {
            strip.identity = Some(identity.clone());
        }
        let active = h.snapshot.scene.strips[1].id;
        h.snapshot.observation.streams.push(StreamObservation {
            runtime: RuntimeId {
                epoch: "server".into(),
                serial: "42".into(),
            },
            identity,
            volume: 1.0,
            mute: false,
            targets: vec![],
            managed: None,
        });
        h.snapshot.active_streams = Some(vec![active]);
        h.app.show_inactive = false;
        h.frame(vec![]);
        assert_eq!(h.panels.len(), 1);
        assert!(h.labels.contains_key("Channel 1"));
        h.app.show_inactive = true;
        h.frame(vec![]);
        assert_eq!(h.panels.len(), 3);
        h.snapshot.active_streams = None;
        h.snapshot.observation.streams[0].managed = Some(active);
        h.app.show_inactive = false;
        h.frame(vec![]);
        assert_eq!(
            h.panels.len(),
            1,
            "older service snapshots can use exact managed IDs too"
        );
    }

    #[test]
    fn collapsed_name_is_draggable_and_cross_section_drops_are_rejected() {
        let mut h = Harness::new(
            vec![StripKind::Stream, StripKind::Stream, StripKind::Output],
            true,
        );
        let source = h.panels[0].center();
        let target = h.panels[1].center() + egui::vec2(30.0, 0.0);
        h.drag(source, target);
        assert!(h.marker);
        h.release(target);
        assert!(matches!(
            h.commands.try_recv().unwrap(),
            Command::Edit(Edit::Reorder { .. })
        ));
        let mut h = Harness::new(vec![StripKind::Stream, StripKind::Output], false);
        let source = h.labels["Channel 0"];
        let target = h.panels[1].center();
        h.drag(source, target);
        assert!(!h.marker);
        h.release(target);
        assert!(h.commands.try_recv().is_err());
    }
}

fn reordered(scene: &Scene, source: Id, target: Id, after: bool) -> Option<Vec<Id>> {
    let source_strip = scene.strip(source)?;
    if source == target || section(&source_strip.kind) != section(&scene.strip(target)?.kind) {
        return None;
    }
    // Preserve every other section's slots, including offline and collapsed strips.
    let mut bank: Vec<_> = scene
        .strips
        .iter()
        .filter(|s| section(&s.kind) == section(&source_strip.kind))
        .map(|s| s.id)
        .filter(|id| *id != source)
        .collect();
    let index = bank.iter().position(|id| *id == target)? + usize::from(after);
    bank.insert(index, source);
    let mut bank = bank.into_iter();
    let order: Vec<_> = scene
        .strips
        .iter()
        .map(|s| {
            if section(&s.kind) == section(&source_strip.kind) {
                bank.next().unwrap()
            } else {
                s.id
            }
        })
        .collect();
    (order != scene.strips.iter().map(|s| s.id).collect::<Vec<_>>()).then_some(order)
}

impl App {
    pub(super) fn strip_drag_handle(&self, response: &egui::Response, strip: &Strip) {
        response.dnd_set_drag_payload(StripDrag(strip.id));
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_text("Drag to reorder within this section. Right-click for options.");
        if response.dragged() {
            response.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }

    pub(super) fn strip_bank(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, height: f32) {
        let mut rects = vec![];
        ui.horizontal_top(|ui| {
            let mut previous = None;
            for bank in 0..5 {
                for strip in snapshot
                    .scene
                    .strips
                    .iter()
                    .filter(|s| section(&s.kind) == bank)
                {
                    let online = match strip.kind {
                        StripKind::Input => snapshot
                            .observation
                            .inputs
                            .iter()
                            .any(|i| Some(&i.key) == strip.input_key.as_ref()),
                        StripKind::Stream => snapshot.stream_active(strip),
                        StripKind::Output => snapshot
                            .observation
                            .outputs
                            .iter()
                            .any(|o| Some(&o.key) == strip.output_key.as_ref()),
                        _ => true,
                    };
                    if !online && !self.show_inactive && !conflict_ui::has_strip_conflict(snapshot, strip.id) {
                        continue;
                    }
                    if previous.is_some_and(|previous| previous != bank) {
                        ui.separator();
                    }
                    previous = Some(bank);
                    let rect = ui
                        .push_id(strip.id, |ui| self.strip(ui, strip, snapshot, height))
                        .inner;
                    rects.push((strip, rect));
                }
            }
        });
        for (strip, rect) in rects {
            self.strip_drop_target(ui, snapshot, strip, rect);
        }
        if egui::DragAndDrop::has_payload_of_type::<StripDrag>(ui.ctx()) {
            let viewport = ui.clip_rect();
            if let Some(pointer) = ui
                .ctx()
                .pointer_interact_pos()
                .filter(|p| viewport.contains(*p))
            {
                let direction = if pointer.x < viewport.left() + 28.0 {
                    1.0
                } else if pointer.x > viewport.right() - 28.0 {
                    -1.0
                } else {
                    0.0
                };
                if direction != 0.0 {
                    let dt = ui.input(|i| i.stable_dt).min(0.05);
                    ui.scroll_with_delta_animation(
                        egui::vec2(direction * 500.0 * dt, 0.0),
                        egui::style::ScrollAnimation::none(),
                    );
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    fn strip_drop_target(
        &mut self,
        ui: &egui::Ui,
        snapshot: &Snapshot,
        strip: &Strip,
        rect: egui::Rect,
    ) {
        let Some(payload) = egui::DragAndDrop::payload::<StripDrag>(ui.ctx()) else {
            return;
        };
        let Some(pointer) = ui
            .ctx()
            .pointer_interact_pos()
            .filter(|p| rect.intersect(ui.clip_rect()).contains(*p))
        else {
            return;
        };
        let Some(source) = snapshot.scene.strip(payload.0) else {
            return;
        };
        if section(&source.kind) != section(&strip.kind) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
            return;
        }
        if source.id == strip.id {
            return;
        }
        let after = pointer.x > rect.center().x;
        let x = if after {
            rect.right() + 2.0
        } else {
            rect.left() - 2.0
        };
        ui.painter().line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(3.0, MINT),
        );
        if ui.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::take_payload::<StripDrag>(ui.ctx());
            if let Some(strips) = reordered(&snapshot.scene, source.id, strip.id, after) {
                self.edit(Edit::Reorder { strips });
            }
        }
    }
}
