use super::*;

const HEIGHT: f32 = 64.0;
struct SendView {
    aux: Id,
    name: String,
    gain: f32,
    tap: Option<Tap>,
}
fn sends(strip: &Strip, scene: &Scene) -> Vec<SendView> {
    scene
        .strips
        .iter()
        .filter(|s| s.kind == StripKind::Aux)
        .filter_map(|aux| {
            let routes: Vec<_> = scene
                .routes
                .iter()
                .filter(|r| r.source == strip.id && r.destination == Destination::Bus(aux.id))
                .collect();
            let first = routes.first()?;
            Some(SendView {
                aux: aux.id,
                name: aux.name.clone(),
                gain: routes.iter().map(|r| r.gain).fold(0.0_f32, f32::max),
                tap: if routes.iter().all(|r| r.tap == first.tap) {
                    Some(first.tap)
                } else {
                    None
                },
            })
        })
        .collect()
}

impl App {
    fn add_send_menu(&mut self, ui: &mut egui::Ui, strip: &Strip, scene: &Scene) {
        if ui.button("Create new aux").clicked() {
            self.edit(Edit::AddAuxSend {
                source: strip.id,
                aux: None,
            });
            ui.close();
        }
        let targets: Vec<_> = scene
            .strips
            .iter()
            .filter(|s| s.kind == StripKind::Aux && s.id != strip.id)
            .collect();
        if !targets.is_empty() {
            ui.separator();
        }
        for aux in targets {
            let already = scene
                .routes
                .iter()
                .any(|r| r.source == strip.id && r.destination == Destination::Bus(aux.id));
            let mut trial = scene.clone();
            let compatible = !already
                && trial
                    .edit(Edit::AddAuxSend {
                        source: strip.id,
                        aux: Some(aux.id),
                    })
                    .is_ok();
            let response = ui.add_enabled(compatible, egui::Button::new(&aux.name));
            if response.clicked() {
                self.edit(Edit::AddAuxSend {
                    source: strip.id,
                    aux: Some(aux.id),
                });
                ui.close();
            }
            if !compatible {
                response.on_disabled_hover_text(if already {
                    "This strip already has a send to this aux"
                } else {
                    "Channel layouts do not match, or this send would create a routing loop"
                });
            }
        }
    }
    pub(super) fn aux_rack(
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
                let rows = sends(strip, &snapshot.scene);
                let background = ui.interact(
                    ui.max_rect(),
                    ui.id().with(("aux-rack", strip.id)),
                    egui::Sense::click(),
                );
                if strip.kind == StripKind::Output {
                    background.on_hover_text(
                        "Terminal output. Aux sends are available on playback strips and buses.",
                    );
                    return;
                }
                if rows.is_empty() {
                    ui.painter().text(
                        ui.max_rect().center(),
                        egui::Align2::CENTER_CENTER,
                        "Right-click for aux sends",
                        egui::FontId::proportional(10.0),
                        DIM,
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt(("aux-send-rows", strip.id))
                    .max_height(HEIGHT - margins.y)
                    .min_scrolled_height(HEIGHT - margins.y)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        for send in rows {
                            let mut db = if send.gain <= 0.00001 {
                                -60.0
                            } else {
                                20.0 * send.gain.log10()
                            };
                            let response = ui.add(fader::Fader::horizontal(
                                &mut db,
                                (strip.id, send.aux),
                                &send.name,
                            ));
                            if response.changed() {
                                self.edit(Edit::SetAuxSend {
                                    source: strip.id,
                                    aux: send.aux,
                                    gain: Some(if db <= -60.0 {
                                        0.0
                                    } else {
                                        10.0_f32.powf(db / 20.0)
                                    }),
                                    tap: None,
                                });
                            }
                            response.clone().on_hover_text(format!(
                                "{} · {} · {}",
                                send.name,
                                if send.gain == 0.0 {
                                    "−∞ dB".into()
                                } else {
                                    format!("{:.1} dB", db)
                                },
                                match send.tap {
                                    Some(Tap::Pre) => "Pre-fader",
                                    Some(Tap::Post) => "Post-fader",
                                    None => "Mixed taps",
                                }
                            ));
                            response.context_menu(|ui| {
                                for (tap, label) in
                                    [(Tap::Pre, "Pre-fader"), (Tap::Post, "Post-fader")]
                                {
                                    if ui.selectable_label(send.tap == Some(tap), label).clicked() {
                                        self.edit(Edit::SetAuxSend {
                                            source: strip.id,
                                            aux: send.aux,
                                            gain: None,
                                            tap: Some(tap),
                                        });
                                        ui.close();
                                    }
                                }
                                ui.separator();
                                if ui.button("Delete send").clicked() {
                                    self.edit(Edit::RemoveAuxSend {
                                        source: strip.id,
                                        aux: send.aux,
                                    });
                                    ui.close();
                                }
                                ui.separator();
                                ui.menu_button("Add send…", |ui| {
                                    self.add_send_menu(ui, strip, &snapshot.scene)
                                });
                            });
                        }
                    });
                background.context_menu(|ui| self.add_send_menu(ui, strip, &snapshot.scene));
            })
            .response
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
        labels: Vec<(String, egui::Rect)>,
        rack: egui::Rect,
        master: Option<Id>,
    }
    impl Harness {
        fn new() -> Self {
            let (app, commands) = test_app();
            let strip = Strip::new("Player", StripKind::Stream, vec!["FL".into(), "FR".into()]);
            let snapshot = Snapshot {
                scene: Scene {
                    strips: vec![strip.clone()],
                    ..Default::default()
                },
                observation: Observation::default(),
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
                labels: vec![],
                rack: egui::Rect::NOTHING,
                master: None,
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
                        Vec2::new(900.0, 850.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(154.0);
                    if let Some(master) = self.master.and_then(|id| self.snapshot.scene.strip(id)) {
                        self.rack = self.app.strip(ui, master, &self.snapshot, STRIP_MIN_HEIGHT);
                    } else {
                        self.rack = self.app.aux_rack(ui, &self.strip, &self.snapshot).rect;
                    }
                    self.app.rename_strip_window(ui.ctx(), &self.snapshot);
                },
            );
            self.labels = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) => Some((
                        t.galley.text().to_owned(),
                        egui::Rect::from_min_size(t.pos, t.galley.size()),
                    )),
                    _ => None,
                })
                .collect();
            output.textures_delta.clear();
        }
        fn click(&mut self, pos: egui::Pos2, button: egui::PointerButton) {
            for pressed in [true, false] {
                self.frame(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: Default::default(),
                    },
                ]);
            }
            self.frame(vec![]);
        }
        fn label(&self, text: &str) -> egui::Pos2 {
            self.labels
                .iter()
                .find(|(s, _)| s == text)
                .unwrap_or_else(|| panic!("Missing {text}: {:?}", self.labels))
                .1
                .center()
        }
        fn click_label(&mut self, text: &str) {
            self.click(self.label(text), egui::PointerButton::Primary);
        }
        fn apply(&mut self) -> Vec<Edit> {
            let mut edits = vec![];
            for command in self.commands.try_iter() {
                if let Command::Edit(edit) = command {
                    self.snapshot.scene.edit(edit.clone()).unwrap();
                    edits.push(edit);
                }
            }
            self.frame(vec![]);
            edits
        }
        fn create(&mut self) -> Id {
            self.click(self.rack.center(), egui::PointerButton::Secondary);
            self.click_label("Create new aux");
            self.apply();
            self.snapshot
                .scene
                .strips
                .iter()
                .find(|s| s.kind == StripKind::Aux)
                .unwrap()
                .id
        }
    }
    #[test]
    fn aux_rack_creates_reuses_and_deletes_sends_without_deleting_the_master() {
        let mut h = Harness::new();
        let aux = h.create();
        assert_eq!(sends(&h.strip, &h.snapshot.scene)[0].gain, 0.0);
        assert!(h.snapshot.scene.routes.iter().all(|r| r.source != aux));
        h.click(h.label("Aux1"), egui::PointerButton::Secondary);
        h.click_label("Pre-fader");
        let edits = h.apply();
        assert!(
            matches!(
                edits.as_slice(),
                [Edit::SetAuxSend {
                    gain: None,
                    tap: Some(Tap::Pre),
                    ..
                }]
            ),
            "right clicking must not alter send gain"
        );
        h.click(h.label("Aux1"), egui::PointerButton::Primary);
        h.apply();
        assert!(sends(&h.strip, &h.snapshot.scene)[0].gain > 0.0);
        h.click(h.label("Aux1"), egui::PointerButton::Secondary);
        h.click_label("Delete send");
        h.apply();
        assert!(h.snapshot.scene.strip(aux).is_some());
        assert!(sends(&h.strip, &h.snapshot.scene).is_empty());
        h.click(h.rack.center(), egui::PointerButton::Secondary);
        h.click_label("Aux1");
        h.apply();
        assert_eq!(sends(&h.strip, &h.snapshot.scene).len(), 1);
        assert_eq!(h.snapshot.scene.strips.len(), 2);
    }
    #[test]
    fn aux_master_can_be_manually_deleted_from_its_name() {
        let mut h = Harness::new();
        let aux = h.create();
        h.master = Some(aux);
        h.frame(vec![]);
        h.click(h.label("Aux1"), egui::PointerButton::Secondary);
        h.click_label("Delete aux master");
        h.apply();
        assert!(h.snapshot.scene.strip(aux).is_none());
        assert!(h.snapshot.scene.routes.is_empty());
    }
    #[test]
    fn hiding_a_strip_collapses_it_and_the_bottom_button_restores_it() {
        let mut h = Harness::new();
        let root = std::env::temp_dir().join(format!("alixer-hidden-strip-{}", Id::new_v4()));
        h.app.preferences = preferences::Preferences::load(root.join("ui.json"));
        let aux = h.create();
        h.master = Some(aux);
        h.frame(vec![]);
        let scene = h.snapshot.scene.clone();
        let expanded = h.rack;
        h.app.selected = Some(aux);
        h.click(h.label("Aux1"), egui::PointerButton::Secondary);
        h.click_label("Hide");
        assert!(h.app.preferences.is_hidden(aux));
        assert_eq!(h.app.selected, None);
        assert!((h.rack.width() - 32.0).abs() < 0.1);
        assert!((h.rack.height() - expanded.height()).abs() < 0.1);
        assert!(h.label("»").y > h.rack.bottom() - 32.0);
        h.click_label("»");
        assert!(!h.app.preferences.is_hidden(aux));
        assert!((h.rack.width() - expanded.width()).abs() < 0.1);
        assert_eq!(h.snapshot.scene, scene);
        assert!(h.commands.try_recv().is_err());
        h.app.preferences.flush().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn renaming_an_aux_master_updates_every_send_label() {
        let mut h = Harness::new();
        let aux = h.create();
        let other = Strip::new(
            "Other playback",
            StripKind::Stream,
            vec!["FL".into(), "FR".into()],
        );
        h.snapshot.scene.strips.push(other.clone());
        h.snapshot
            .scene
            .edit(Edit::AddAuxSend {
                source: other.id,
                aux: Some(aux),
            })
            .unwrap();
        let routes = h.snapshot.scene.routes.clone();
        h.master = Some(aux);
        h.frame(vec![]);
        h.click(h.label("Aux1"), egui::PointerButton::Secondary);
        h.click_label("Rename…");
        assert_eq!(h.app.rename_strip.as_ref().unwrap().0, aux);
        h.app.rename_strip.as_mut().unwrap().1 = "Reverb".into();
        h.frame(vec![]);
        h.click_label("Rename");
        assert!(
            matches!(h.apply().as_slice(), [Edit::Rename { strip, name }] if *strip==aux && name=="Reverb")
        );
        assert_eq!(sends(&h.strip, &h.snapshot.scene)[0].name, "Reverb");
        assert_eq!(sends(&other, &h.snapshot.scene)[0].name, "Reverb");
        assert_eq!(h.snapshot.scene.routes, routes);
        h.master = None;
        h.frame(vec![]);
        h.label("Reverb");
    }
    #[test]
    fn aux_masters_are_displayed_between_playback_and_output_strips() {
        let (mut app, _commands) = test_app();
        app.show_inactive = true;
        let a = Strip::new(
            "Playback A",
            StripKind::Stream,
            vec!["FL".into(), "FR".into()],
        );
        let b = Strip::new("Playback B", StripKind::Stream, a.channels.clone());
        let mut output = Strip::new("Output master", StripKind::Output, a.channels.clone());
        output.output_key = Some("out".into());
        let aux = Strip::new("Aux1", StripKind::Aux, a.channels.clone());
        let snapshot = Snapshot {
            scene: Scene {
                strips: vec![a, output, aux, b],
                ..Default::default()
            },
            observation: Observation::default(),
            conflicts: vec![],
            status: String::new(),
            peaks: vec![],
            channel_peaks: vec![],
        };
        let ctx = egui::Context::default();
        let mut result = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1500.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| app.console(ui, &snapshot),
        );
        let x = |name: &str| {
            result
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) if t.galley.text() == name => Some(t.pos.x),
                    _ => None,
                })
                .unwrap()
        };
        assert!(x("Playback A") < x("Playback B"));
        assert!(x("Playback B") < x("Aux1"));
        assert!(x("Aux1") < x("Output master"));
        result.textures_delta.clear();
    }
    #[test]
    fn many_sends_scroll_without_resizing_the_rack() {
        let mut h = Harness::new();
        for _ in 0..20 {
            h.snapshot
                .scene
                .edit(Edit::AddAuxSend {
                    source: h.strip.id,
                    aux: None,
                })
                .unwrap();
        }
        for _ in 0..5 {
            h.frame(vec![]);
            assert!((h.rack.width() - 154.0).abs() < 0.1);
            assert!((h.rack.height() - HEIGHT).abs() < 0.1);
        }
        h.click(h.label("Aux1"), egui::PointerButton::Secondary);
        assert!(
            h.labels.iter().any(|(label, _)| label == "Add send…"),
            "full racks must still offer a way to add another send"
        );
    }
}
