use super::*;

const COLLAPSED_WIDTH: f32 = 32.0;

impl App {
    pub(super) fn strip_context_menu(&mut self, response: &egui::Response, strip: &Strip) {
        response.context_menu(|ui| {
            if ui.button("Rename…").clicked() {
                self.rename_strip = Some((strip.id, strip.name.clone()));
                ui.close();
            }
            if ui.button("Hide").clicked() {
                self.preferences.set_hidden(strip.id, true);
                if self.selected == Some(strip.id) {
                    self.selected = None;
                }
                ui.close();
            }
            if strip.kind == StripKind::Aux {
                ui.separator();
                if ui.button("Delete aux master").clicked() {
                    self.edit(Edit::RemoveStrip { strip: strip.id });
                    if self.selected == Some(strip.id) {
                        self.selected = None;
                    }
                    ui.close();
                }
            }
        });
    }

    pub(super) fn rename_strip_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some((id, mut name)) = self.rename_strip.clone() else {
            return;
        };
        let Some(strip) = snapshot.scene.strip(id) else {
            self.rename_strip = None;
            return;
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new("Rename channel")
            .id(egui::Id::new("rename-strip"))
            .open(&mut open).collapsible(false).resizable(false).default_width(320.0)
            .show(ctx, |ui| {
                ui.text_edit_singleline(&mut name);
                if strip.kind == StripKind::Output {
                    ui.label("Output names are shared with other audio applications after applying them.");
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Rename")).clicked() {
                        self.edit(Edit::Rename { strip: id, name: name.clone() });
                        self.names.insert(id, name.clone());
                        done = true;
                    }
                    if ui.button("Cancel").clicked() { done = true; }
                });
            });
        self.rename_strip = if open && !done {
            Some((id, name))
        } else {
            None
        };
    }

    pub(super) fn collapsed_strip(
        &mut self,
        ui: &mut egui::Ui,
        strip: &Strip,
        height: f32,
    ) -> egui::Rect {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(COLLAPSED_WIDTH, height), egui::Sense::hover());
        response.on_hover_text(&strip.name);
        ui.painter().rect_filled(rect, 8.0, PANEL);
        ui.painter().rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(0.5, Color32::from_gray(55)),
            egui::StrokeKind::Inside,
        );
        let color = Color32::from_rgb(strip.color[0], strip.color[1], strip.color[2]);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                rect.min + Vec2::new(6.0, 12.0),
                Vec2::new(COLLAPSED_WIDTH - 12.0, 3.0),
            ),
            2.0,
            color,
        );
        let name_rect = egui::Rect::from_min_max(
            rect.min + Vec2::new(4.0, 24.0),
            rect.max - Vec2::new(4.0, 40.0),
        );
        let handle = ui.interact(
            name_rect,
            ui.id().with(("collapsed-strip-drag", strip.id)),
            egui::Sense::click_and_drag(),
        );
        self.strip_drag_handle(&handle, strip);
        self.strip_context_menu(&handle, strip);
        let galley =
            ui.painter()
                .layout_no_wrap(strip.name.clone(), egui::FontId::proportional(13.0), DIM);
        ui.painter()
            .with_clip_rect(name_rect.intersect(ui.clip_rect()))
            .add(
                egui::epaint::TextShape::new(
                    egui::pos2(name_rect.left() + 3.0, name_rect.bottom()),
                    galley,
                    DIM,
                )
                .with_angle(-std::f32::consts::FRAC_PI_2),
            );
        let restore = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 4.0, rect.bottom() - 32.0),
            rect.max - Vec2::splat(4.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("restore-strip", strip.id))
                .max_rect(restore),
        );
        if child
            .add_sized(restore.size(), egui::Button::new("»"))
            .on_hover_text(format!("Restore {}", strip.name))
            .clicked()
        {
            self.preferences.set_hidden(strip.id, false);
        }
        rect
    }
}
