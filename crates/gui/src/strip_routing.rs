use super::*;

const HEIGHT: f32 = 90.0;

struct Pair {
    destination: Destination,
    name: String,
    channels: Vec<(usize, String)>,
    offline: bool,
}

fn pairs(strip: &Strip, snapshot: &Snapshot, show_offline: bool) -> Vec<Pair> {
    if strip.kind.terminal() {
        return vec![];
    }
    let mut result = vec![];
    for target in output_masters(snapshot, show_offline).chain(
        snapshot
            .scene
            .strips
            .iter()
            .filter(|s| s.kind == StripKind::Group),
    ) {
        let destination = match target.kind {
            StripKind::Output | StripKind::VirtualMic => target
                .output_key
                .as_ref()
                .map(|key| Destination::Output(key.clone())),
            StripKind::Group | StripKind::Aux if target.id != strip.id => {
                Some(Destination::Bus(target.id))
            }
            _ => None,
        };
        let Some(destination) = destination else {
            continue;
        };
        let offline = target.kind != StripKind::VirtualMic && target
            .output_key
            .as_ref()
            .is_some_and(|key| !snapshot.observation.outputs.iter().any(|o| &o.key == key));
        let mut channels: Vec<_> = target
            .channels
            .iter()
            .enumerate()
            .map(|(i, name)| (i, name.clone()))
            .collect();
        if channels.len() == 2
            && channels.iter().any(|(_, c)| c == "FL")
            && channels.iter().any(|(_, c)| c == "FR")
        {
            channels.sort_by_key(|(_, name)| if name == "FL" { 0 } else { 1 });
        }
        for first in (0..channels.len()).step_by(2) {
            result.push(Pair {
                destination: destination.clone(),
                name: target.name.clone(),
                offline,
                channels: channels.iter().skip(first).take(2).cloned().collect(),
            });
        }
    }
    result
}

fn source_channel(strip: &Strip, target_channel: usize, side: usize) -> Option<usize> {
    match strip.channels.len() {
        1 => Some(0),
        2 => Some(
            strip
                .channels
                .iter()
                .position(|c| c == if side == 0 { "FL" } else { "FR" })
                .unwrap_or(side),
        ),
        count if target_channel < count => Some(target_channel),
        _ => None,
    }
}

fn allowed(scene: &Scene, source: Id, destination: &Destination) -> bool {
    let Destination::Bus(target) = destination else {
        return true;
    };
    let mut pending = vec![*target];
    let mut visited = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        if id == source {
            return false;
        }
        if visited.insert(id) {
            pending.extend(
                scene
                    .routes
                    .iter()
                    .filter(|r| r.source == id)
                    .filter_map(|r| scene.route_target(&r.destination)),
            );
        }
    }
    true
}

impl App {
    pub(super) fn routing_rack(
        &mut self,
        ui: &mut egui::Ui,
        strip: &Strip,
        snapshot: &Snapshot,
    ) -> egui::Response {
        let frame = egui::Frame::new()
            .fill(BG)
            .inner_margin(4.0)
            .stroke(egui::Stroke::new(1.0, Color32::from_gray(50)))
            .corner_radius(3.0);
        let margins = frame.total_margin().sum();
        let width = ui.available_width();
        frame
            .show(ui, |ui| {
                ui.set_width(width - margins.x);
                ui.set_height(HEIGHT - margins.y);
                let pairs = pairs(strip, snapshot, self.show_inactive);
                if strip.kind == StripKind::Output {
                    ui.interact(
                        ui.max_rect(),
                        ui.id().with(("terminal-routing", strip.id)),
                        egui::Sense::hover(),
                    )
                    .on_hover_text(
                        "Terminal output. Edit incoming connections in the sending channel's routing box.",
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt(("strip-routes", strip.id))
                    .max_height(HEIGHT - margins.y)
                    .min_scrolled_height(HEIGHT - margins.y)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let rows = pairs.len().div_ceil(2);
                        let cell = ui.spacing().interact_size.y.max(18.0) + 6.0;
                        let row_pitch = cell + 4.0;
                        let size = Vec2::new(
                            ui.available_width(),
                            if rows == 0 {
                                0.0
                            } else {
                                rows as f32 * row_pitch - 4.0
                            },
                        );
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        let gap = 8.0;
                        let pair_width = (rect.width() - gap) / 2.0;
                        if rows > 0 {
                            ui.painter().vline(
                                rect.center().x,
                                rect.y_range(),
                                egui::Stroke::new(1.0, Color32::from_gray(55)),
                            );
                        }
                        for (index, pair) in pairs.iter().enumerate() {
                            let column = index / rows;
                            let row = index % rows;
                            let left = rect.left() + column as f32 * (pair_width + gap);
                            let top = rect.top() + row as f32 * row_pitch;
                            let cells = std::array::from_fn(|side| {
                                egui::Rect::from_center_size(
                                    egui::pos2(
                                        left + (side as f32 + 0.5) * pair_width / 2.0,
                                        top + cell / 2.0,
                                    ),
                                    Vec2::splat(cell),
                                )
                            });
                            self.route_pair(ui, strip, snapshot, pair, cells);
                        }
                    });
            })
            .response
    }

