use eframe::egui::{self, Color32, Id, Pos2, Rect, Response, Sense, Vec2, Widget};

const MIN_DB: f32 = -60.0;
const MAX_DB: f32 = 12.0;
const HANDLE_HEIGHT: f32 = 12.0;

#[derive(Clone, Copy)]
struct Drag {
    value: f32,
    last_position: f32,
    on_handle: bool,
}

pub struct Fader<'a> {
    db: &'a mut f32,
    salt: Id,
    label: Option<&'a str>,
}

impl<'a> Fader<'a> {
    pub fn new(db: &'a mut f32, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        Self {
            db,
            salt: Id::new(salt),
            label: None,
        }
    }
    pub fn horizontal(
        db: &'a mut f32,
        salt: impl std::hash::Hash + std::fmt::Debug,
        label: &'a str,
    ) -> Self {
        Self {
            db,
            salt: Id::new(salt),
            label: Some(label),
        }
    }
}

pub(super) fn db_y(rect: Rect, db: f32) -> f32 {
    let travel = rect.height() - HANDLE_HEIGHT;
    rect.bottom()
        - HANDLE_HEIGHT / 2.0
        - (db.clamp(MIN_DB, MAX_DB) - MIN_DB) / (MAX_DB - MIN_DB) * travel
}
fn handle(rect: Rect, db: f32) -> Rect {
    let y = db_y(rect, db);
    Rect::from_center_size(
        Pos2::new(rect.center().x, y),
        Vec2::new(rect.width(), HANDLE_HEIGHT),
    )
}
fn horizontal_handle(rect: Rect, db: f32) -> Rect {
    let x = rect.left()
        + HANDLE_HEIGHT / 2.0
        + (db - MIN_DB) / (MAX_DB - MIN_DB) * (rect.width() - HANDLE_HEIGHT);
    Rect::from_center_size(
        Pos2::new(x, rect.center().y),
        Vec2::new(HANDLE_HEIGHT, rect.height()),
    )
}

