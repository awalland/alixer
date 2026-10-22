use super::*;
use alixer_core::effects::Effect;

impl App {
    pub(super) fn ensure_catalog(&mut self) {
        if self.catalog.is_null() && !self.catalog_requested {
            self.send(Command::Plugins);
        }
    }

    pub(super) fn open_plugin_picker(&mut self, strip: Id) {
        self.selected = Some(strip);
        self.plugin_picker = Some(strip);
        self.filter.clear();
        self.ensure_catalog();
    }

    pub(super) fn plugin_name(&self, uri: &str) -> String {
        if let Some(effect) = Effect::from_uri(uri) {
            return effect.name().into();
        }
        self.catalog
            .as_array()
            .and_then(|plugins| plugins.iter().find(|p| p["uri"].as_str() == Some(uri)))
            .and_then(|p| p["name"].as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                uri.rsplit(['/', '#'])
                    .find(|s| !s.is_empty())
                    .unwrap_or(uri)
            })
            .to_owned()
    }

    fn insert_menu(&mut self, ui: &mut egui::Ui, strip: &Strip, index: Option<usize>) {
        if ui.button("Add effect…").clicked() {
            self.open_plugin_picker(strip.id);
            ui.close();
        }
        if let Some(index) = index {
            let insert = &strip.inserts[index];
            ui.separator();
            if ui.button("Open editor…").clicked() {
                self.selected = Some(strip.id);
                self.selected_insert = Some((strip.id, insert.id));
                self.ensure_catalog();
                ui.close();
            }
            let mut bypass = insert.bypass;
            if ui.checkbox(&mut bypass, "Bypass").changed() {
                self.edit(Edit::BypassInsert {
                    strip: strip.id,
                    insert: insert.id,
                    bypass,
                });
                ui.close();
            }
            for (up, label) in [(true, "Move up"), (false, "Move down")] {
                let enabled = if up {
                    index > 0
                } else {
                    index + 1 < strip.inserts.len()
                };
                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                    let mut ids: Vec<_> = strip.inserts.iter().map(|i| i.id).collect();
                    ids.swap(index, if up { index - 1 } else { index + 1 });
                    self.edit(Edit::ReorderInserts {
                        strip: strip.id,
                        inserts: ids,
                    });
                    ui.close();
                }
            }
            ui.separator();
            if ui
                .button(RichText::new("Remove effect").color(RED))
                .clicked()
            {
                self.edit(Edit::RemoveInsert {
                    strip: strip.id,
                    insert: insert.id,
                });
                ui.close();
            }
        }
    }

    pub(super) fn plugin_rack(
        &mut self,
        ui: &mut egui::Ui,
        strip: &Strip,
        height: f32,
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
                ui.set_height(height - margins.y);
                let background = ui.interact(
                    ui.max_rect(),
                    ui.id().with(("insert-rack", strip.id)),
                    egui::Sense::click(),
                );
                if strip.inserts.is_empty() {
                    ui.painter().text(
                        ui.max_rect().center(),
                        egui::Align2::CENTER_CENTER,
                        "Right-click to add\neffects",
                        egui::FontId::proportional(11.0),
                        DIM,
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt(("insert-rows", strip.id))
                    .max_height(height - margins.y)
                    .min_scrolled_height(height - margins.y)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        for (index, insert) in strip.inserts.iter().enumerate() {
                            let name = self.plugin_name(&insert.uri);
                            let text = RichText::new(format!(
                                "{} {name}",
                                if insert.bypass { "○" } else { "●" }
                            ))
                            .color(if insert.bypass {
                                DIM
                            } else {
                                MINT
                            });
                            let row = ui
                                .add_sized(
                                    [ui.available_width(), 23.0],
                                    egui::Button::selectable(
                                        self.selected_insert == Some((strip.id, insert.id)),
                                        (text, egui::Atom::grow()),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(format!(
                                    "{name}\n{}\nClick to edit · Right-click for actions{}",
                                    insert.uri,
                                    if insert.bypass { "\nBypassed" } else { "" }
                                ));
                            if row.clicked() {
                                self.selected = Some(strip.id);
                                self.selected_insert = Some((strip.id, insert.id));
                                self.ensure_catalog();
                            }
                            row.context_menu(|ui| self.insert_menu(ui, strip, Some(index)));
                        }
                    });
                background.context_menu(|ui| self.insert_menu(ui, strip, None));
            })
            .response
    }

    pub(super) fn plugin_picker_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some(id) = self.plugin_picker else {
            return;
        };
        let Some(strip) = snapshot.scene.strip(id) else {
            self.plugin_picker = None;
            return;
        };
        let mut open = true;
        egui::Window::new("Add effect")
            .id(egui::Id::new("plugin-picker"))
            .open(&mut open)
            .default_size(Vec2::new(680.0, 480.0))
            .min_size(Vec2::new(320.0, 220.0))
            .resizable(true)
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(format!(
                        "{} · {} channels",
                        strip.name,
                        strip.channels.len()
                    ))
                    .truncate(),
                )
                .on_hover_text(&strip.name);
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Search effects…")
                        .desired_width(f32::INFINITY),
                );
                if ui
                    .add_enabled(
                        !self.catalog_loading,
                        egui::Button::new("Refresh plugin catalog"),
                    )
                    .clicked()
                {
                    self.send(Command::Plugins);
                }
                if self.catalog.is_null() {
                    ui.label(if self.catalog_error.is_empty() {
                        "Loading plugins…"
                    } else {
                        "Plugin catalog unavailable. Refresh to retry."
                    });
                }
                if !self.catalog_error.is_empty() {
                    ui.colored_label(RED, &self.catalog_error);
                }
                let candidates: Vec<_> = self
                    .catalog
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| matches_plugin(p, strip.channels.len(), &self.filter))
                    .cloned()
                    .collect();
                if candidates.is_empty() && self.catalog.is_array() {
                    ui.label(if strip.channels.len() > 2 {
                        "These effects support mono and stereo strips."
                    } else if !self.filter.trim().is_empty() {
                        "No supported effects match this search."
                    } else {
                        "Install the LSP LV2 package to use EQ, compressor and limiter, then refresh."
                    });
                }
                egui::ScrollArea::vertical()
                    .id_salt("plugin-picker-list")
                    .max_height(ui.available_height().max(1.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for plugin in &candidates {
                            let Some(uri) = plugin["uri"].as_str() else { continue; };
                            let Some(effect) = Effect::from_uri(uri) else { continue; };
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new(effect.name()).size(20.0).color(MINT));
                                        ui.label(effect.description());
                                        ui.label(RichText::new("LSP · LV2").small().color(DIM));
                                    });
                                    if ui.button(format!("Add {}", effect.name())).clicked() {
                                        self.edit(Edit::AddInsert { strip: id, uri: uri.to_owned() });
                                        self.plugin_picker = None;
                                    }
                                });
                            });
                        }
                    });
            });
        if !open {
            self.plugin_picker = None;
        }
    }
}