    fn route_pair(
        &mut self,
        ui: &mut egui::Ui,
        strip: &Strip,
        snapshot: &Snapshot,
        pair: &Pair,
        cells: [egui::Rect; 2],
    ) {
        let can_connect = allowed(&snapshot.scene, strip.id, &pair.destination);
        for side in 0..2 {
            let mapping = pair
                .channels
                .get(side)
                .and_then(|(tc, name)| source_channel(strip, *tc, side).map(|sc| (sc, *tc, name)));
            let existing: Vec<_> = mapping
                .map(|(sc, tc, _)| {
                    snapshot
                        .scene
                        .routes
                        .iter()
                        .filter(|r| {
                            r.source == strip.id
                                && r.destination == pair.destination
                                && r.source_channel == sc
                                && r.target_channel == tc
                        })
                        .map(|r| r.id)
                        .collect()
                })
                .unwrap_or_default();
            let mut checked = !existing.is_empty();
            let tooltip = format!(
                "{}\n{}{}{}",
                pair.name,
                mapping
                    .map(|(_, _, name)| name.as_str())
                    .unwrap_or("No corresponding channel"),
                if pair.offline { " · offline" } else { "" },
                if !can_connect {
                    " · would create a routing loop"
                } else {
                    ""
                }
            );
            let enabled = mapping.is_some() && (can_connect || checked);
            let key = match &pair.destination {
                Destination::Output(key) => format!("output:{key}"),
                Destination::Bus(id) => format!("bus:{id}"),
            };
            let mut builder = egui::UiBuilder::new()
                .max_rect(cells[side])
                .layout(egui::Layout::centered_and_justified(
                    egui::Direction::TopDown,
                ))
                .id_salt(("route-cell", strip.id, key, pair.channels[0].0, side));
            if !enabled {
                builder = builder.disabled();
            }
            let mut child = ui.new_child(builder);
            let response = child.add(egui::Checkbox::without_text(&mut checked));
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    ui.is_enabled() && enabled,
                    checked,
                    &tooltip,
                )
            });
            if response.changed() {
                if checked {
                    if let Some((sc, tc, _)) = mapping {
                        self.edit(Edit::PutRoute {
                            route: Route {
                                id: Id::new_v4(),
                                source: strip.id,
                                destination: pair.destination.clone(),
                                source_channel: sc,
                                target_channel: tc,
                                gain: 1.0,
                                tap: Tap::Post,
                            },
                        });
                    }
                } else {
                    for route in existing {
                        self.edit(Edit::RemoveRoute { route });
                    }
                }
            }
            response
                .on_hover_text(&tooltip)
                .on_disabled_hover_text(&tooltip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Harness {
        app: App,
        commands: mpsc::Receiver<Command>,
        ctx: egui::Context,
        strip: Strip,
        snapshot: Snapshot,
        time: f64,
        checkboxes: Vec<egui::Rect>,
        texts: Vec<String>,
        rack: egui::Rect,
    }
    impl Harness {
        fn new(count: usize) -> Self {
            let (app, commands) = test_app();
            let strip = Strip::new("Player", StripKind::Stream, vec!["FL".into(), "FR".into()]);
            let mut scene = Scene::default();
            scene.strips.push(strip.clone());
            for n in 0..count {
                let mut target = Strip::new(
                    format!("Output {n}"),
                    StripKind::Output,
                    vec!["FL".into(), "FR".into()],
                );
                target.output_key = Some(format!("out{n}"));
                scene.strips.push(target);
            }
            let outputs = scene
                .strips
                .iter()
                .filter_map(|s| {
                    s.output_key.as_ref().map(|key| Output {
                        key: key.clone(),
                        name: s.name.clone(),
                        channels: s.channels.clone(),
                        volume: 1.0,
                        mute: false,
                        channel_volumes: vec![1.0; s.channels.len()],
                        control_revision: 0,
                    })
                })
                .collect();
            let snapshot = Snapshot {
                active_streams: None,
                scene,
                observation: Observation {
                    outputs,
                    ..Default::default()
                },
                conflicts: vec![],
                status: String::new(),
                peaks: vec![],
                channel_peaks: vec![],
            };
            let mut this = Self {
                app,
                commands,
                ctx: egui::Context::default(),
                strip,
                snapshot,
                time: 0.0,
                checkboxes: vec![],
                texts: vec![],
                rack: egui::Rect::NOTHING,
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
                        Vec2::new(500.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(154.0);
                    self.rack = self.app.routing_rack(ui, &self.strip, &self.snapshot).rect;
                },
            );
            self.checkboxes = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Rect(r)
                        if r.rect.width() >= 8.0
                            && r.rect.width() <= 24.0
                            && (r.rect.width() - r.rect.height()).abs() < 0.1 =>
                    {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .collect();
            let mut centers: Vec<egui::Pos2> = vec![];
            self.checkboxes.retain(|rect| {
                let center = rect.center();
                if centers.iter().any(|p| p.distance(center) < 5.0) {
                    false
                } else {
                    centers.push(center);
                    true
                }
            });
            self.checkboxes.sort_by(|a, b| {
                a.center()
                    .y
                    .total_cmp(&b.center().y)
                    .then(a.center().x.total_cmp(&b.center().x))
            });
            self.texts = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            output.textures_delta.clear();
        }
        fn click(&mut self, pos: egui::Pos2) {
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
        }
    }
    #[test]
    fn two_destination_pairs_share_a_row_and_clicks_edit_the_matching_channels() {
        let mut h = Harness::new(2);
        assert_eq!(h.checkboxes.len(), 4);
        let boxes = h.checkboxes.clone();
        assert!((boxes[0].center().y - boxes[3].center().y).abs() < 0.1);
        assert!(
            !h.texts.iter().any(|s| s.contains("Output")),
            "names must not be displayed beside the checkboxes"
        );
        h.click(boxes[0].center());
        let Command::Edit(Edit::PutRoute { route }) = h.commands.try_recv().unwrap() else {
            panic!("Expected route insertion")
        };
        assert_eq!(route.destination, Destination::Output("out0".into()));
        assert_eq!((route.source_channel, route.target_channel), (0, 0));
        h.snapshot
            .scene
            .edit(Edit::PutRoute {
                route: route.clone(),
            })
            .unwrap();
        h.frame(vec![]);
        h.click(boxes[0].center());
        assert!(
            matches!(h.commands.try_recv().unwrap(),Command::Edit(Edit::RemoveRoute{route:id})if id==route.id)
        );
        h.click(boxes[3].center());
        assert!(
            matches!(h.commands.try_recv().unwrap(),Command::Edit(Edit::PutRoute{route})if route.destination==Destination::Output("out1".into())&&route.source_channel==1&&route.target_channel==1)
        );
    }
    #[test]
    fn routing_box_scrolls_and_names_only_appear_on_hover() {
        let mut h = Harness::new(40);
        assert!((h.rack.height() - HEIGHT).abs() < 0.1);
        let first = h.checkboxes[0].center();
        h.frame(vec![egui::Event::PointerMoved(first)]);
        for _ in 0..30 {
            h.frame(vec![]);
        }
        assert!(
            h.texts.iter().any(|s| s.contains("Output 0")),
            "hover must identify the destination"
        );
    }
    #[test]
    fn mono_multichannel_and_terminal_outputs_keep_their_routing_semantics() {
        let mut h = Harness::new(1);
        h.strip.channels = vec!["MONO".into()];
        assert_eq!(source_channel(&h.strip, 1, 1), Some(0));
        h.strip.channels = (0..8).map(|n| format!("AUX{n}")).collect();
        assert_eq!(source_channel(&h.strip, 7, 1), Some(7));
        let output = h.snapshot.scene.strips[1].clone();
        assert!(pairs(&output, &h.snapshot, false).is_empty());
        let mut a = Strip::new("A", StripKind::Group, vec!["FL".into(), "FR".into()]);
        a.output_key = None;
        let b = Strip::new("B", StripKind::Aux, a.channels.clone());
        h.snapshot.scene.strips.extend([a.clone(), b.clone()]);
        h.snapshot
            .scene
            .edit(Edit::PutRoute {
                route: Route {
                    id: Id::new_v4(),
                    source: a.id,
                    destination: Destination::Bus(b.id),
                    source_channel: 0,
                    target_channel: 0,
                    gain: 1.0,
                    tap: Tap::Post,
                },
            })
            .unwrap();
        assert!(!allowed(&h.snapshot.scene, b.id, &Destination::Bus(a.id)));
        assert!(allowed(&h.snapshot.scene, a.id, &Destination::Bus(b.id)));
    }

    #[test]
    fn five_outputs_fill_down_the_left_column_then_down_the_right() {
        let mut h = Harness::new(5);
        assert_eq!(h.checkboxes.len(), 10);
        let boxes = h.checkboxes.clone();
        for (index, output) in [(0, 0), (4, 1), (8, 2), (2, 3), (6, 4)] {
            h.click(boxes[index].center());
            assert!(
                matches!(h.commands.try_recv().unwrap(),Command::Edit(Edit::PutRoute{route})if route.destination==Destination::Output(format!("out{output}"))&&route.target_channel==0)
            );
        }
        for column in [[0, 4, 8], [2, 6, 6]] {
            assert!((boxes[column[0]].center().x - boxes[column[1]].center().x).abs() < 0.1);
            assert!((boxes[column[0]].center().x - boxes[column[2]].center().x).abs() < 0.1);
        }
        for _ in 0..30 {
            h.frame(vec![]);
            assert!(
                (h.rack.width() - 154.0).abs() < 0.1,
                "routing controls must not grow their parent"
            );
        }
    }

    #[test]
    fn destination_order_follows_visible_output_masters_after_reordering() {
        let mut h = Harness::new(5);
        let bus = Strip::new("Bus", StripKind::Group, vec!["FL".into(), "FR".into()]);
        h.snapshot.scene.strips.insert(0, bus);
        h.snapshot.observation.outputs.retain(|o| o.key != "out1");
        let mut order: Vec<_> = h.snapshot.scene.strips.iter().map(|s| s.id).collect();
        order.reverse();
        h.snapshot
            .scene
            .edit(Edit::Reorder { strips: order })
            .unwrap();
        for offline in [false, true] {
            let expected: Vec<_> = output_masters(&h.snapshot, offline)
                .map(|s| Destination::Output(s.output_key.clone().unwrap()))
                .collect();
            let actual = pairs(&h.strip, &h.snapshot, offline);
            assert_eq!(
                actual
                    .iter()
                    .take(expected.len())
                    .map(|p| p.destination.clone())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(
                matches!(actual.last().unwrap().destination, Destination::Bus(_)),
                "buses follow the output masters"
            );
            assert_eq!(actual.len(), if offline { 6 } else { 5 });
        }
    }

    #[test]
    fn left_and_right_follow_channel_names_when_port_order_is_reversed() {
        let mut h = Harness::new(1);
        h.strip.channels = vec!["FR".into(), "FL".into()];
        h.snapshot.scene.strips[1].channels = vec!["FR".into(), "FL".into()];
        h.frame(vec![]);
        let left = h.checkboxes[0].center();
        h.click(left);
        assert!(
            matches!(h.commands.try_recv().unwrap(),Command::Edit(Edit::PutRoute{route})if route.source_channel==1&&route.target_channel==1)
        );
    }
}