impl Widget for Fader<'_> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let horizontal = self.label.is_some();
        let size = if horizontal {
            Vec2::new(ui.available_width(), 23.0)
        } else {
            Vec2::new(18.0, ui.spacing().slider_width)
        };
        let (_, rect) = ui.allocate_space(size);
        let position = |pos: Pos2| if horizontal { pos.x } else { -pos.y };
        let handle_rect = |db| {
            if horizontal {
                horizontal_handle(rect, db)
            } else {
                handle(rect, db)
            }
        };
        let id = ui.id().with(self.salt);
        let mut response = ui.interact(rect, id, Sense::click_and_drag());
        let (pointer, pressed, released, shift) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
                i.modifiers.shift,
            )
        });
        let mut drag = ui.data(|d| d.get_temp::<Drag>(id));
        let travel = (if horizontal {
            rect.width()
        } else {
            rect.height()
        } - HANDLE_HEIGHT)
            .max(1.0);
        let mut changed = false;
        if pressed && response.is_pointer_button_down_on() {
            if let Some(pos) = pointer {
                let on_handle = handle_rect(*self.db).expand(3.0).contains(pos);
                let value = if on_handle {
                    *self.db
                } else {
                    (MIN_DB
                        + (position(pos)
                            - if horizontal {
                                rect.left() + HANDLE_HEIGHT / 2.0
                            } else {
                                -rect.bottom() + HANDLE_HEIGHT / 2.0
                            })
                            / travel
                            * (MAX_DB - MIN_DB))
                        .clamp(MIN_DB, MAX_DB)
                };
                changed = value != *self.db;
                drag = Some(Drag {
                    value,
                    last_position: position(pos),
                    on_handle,
                });
                response.request_focus();
            }
        }
        if let Some(state) = &mut drag {
            if response.is_pointer_button_down_on() || response.drag_stopped() || response.clicked()
            {
                if let Some(pos) = pointer {
                    let speed = if shift { 0.5 } else { 1.0 };
                    let value = (state.value
                        + (position(pos) - state.last_position) / travel
                            * (MAX_DB - MIN_DB)
                            * speed)
                        .clamp(MIN_DB, MAX_DB);
                    changed |= value != state.value;
                    state.value = value;
                    state.last_position = position(pos);
                }
                if state.on_handle && response.double_clicked() {
                    changed |= state.value != 0.0;
                    state.value = 0.0;
                }
                // Keep dragging responsive even while the service snapshot lags behind.
                *self.db = state.value;
            }
        }
        if released || !ui.is_enabled() {
            ui.data_mut(|d| d.remove::<Drag>(id));
        } else if let Some(state) = drag {
            ui.data_mut(|d| d.insert_temp(id, state));
        }
        if response.has_focus() && ui.is_enabled() {
            let step = if shift { 0.5 } else { 1.0 };
            let direction = ui.input(|i| {
                i.num_presses(if horizontal {
                    egui::Key::ArrowRight
                } else {
                    egui::Key::ArrowUp
                }) as f32
                    - i.num_presses(if horizontal {
                        egui::Key::ArrowLeft
                    } else {
                        egui::Key::ArrowDown
                    }) as f32
            });
            if direction != 0.0 {
                let value = (*self.db + direction * step).clamp(MIN_DB, MAX_DB);
                changed |= value != *self.db;
                *self.db = value;
            }
        }
        if changed {
            response.mark_changed();
        }
        response.widget_info(|| {
            egui::WidgetInfo::slider(
                ui.is_enabled(),
                *self.db as f64,
                self.label.unwrap_or("Gain (dB)"),
            )
        });
        let knob = handle_rect(*self.db);
        let track = if horizontal {
            rect
        } else {
            Rect::from_center_size(rect.center(), Vec2::new(6.0, travel))
        };
        let visuals = ui.style().interact(&response);
        ui.painter()
            .rect_filled(track, 3.0, ui.visuals().extreme_bg_color);
        let fill = if horizontal {
            Rect::from_min_max(track.left_top(), Pos2::new(knob.center().x, track.bottom()))
        } else {
            Rect::from_min_max(
                Pos2::new(track.left(), knob.center().y),
                track.right_bottom(),
            )
        };
        ui.painter().rect_filled(
            fill,
            3.0,
            if ui.is_enabled() {
                ui.visuals().selection.bg_fill
            } else {
                Color32::from_gray(45)
            },
        );
        ui.painter().rect(
            knob,
            2.0,
            visuals.bg_fill,
            visuals.fg_stroke,
            egui::StrokeKind::Inside,
        );
        if let Some(label) = self.label {
            ui.painter().with_clip_rect(rect.shrink(2.0)).text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.0),
                ui.visuals().text_color(),
            );
        }
        response.on_hover_text("Double-click handle: 0 dB\nShift-drag: half speed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Harness {
        ctx: egui::Context,
        db: f32,
        rect: Rect,
        time: f64,
        horizontal: bool,
    }
    impl Harness {
        fn new(db: f32) -> Self {
            let mut this = Self {
                ctx: egui::Context::default(),
                db,
                rect: Rect::NOTHING,
                time: 0.0,
                horizontal: false,
            };
            this.frame(vec![], false);
            this
        }
        fn frame(&mut self, mut events: Vec<egui::Event>, shift: bool) -> bool {
            self.time += 0.03;
            let mut changed = false;
            events.insert(
                0,
                egui::Event::ModifiersChanged(egui::Modifiers {
                    shift,
                    ..Default::default()
                }),
            );
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    time: Some(self.time),
                    events,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0))),
                    ..Default::default()
                },
                |ui| {
                    ui.spacing_mut().slider_width = 200.0;
                    ui.set_width(200.0);
                    let response = ui.add(if self.horizontal {
                        Fader::horizontal(&mut self.db, "test", "Aux1")
                    } else {
                        Fader::new(&mut self.db, "test")
                    });
                    self.rect = response.rect;
                    changed = response.changed();
                },
            );
            output.textures_delta.clear();
            changed
        }
        fn button(&mut self, pos: Pos2, pressed: bool, shift: bool) -> bool {
            self.frame(
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers {
                            shift,
                            ..Default::default()
                        },
                    },
                ],
                shift,
            )
        }
        fn move_to(&mut self, pos: Pos2, shift: bool) {
            self.frame(vec![egui::Event::PointerMoved(pos)], shift);
        }
    }
    #[test]
    fn double_click_handle_resets_to_unity() {
        let mut h = Harness::new(-12.0);
        let pos = handle(h.rect, h.db).center();
        h.button(pos, true, false);
        h.button(pos, false, false);
        assert_eq!(h.db, -12.0);
        h.button(pos, true, false);
        assert!(h.button(pos, false, false));
        assert_eq!(h.db, 0.0);
    }
    #[test]
    fn shift_drag_is_half_speed_without_a_grab_jump() {
        let mut changes = vec![];
        for shift in [false, true] {
            let mut h = Harness::new(-24.0);
            let pos = handle(h.rect, h.db).center() + Vec2::new(0.0, 4.0);
            h.button(pos, true, shift);
            assert_eq!(h.db, -24.0);
            h.move_to(pos - Vec2::new(0.0, 40.0), shift);
            changes.push(h.db + 24.0);
        }
        assert!(changes[0] > 0.0);
        assert!((changes[1] * 2.0 - changes[0]).abs() < 0.001);
    }
    #[test]
    fn modifiers_can_change_mid_drag_and_stale_snapshots_do_not_reset_the_drag() {
        let mut h = Harness::new(-36.0);
        let start = handle(h.rect, h.db).center();
        h.button(start, true, false);
        h.move_to(start - Vec2::new(0.0, 20.0), false);
        let first = h.db + 36.0;
        h.db = -36.0; // Simulate an older service snapshot arriving during the gesture.
        h.move_to(start - Vec2::new(0.0, 40.0), true);
        assert!((h.db - (-36.0 + first * 1.5)).abs() < 0.001);
        h.move_to(start - Vec2::new(0.0, 60.0), false);
        assert!((h.db - (-36.0 + first * 2.5)).abs() < 0.001);
    }
    #[test]
    fn horizontal_send_faders_support_half_speed_and_double_click_reset() {
        let mut changes = vec![];
        for shift in [false, true] {
            let mut h = Harness::new(-24.0);
            h.horizontal = true;
            h.frame(vec![], false);
            let start = horizontal_handle(h.rect, h.db).center();
            h.button(start, true, shift);
            h.move_to(start + Vec2::new(40.0, 0.0), shift);
            changes.push(h.db + 24.0);
        }
        assert!((changes[0] - 2.0 * changes[1]).abs() < 0.001);
        let mut h = Harness::new(-12.0);
        h.horizontal = true;
        h.frame(vec![], false);
        let start = horizontal_handle(h.rect, h.db).center();
        h.button(start, true, false);
        h.button(start, false, false);
        h.button(start, true, false);
        h.button(start, false, false);
        assert_eq!(h.db, 0.0);
    }
}