fn matches_plugin(plugin: &serde_json::Value, channels: usize, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    let name = plugin["name"].as_str().unwrap_or_default();
    let uri = plugin["uri"].as_str().unwrap_or_default();
    let Some(ports) = plugin["ports"].as_array() else {
        return false;
    };
    (name.to_lowercase().contains(&query)
        || uri.to_lowercase().contains(&query)
        || Effect::from_uri(uri).is_some_and(|e| e.name().to_lowercase().contains(&query)))
        && Effect::from_uri(uri).is_some()
        && plugin["unsupported"]
            .as_array()
            .is_some_and(|requirements| requirements.is_empty())
        && ports
            .iter()
            .filter(|p| p["audio"] == true && p["input"] == true)
            .count()
            == channels
        && ports
            .iter()
            .filter(|p| p["audio"] == true && p["input"] == false)
            .count()
            == channels
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plugin() -> serde_json::Value {
        serde_json::json!({"name":"Parametric EQ","uri":"http://lsp-plug.in/plugins/lv2/para_equalizer_x8_stereo","unsupported":[],"ports":[
            {"audio":true,"input":true},{"audio":true,"input":true},
            {"audio":true,"input":false},{"audio":true,"input":false}
        ]})
    }
    struct Harness {
        app: App,
        commands: mpsc::Receiver<Command>,
        ctx: egui::Context,
        snapshot: Snapshot,
        strip: Strip,
        time: f64,
        labels: Vec<(String, egui::Rect)>,
        rack: egui::Rect,
    }
    impl Harness {
        fn new(populated: bool) -> Self {
            let (mut app, commands) = test_app();
            app.catalog = serde_json::json!([plugin()]);
            app.catalog_requested = true;
            let mut strip = Strip::new("Music", StripKind::Stream, vec!["FL".into(), "FR".into()]);
            if populated {
                strip.inserts.push(Insert {
                    id: Id::new_v4(),
                    uri: "http://lsp-plug.in/plugins/lv2/para_equalizer_x8_stereo".into(),
                    bypass: false,
                    controls: BTreeMap::new(),
                    state: None,
                });
            }
            let snapshot = Snapshot {
                active_streams: None,
                scene: Scene {
                    strips: vec![strip.clone()],
                    ..Default::default()
                },
                observation: Observation::default(),
                conflicts: vec![],
                status: String::new(),
                peaks: vec![],
                channel_peaks: vec![],
                plugin_meters: vec![],
            };
            let mut this = Self {
                app,
                commands,
                ctx: egui::Context::default(),
                snapshot,
                strip,
                time: 0.0,
                labels: vec![],
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
                        Vec2::new(900.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(154.0);
                    self.rack = self.app.plugin_rack(ui, &self.strip, 160.0).rect;
                    self.app.plugin_picker_window(ui.ctx(), &self.snapshot);
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
        fn click_label(&mut self, text: &str) {
            let pos = self
                .labels
                .iter()
                .find(|(label, _)| label == text)
                .unwrap_or_else(|| panic!("Missing {text}: {:?}", self.labels))
                .1
                .center();
            self.click(pos, egui::PointerButton::Primary);
        }
    }
    #[test]
    fn empty_rack_context_menu_can_add_a_plugin() {
        let mut h = Harness::new(false);
        h.click(h.rack.center(), egui::PointerButton::Secondary);
        h.click_label("Add effect…");
        assert_eq!(h.app.plugin_picker, Some(h.strip.id));
        h.click_label("Add Parametric EQ");
        assert!(
            matches!(h.commands.try_recv().unwrap(),Command::Edit(Edit::AddInsert{strip,uri}) if strip==h.strip.id&&uri=="http://lsp-plug.in/plugins/lv2/para_equalizer_x8_stereo")
        );
        assert_eq!(h.app.plugin_picker, None);
    }
    #[test]
    fn insert_rows_select_controls_and_offer_bypass() {
        let mut h = Harness::new(true);
        let insert = h.strip.inserts[0].id;
        h.click_label("● Parametric EQ");
        assert_eq!(h.app.selected_insert, Some((h.strip.id, insert)));
        let pos = h
            .labels
            .iter()
            .find(|(text, _)| text == "● Parametric EQ")
            .unwrap()
            .1
            .center();
        h.click(pos, egui::PointerButton::Secondary);
        h.click_label("Bypass");
        assert!(
            matches!(h.commands.try_recv().unwrap(),Command::Edit(Edit::BypassInsert{strip,insert:id,bypass:true})if strip==h.strip.id&&id==insert)
        );
    }
    #[test]
    fn picker_filters_search_and_channel_layout() {
        let p = plugin();
        assert!(matches_plugin(&p, 2, " eq "));
        assert!(matches_plugin(&p, 2, "PARA_EQUALIZER"));
        assert!(!matches_plugin(&p, 1, ""));
        assert!(!matches_plugin(&p, 2, "compressor"));
        let mut unrelated = p.clone();
        unrelated["uri"] = serde_json::json!("urn:another:equalizer");
        assert!(!matches_plugin(&unrelated, 2, ""));
        let mut unsupported = p.clone();
        unsupported["unsupported"] = serde_json::json!(["Unsupported port type: events"]);
        assert!(!matches_plugin(&unsupported, 2, ""));
        let mut supported_urid = p;
        supported_urid["required"] = serde_json::json!(["http://lv2plug.in/ns/ext/urid#map"]);
        assert!(matches_plugin(&supported_urid, 2, ""));
    }
}
