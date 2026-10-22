//! Dedicated editors for the curated LSP effects; no native plugin UI is loaded.
use super::*;
use alixer_core::effects::Effect;
use serde_json::Value;

const GOLD: Color32 = Color32::from_rgb(238, 191, 113);
fn number(port: &Value, key: &str, fallback: f32) -> f32 {
    port[key].as_f64().map(|n| n as f32).unwrap_or(fallback)
}
fn value(insert: &Insert, ports: &[Value], symbol: &str) -> f32 {
    insert.controls.get(symbol).copied().unwrap_or_else(|| {
        ports
            .iter()
            .find(|p| p["symbol"] == symbol)
            .map(|p| number(p, "default", 0.0))
            .unwrap_or(0.0)
    })
}
fn db(gain: f32) -> f32 {
    20.0 * gain.max(1e-9).log10()
}
fn gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}
// Zero is a real value for gain controls. It must remain reachable and distinct
// from the lowest positive dB value. The leftmost slider position means silence.
fn display_value(raw: f32, in_db: bool) -> f32 {
    if in_db {
        if raw <= 0.0 { -100.0 } else { db(raw) }
    } else {
        raw
    }
}
fn raw_value(shown: f32, in_db: bool, min: f32, max: f32) -> f32 {
    let raw = if in_db {
        if min == 0.0 && shown <= -100.0 {
            0.0
        } else {
            gain(shown)
        }
    } else {
        shown
    };
    raw.clamp(min, max)
}
fn housekeeping(symbol: &str) -> bool {
    matches!(
        symbol,
        "enabled"
            | "react"
            | "shift"
            | "zoom"
            | "fsel"
            | "quant"
            | "insp_id"
            | "insp_r"
            | "insp_on"
            | "pause"
            | "clear"
            | "showpmx"
            | "showmx"
            | "showsc"
    ) || symbol.starts_with("hue_")
        || [
            "ife", "ofe", "rfe", "slv", "elv", "grv", "ilv", "olv", "igv", "ogv", "scgv", "grgv",
        ]
        .iter()
        .any(|prefix| symbol == *prefix || symbol.starts_with(&format!("{prefix}_")))
}
fn external_sidechain(symbol: &str) -> bool {
    matches!(symbol, "in2lk" | "lk2in" | "lk2sc" | "sct" | "extsc")
}
impl App {
    pub(super) fn effect_editor_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some((strip_id, insert_id)) = self.selected_insert else {
            return;
        };
        let Some(strip) = snapshot.scene.strip(strip_id) else {
            self.selected_insert = None;
            return;
        };
        let Some(insert) = strip.inserts.iter().find(|i| i.id == insert_id) else {
            self.selected_insert = None;
            return;
        };
        let plugin = self
            .catalog
            .as_array()
            .and_then(|p| p.iter().find(|p| p["uri"] == insert.uri))
            .cloned();
        let effect = Effect::from_uri(&insert.uri);
        let mut open = true;
        egui::Window::new(format!(
            "{} · {}",
            self.plugin_name(&insert.uri),
            strip.name
        ))
        .id(egui::Id::new(("effect-editor", insert_id)))
        .default_size(Vec2::new(820.0, 680.0))
        .min_size(Vec2::new(560.0, 400.0))
        .open(&mut open)
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(self.plugin_name(&insert.uri))
                        .size(26.0)
                        .color(MINT),
                );
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        strip.name,
                        if strip.channels.len() == 1 {
                            "MONO"
                        } else {
                            "STEREO"
                        }
                    ))
                    .color(DIM),
                );
                let mut bypass = insert.bypass;
                if ui.checkbox(&mut bypass, "Bypass").changed() {
                    self.edit(Edit::BypassInsert {
                        strip: strip_id,
                        insert: insert_id,
                        bypass,
                    });
                }
            });
            ui.separator();
            let Some(plugin) = &plugin else {
                ui.label("Effect metadata unavailable. Refresh the catalog to retry.");
                if ui.button("Refresh plugin catalog").clicked() {
                    self.send(Command::Plugins);
                }
                return;
            };
            let Some(ports) = plugin["ports"].as_array() else {
                return;
            };
            egui::ScrollArea::vertical()
                .id_salt(("editor-scroll", insert_id))
                .show(ui, |ui| {
                    if let Some(effect) = effect {
                        if ports.iter().any(|p| p["symbol"] == "enabled")
                            && value(insert, ports, "enabled") == 0.0
                        {
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    GOLD,
                                    "Processing is disabled in this saved plugin.",
                                );
                                if ui.button("Enable processing").clicked() {
                                    self.edit(Edit::SetControl {
                                        strip: strip_id,
                                        insert: insert_id,
                                        symbol: "enabled".into(),
                                        value: 1.0,
                                    });
                                }
                            });
                        }
                        self.effect_meters(ui, snapshot, insert, effect);
                        match effect {
                            Effect::Equalizer => self.eq_editor(ui, strip_id, insert, ports),
                            Effect::Compressor | Effect::Limiter => {
                                self.dynamics_editor(ui, strip_id, insert, ports, snapshot, effect)
                            }
                        }
                        // Keep newly added DSP parameters accessible after plugin upgrades.
                        // Presentation-only and external-sidechain ports are intentionally omitted.
                        let known = known_symbols(effect);
                        let remaining: Vec<_> = ports
                            .iter()
                            .filter(|p| {
                                let symbol = p["symbol"].as_str().unwrap_or_default();
                                p["control"] == true
                                    && p["input"] == true
                                    && !housekeeping(symbol)
                                    && !external_sidechain(symbol)
                                    && !known.contains(&symbol)
                                    && !(effect == Effect::Equalizer
                                        && symbol.rsplit_once('_').is_some_and(
                                            |(prefix, index)| {
                                                index.parse::<usize>().is_ok()
                                                    && [
                                                        "ft", "fm", "s", "xs", "xm", "f", "w", "g",
                                                        "q",
                                                    ]
                                                    .contains(&prefix)
                                            },
                                        ))
                            })
                            .collect();
                        if !remaining.is_empty() {
                            ui.collapsing("Additional processing controls", |ui| {
                                for port in remaining {
                                    self.effect_control(ui, strip_id, insert, ports, port);
                                }
                            });
                        }
                        if insert
                            .controls
                            .iter()
                            .any(|(symbol, v)| external_sidechain(symbol) && *v != 0.0)
                        {
                            ui.colored_label(
                                GOLD,
                                "This saved effect uses an external sidechain or link mix.",
                            );
                            if ui.button("Use internal input").clicked() {
                                for symbol in ["in2lk", "lk2in", "lk2sc", "sct", "extsc"] {
                                    if ports.iter().any(|p| p["symbol"] == symbol) {
                                        self.edit(Edit::SetControl {
                                            strip: strip_id,
                                            insert: insert_id,
                                            symbol: symbol.into(),
                                            value: 0.0,
                                        });
                                    }
                                }
                            }
                        }
                    } else {
                        ui.label(RichText::new("Legacy insert · generic controls").color(DIM));
                        for port in ports
                            .iter()
                            .filter(|p| p["control"] == true && p["input"] == true)
                        {
                            self.effect_control(ui, strip_id, insert, ports, port);
                        }
                    }
                });
        });
        if !open {
            self.selected_insert = None;
        }
    }
    fn dynamics_editor(
        &mut self,
        ui: &mut egui::Ui,
        strip: Id,
        insert: &Insert,
        ports: &[Value],
        snapshot: &Snapshot,
        effect: Effect,
    ) {
        let groups: &[(&str, &[&str])] = if effect == Effect::Compressor {
            meter_history(ui, snapshot, insert, "rlm", "Gain change", GOLD);
            &[
                (
                    "Compression",
                    &[
                        "cm", "al", "cr", "kn", "at", "rt", "hold", "rrl", "bth", "bsa", "mk",
                    ],
                ),
                (
                    "Detector",
                    &[
                        "scm", "scs", "scr", "scp", "sla", "scl", "ssplit", "sscs", "shpm", "shpf",
                        "slpm", "slpf",
                    ],
                ),
                ("Signal", &["g_in", "g_out", "cdr", "cwt", "cdw"]),
            ]
        } else {
            meter_history(ui, snapshot, insert, "grlm", "Gain reduction", GOLD);
            &[
                (
                    "Limiting",
                    &[
                        "mode", "th", "knee", "smooth", "boost", "lk", "at", "rt", "slink",
                    ],
                ),
                ("Level regulation", &["alr", "alr_at", "alr_rt"]),
                ("Signal & quality", &["g_in", "g_out", "scp", "ovs", "dith"]),
            ]
        };
        let key = egui::Id::new(("dynamics-tab", insert.id));
        let mut tab = ui
            .ctx()
            .data_mut(|d| d.get_temp::<usize>(key).unwrap_or(0))
            .min(groups.len() - 1);
        ui.horizontal(|ui| {
            for (index, (title, _)) in groups.iter().enumerate() {
                if ui.selectable_label(tab == index, *title).clicked() {
                    tab = index;
                }
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(key, tab));
        self.control_section(ui, strip, insert, ports, groups[tab].0, groups[tab].1);
    }
    fn control_section(
        &mut self,
        ui: &mut egui::Ui,
        strip: Id,
        insert: &Insert,
        ports: &[Value],
        title: &str,
        symbols: &[&str],
    ) {
        ui.add_space(8.0);
        let width = ui.available_width();
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.set_min_width((width - 26.0).max(0.0));
                ui.spacing_mut().slider_width = (width - 390.0).clamp(100.0, 360.0);
                ui.label(RichText::new(title).size(17.0).color(MINT));
                for symbol in symbols {
                    if let Some(port) = ports.iter().find(|p| p["symbol"] == *symbol) {
                        self.effect_control(ui, strip, insert, ports, port);
                    }
                }
            });
    }
    fn effect_control(
        &mut self,
        ui: &mut egui::Ui,
        strip: Id,
        insert: &Insert,
        ports: &[Value],
        port: &Value,
    ) {
        let symbol = port["symbol"].as_str().unwrap_or_default();
        let metadata_name = port["name"].as_str().unwrap_or(symbol);
        let name = if Effect::from_uri(&insert.uri) == Some(Effect::Equalizer) {
            match symbol.rsplit_once('_').map(|(prefix, _)| prefix) {
                Some("ft") => "Filter type",
                Some("fm") => "Filter algorithm",
                Some("s") => "Slope",
                Some("f") => "Frequency",
                Some("g") => "Gain",
                Some("q") => "Q",
                Some("w") => "Width",
                Some("xs") => "Solo",
                Some("xm") => "Mute",
                _ => metadata_name,
            }
        } else {
            metadata_name
        };
        let min = number(port, "min", 0.0);
        let max = number(port, "max", 1.0).max(min);
        let raw = value(insert, ports, symbol);
        let unit = port["unit"].as_str().unwrap_or_default();
        let in_db = unit == "G";
        let mut shown = display_value(raw, in_db);
        let lo = display_value(min, in_db);
        let hi = display_value(max, in_db);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.push_id((insert.id, symbol), |ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(250.0, 20.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(egui::Label::new(name).wrap());
                    },
                );
                if port["toggle"] == true {
                    let mut on = raw > 0.0;
                    changed = ui.checkbox(&mut on, "").changed();
                    shown = if on { 1.0 } else { 0.0 };
                } else if let Some(choices) = port["choices"].as_array().filter(|c| !c.is_empty()) {
                    let current = choices
                        .iter()
                        .find(|c| c[0].as_f64().is_some_and(|v| (v as f32 - raw).abs() < 0.01))
                        .and_then(|c| c[1].as_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{raw}"));
                    egui::ComboBox::from_id_salt("choice")
                        .selected_text(current)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for choice in choices {
                                if let (Some(v), Some(label)) =
                                    (choice[0].as_f64(), choice[1].as_str())
                                {
                                    if ui
                                        .selectable_label((v as f32 - raw).abs() < 0.01, label)
                                        .clicked()
                                    {
                                        shown = v as f32;
                                        changed = true;
                                    }
                                }
                            }
                        });
                } else {
                    let mut slider = egui::Slider::new(&mut shown, lo..=hi)
                        .logarithmic(
                            !in_db
                                && unit != "%"
                                && (port["logarithmic"] == true
                                    || (Effect::from_uri(&insert.uri) == Some(Effect::Equalizer)
                                        && symbol.starts_with("q_"))),
                        )
                        .smallest_positive(if min > 0.0 { min as f64 } else { 0.01 })
                        .suffix(if in_db {
                            " dB".to_owned()
                        } else if unit.is_empty() {
                            String::new()
                        } else {
                            format!(" {unit}")
                        });
                    if in_db {
                        let has_silence = min == 0.0;
                        slider = slider
                            .custom_formatter(move |v, _| {
                                if has_silence && v <= -100.0 {
                                    "−∞".into()
                                } else {
                                    format!("{v:.2}")
                                }
                            })
                            .custom_parser(move |text| {
                                if has_silence && ["−∞", "-inf", "-infinity"].contains(&text.trim())
                                {
                                    Some(-100.0)
                                } else {
                                    text.parse().ok()
                                }
                            });
                    }
                    if port["integer"] == true && !in_db {
                        slider = slider.integer();
                    }
                    let response = ui.add(slider).on_hover_text(
                        "Drag to adjust · click the number to type · double-click to reset",
                    );
                    changed = response.changed();
                    if response.double_clicked() {
                        shown = display_value(number(port, "default", 0.0), in_db);
                        changed = true;
                    }
                }
            });
        });
        if changed {
            self.edit(Edit::SetControl {
                strip,
                insert: insert.id,
                symbol: symbol.into(),
                value: raw_value(shown, in_db && port["toggle"] != true, min, max),
            });
        }
    }
    fn eq_editor(&mut self, ui: &mut egui::Ui, strip: Id, insert: &Insert, ports: &[Value]) {
        let selection = egui::Id::new(("eq-band", insert.id));
        let mut selected = ui
            .ctx()
            .data_mut(|d| d.get_temp::<usize>(selection).unwrap_or(0));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("BAND FREQUENCY / GAIN").small().color(DIM));
            ui.label("Drag a band · scroll over it to adjust Q");
        });
        let (outer, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 235.0), egui::Sense::hover());
        let plot = outer.shrink2(Vec2::new(35.0, 24.0));
        let painter = ui.painter_at(outer);
        painter.rect_filled(outer, 6.0, BG);
        let position = |frequency: f32, level: f32| {
            egui::pos2(
                plot.left()
                    + (frequency.clamp(10.0, 24000.0) / 10.0).ln() / 2400.0_f32.ln() * plot.width(),
                plot.center().y - level.clamp(-36.0, 36.0) / 72.0 * plot.height(),
            )
        };
        for frequency in [20.0, 100.0, 1000.0, 10000.0, 20000.0] {
            let x = position(frequency, 0.0).x;
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                (1.0, Color32::from_gray(45)),
            );
            painter.text(
                egui::pos2(x, plot.bottom() + 8.0),
                egui::Align2::CENTER_TOP,
                if frequency >= 1000.0 {
                    format!("{}k", frequency / 1000.0)
                } else {
                    format!("{frequency}")
                },
                egui::FontId::proportional(11.0),
                DIM,
            );
        }
        for level in [-36.0, -18.0, 0.0, 18.0, 36.0] {
            let y = position(1000.0, level).y;
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                (
                    1.0,
                    if level == 0.0 {
                        Color32::from_gray(80)
                    } else {
                        Color32::from_gray(40)
                    },
                ),
            );
            painter.text(
                egui::pos2(plot.left() - 5.0, y),
                egui::Align2::RIGHT_CENTER,
                format!("{level:+.0}"),
                egui::FontId::proportional(11.0),
                DIM,
            );
        }
        for band in 0..8 {
            let f = format!("f_{band}");
            let g = format!("g_{band}");
            let q = format!("q_{band}");
            let active = value(insert, ports, &format!("ft_{band}")) != 0.0
                && value(insert, ports, &format!("xm_{band}")) == 0.0;
            let point = position(value(insert, ports, &f), db(value(insert, ports, &g)));
            let response = ui.interact(
                egui::Rect::from_center_size(point, Vec2::splat(24.0)),
                ui.id().with(("eq-handle", insert.id, band)),
                egui::Sense::click_and_drag(),
            );
            if response.clicked() || response.dragged() {
                selected = band;
            }
            if response.drag_started() && value(insert, ports, &format!("ft_{band}")) == 0.0 {
                self.edit(Edit::SetControl {
                    strip,
                    insert: insert.id,
                    symbol: format!("ft_{band}"),
                    value: 1.0,
                });
            }
            if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let frequency = 10.0
                        * 2400.0_f32
                            .powf(((pointer.x - plot.left()) / plot.width()).clamp(0.0, 1.0));
                    let level =
                        ((plot.center().y - pointer.y) / plot.height() * 72.0).clamp(-36.0, 36.0);
                    for (symbol, raw) in [(&f, frequency), (&g, gain(level))] {
                        if let Some(port) = ports.iter().find(|p| p["symbol"] == symbol.as_str()) {
                            self.edit(Edit::SetControl {
                                strip,
                                insert: insert.id,
                                symbol: symbol.clone(),
                                value: raw
                                    .clamp(number(port, "min", 0.0), number(port, "max", 1.0)),
                            });
                        }
                    }
                }
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    if let Some(port) = ports.iter().find(|p| p["symbol"] == q) {
                        self.edit(Edit::SetControl {
                            strip,
                            insert: insert.id,
                            symbol: q,
                            value: (value(insert, ports, &format!("q_{band}")) + scroll * 0.01)
                                .clamp(number(port, "min", 0.0), number(port, "max", 100.0)),
                        });
                    }
                    ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
                }
            }
            response.on_hover_text(format!(
                "Band {} · {:.0} Hz · {:+.1} dB{}",
                band + 1,
                value(insert, ports, &f),
                db(value(insert, ports, &g)),
                if active { "" } else { " · off / muted" }
            ));
            let color = if selected == band {
                GOLD
            } else if active {
                MINT
            } else {
                DIM
            };
            painter.circle_filled(point, 10.0, if active { color } else { PANEL });
            painter.circle_stroke(point, 10.0, (1.5, color));
            painter.text(
                point,
                egui::Align2::CENTER_CENTER,
                format!("{}", band + 1),
                egui::FontId::proportional(12.0),
                if active { BG } else { color },
            );
        }
        ui.horizontal_wrapped(|ui| {
            for band in 0..8 {
                if ui
                    .selectable_label(selected == band, format!("Band {}", band + 1))
                    .clicked()
                {
                    selected = band;
                }
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(selection, selected));
        let symbols: Vec<String> = ["ft", "fm", "s", "f", "g", "q", "w", "xs", "xm"]
            .iter()
            .map(|prefix| format!("{prefix}_{selected}"))
            .collect();
        let refs: Vec<_> = symbols.iter().map(String::as_str).collect();
        self.control_section(
            ui,
            strip,
            insert,
            ports,
            &format!("Band {}", selected + 1),
            &refs,
        );
        self.control_section(
            ui,
            strip,
            insert,
            ports,
            "Signal & processing",
            &["g_in", "g_out", "bal", "frqs", "mode", "decramp"],
        );
        ui.label(RichText::new("The graph positions bands by frequency and gain; it does not depict the combined filter response.").small().color(DIM));
    }
    fn effect_meters(
        &self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        insert: &Insert,
        effect: Effect,
    ) {
        let meters: Vec<_> = snapshot
            .plugin_meters
            .iter()
            .filter(|(id, _, _)| *id == insert.id)
            .collect();
        if insert.bypass {
            ui.label(RichText::new("BYPASSED").color(GOLD));
            return;
        }
        if meters.is_empty() {
            ui.label(
                RichText::new(
                    if snapshot.status.starts_with("Offline")
                        || snapshot.status.starts_with("Disconnected")
                    {
                        "Effect meters unavailable while audio is offline."
                    } else {
                        "Waiting for live effect meters…"
                    },
                )
                .color(DIM),
            );
            return;
        }
        let width = ((ui.available_width() - 24.0)
            / if effect == Effect::Equalizer {
                2.0
            } else {
                3.0
            })
        .max(110.0);
        ui.horizontal(|ui| {
            for (label, prefixes, reduction) in match effect {
                Effect::Equalizer => vec![
                    ("INPUT", vec!["im", "iml", "imr"], false),
                    ("OUTPUT", vec!["sm", "sml", "smr"], false),
                ],
                Effect::Compressor => vec![
                    ("INPUT", vec!["ilm", "ilm_l", "ilm_r"], false),
                    ("OUTPUT", vec!["olm", "olm_l", "olm_r"], false),
                    ("GAIN CHANGE", vec!["rlm", "rlm_l", "rlm_r"], true),
                ],
                Effect::Limiter => vec![
                    ("INPUT", vec!["ilm", "ilm_l", "ilm_r"], false),
                    ("OUTPUT", vec!["olm", "olm_l", "olm_r"], false),
                    ("REDUCTION", vec!["grlm", "grlm_l", "grlm_r"], true),
                ],
            } {
                let values: Vec<_> = meters
                    .iter()
                    .filter(|(_, symbol, _)| prefixes.contains(&symbol.as_str()))
                    .map(|(_, _, v)| *v)
                    .collect();
                if values.is_empty() {
                    continue;
                }
                let level = if reduction {
                    values.into_iter().fold(f32::INFINITY, f32::min)
                } else {
                    values.into_iter().fold(0.0_f32, f32::max)
                };
                let level_db = db(level);
                let text = if level <= 0.0 {
                    "−∞ dB".into()
                } else if reduction {
                    format!("{:+.1} dB", level_db)
                } else {
                    format!("{:.1} dBFS", level_db)
                };
                ui.allocate_ui(Vec2::new(width, 60.0), |ui| {
                    ui.label(RichText::new(label).small().color(DIM));
                    ui.label(RichText::new(text).size(21.0).color(if reduction {
                        GOLD
                    } else {
                        MINT
                    }));
                    let fraction = if reduction {
                        (-level_db / 36.0).clamp(0.0, 1.0)
                    } else {
                        ((level_db + 60.0) / 60.0).clamp(0.0, 1.0)
                    };
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(width)
                            .fill(if reduction { GOLD } else { MINT }),
                    );
                });
            }
        });
    }
}
fn known_symbols(effect: Effect) -> &'static [&'static str] {
    match effect {
        Effect::Equalizer => &["g_in", "g_out", "bal", "frqs", "mode", "decramp"],
        Effect::Compressor => &[
            "cm", "al", "cr", "kn", "at", "rt", "hold", "rrl", "bth", "bsa", "mk", "scm", "scs",
            "scr", "scp", "sla", "scl", "ssplit", "sscs", "shpm", "shpf", "slpm", "slpf", "g_in",
            "g_out", "cdr", "cwt", "cdw",
        ],
        Effect::Limiter => &[
            "mode", "th", "knee", "smooth", "boost", "lk", "at", "rt", "slink", "alr", "alr_at",
            "alr_rt", "g_in", "g_out", "scp", "ovs", "dith",
        ],
    }
}
#[derive(Clone, Default)]
struct History {
    points: std::collections::VecDeque<(f64, f32)>,
    last: f64,
}
fn meter_history(
    ui: &mut egui::Ui,
    snapshot: &Snapshot,
    insert: &Insert,
    prefix: &str,
    title: &str,
    color: Color32,
) {
    let key = egui::Id::new(("gain-history", insert.id));
    let now = ui.input(|i| i.time);
    let current = snapshot
        .plugin_meters
        .iter()
        .filter(|(id, s, _)| {
            *id == insert.id && (s == prefix || s.starts_with(&format!("{prefix}_")))
        })
        .map(|(_, _, v)| db(*v))
        .reduce(f32::min);
    let mut history = ui
        .ctx()
        .data_mut(|d| d.get_temp::<History>(key).unwrap_or_default());
    if now - history.last >= 0.03 {
        if let Some(current) = current.filter(|_| !insert.bypass) {
            history.points.push_back((now, current));
        }
        while history
            .points
            .front()
            .is_some_and(|(time, _)| now - time > 8.0)
        {
            history.points.pop_front();
        }
        history.last = now;
    }
    ui.label(
        RichText::new(format!("{} · last 8 seconds", title.to_uppercase()))
            .small()
            .color(DIM),
    );
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 105.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, BG);
    let top = if prefix == "rlm" { 12.0 } else { 0.0 };
    let span = top + 36.0;
    for level in [12.0, 0.0, -12.0, -24.0, -36.0]
        .into_iter()
        .filter(|v| *v <= top)
    {
        let y = rect.top() + 8.0 + ((top - level) / span) * (rect.height() - 20.0);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            (1.0, Color32::from_gray(45)),
        );
        painter.text(
            egui::pos2(rect.left() + 5.0, y + 2.0),
            egui::Align2::LEFT_TOP,
            format!("{level:.0} dB"),
            egui::FontId::proportional(10.0),
            DIM,
        );
    }
    // Positive gain changes are shown above the 0 dB line, within the plot bounds.
    let points: Vec<_> = history
        .points
        .iter()
        .map(|(time, level)| {
            egui::pos2(
                rect.right() - ((now - time) / 8.0) as f32 * rect.width(),
                (rect.top() + 8.0 + (top - level) / span * (rect.height() - 20.0))
                    .clamp(rect.top(), rect.bottom()),
            )
        })
        .collect();
    if points.len() > 1 {
        painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, color)));
    }
    ui.ctx().data_mut(|d| d.insert_temp(key, history));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gain_controls_roundtrip_including_silence_and_full_range() {
        for (min, max) in [(0.0, 1000.0), (0.01585, 63.09575), (0.001, 1.0)] {
            for raw in [min, 0.25_f32.clamp(min, max), 1.0, max] {
                let restored = raw_value(display_value(raw, true), true, min, max);
                assert!(
                    (restored - raw).abs() <= raw.abs() * 1e-5 + 1e-7,
                    "{raw} -> {restored}"
                );
            }
        }
    }
    struct Harness {
        app: App,
        commands: mpsc::Receiver<Command>,
        snapshot: Snapshot,
        ctx: egui::Context,
        labels: Vec<(String, egui::Rect)>,
        handles: Vec<egui::Pos2>,
        time: f64,
    }
    impl Harness {
        fn new(plugin: Value, channels: usize) -> Self {
            let (mut app, commands) = test_app();
            let mut strip = Strip::new("Desktop", StripKind::Group, vec!["MONO".into(); channels]);
            let insert = Insert {
                id: Id::new_v4(),
                uri: plugin["uri"].as_str().unwrap().into(),
                bypass: false,
                controls: BTreeMap::new(),
                state: None,
            };
            app.selected_insert = Some((strip.id, insert.id));
            app.catalog = serde_json::json!([plugin]);
            app.catalog_requested = true;
            strip.inserts.push(insert);
            let snapshot = Snapshot {
                active_streams: None,
                scene: Scene {
                    strips: vec![strip],
                    ..Default::default()
                },
                observation: Observation::default(),
                conflicts: vec![],
                status: "Connected".into(),
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
                handles: vec![],
                time: 0.0,
            };
            for _ in 0..4 {
                h.frame(vec![]);
            }
            h
        }
        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.1;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    events,
                    time: Some(self.time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1200.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    self.app.effect_editor_window(ui.ctx(), &self.snapshot);
                },
            );
            self.labels = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((
                        t.galley.text().to_owned(),
                        egui::Rect::from_min_size(t.pos, t.galley.size()),
                    )),
                    _ => None,
                })
                .collect();
            self.handles = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Circle(c) if c.radius == 10.0 => Some(c.center),
                    _ => None,
                })
                .collect();
            output.textures_delta.clear();
        }
        fn pointer(&mut self, pos: egui::Pos2, pressed: bool) {
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
        fn click_label(&mut self, label: &str) {
            let pos = self
                .labels
                .iter()
                .find(|(s, _)| s == label)
                .unwrap_or_else(|| panic!("Missing {label}: {:?}", self.labels))
                .1
                .center();
            self.pointer(pos, true);
            self.pointer(pos, false);
            self.frame(vec![]);
        }
    }
    #[test]
    fn legacy_insert_keeps_a_functional_fallback_editor() {
        let mut h = Harness::new(
            serde_json::json!({"uri":"urn:legacy:effect","name":"Legacy effect","ports":[{"symbol":"enabled","name":"Enabled","control":true,"input":true,"toggle":true,"default":1,"min":0,"max":1}]}),
            2,
        );
        assert!(
            h.labels
                .iter()
                .any(|(s, _)| s == "Legacy insert · generic controls")
        );
        h.click_label("Bypass");
        assert!(matches!(
            h.commands.try_recv().unwrap(),
            Command::Edit(Edit::BypassInsert { bypass: true, .. })
        ));
    }
    #[test]
    fn mode_selector_sends_the_plugins_declared_value() {
        let mut h = Harness::new(
            serde_json::json!({
                "uri":"http://lsp-plug.in/plugins/lv2/compressor_stereo", "name":"Compressor",
                "ports":[{"symbol":"cm","name":"Compression mode","control":true,"input":true,
                    "default":0,"min":0,"max":2,"choices":[[0,"Downward"],[2,"Boosting"]]}]
            }),
            2,
        );
        h.click_label("Downward");
        h.click_label("Boosting");
        assert!(
            matches!(h.commands.try_recv().unwrap(), Command::Edit(Edit::SetControl {symbol,value,..}) if symbol == "cm" && value == 2.0)
        );
    }
    #[test]
    fn installed_effect_editors_render_and_eq_drag_edits_the_correct_parameters() {
        let Some(path) = std::env::var_os("ALIXER_EFFECT_CATALOG_PATH") else {
            return;
        };
        let catalog: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for channels in [1, 2] {
            for effect in Effect::ALL {
                let uri = effect.uri(channels).unwrap();
                let plugin = catalog.iter().find(|p| p["uri"] == uri).unwrap().clone();
                let mut h = Harness::new(plugin, channels);
                assert!(h.labels.iter().any(|(s, _)| s == effect.name()));
                assert!(!h.labels.iter().any(|(s, _)| {
                    [
                        "Sidechain type",
                        "Input to Link mix",
                        "Link to Input mix",
                        "Link to Sidechain mix",
                    ]
                    .contains(&s.as_str())
                }));
                if effect == Effect::Equalizer {
                    let point = h.handles[0];
                    h.pointer(point, true);
                    let moved = point + Vec2::new(80.0, -25.0);
                    h.frame(vec![egui::Event::PointerMoved(moved)]);
                    h.pointer(moved, false);
                    let edits: Vec<_> = h.commands.try_iter().collect();
                    assert!(edits.iter().any(|e|matches!(e,Command::Edit(Edit::SetControl {symbol,value,..}) if symbol=="f_0" && *value>16.0)));
                    assert!(edits.iter().any(|e|matches!(e,Command::Edit(Edit::SetControl {symbol,value,..}) if symbol=="g_0" && *value>1.0)));
                } else {
                    h.click_label("Bypass");
                    assert!(matches!(
                        h.commands.try_recv().unwrap(),
                        Command::Edit(Edit::BypassInsert { bypass: true, .. })
                    ));
                }
            }
        }
    }
}
