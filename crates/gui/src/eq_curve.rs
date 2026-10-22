//! Analog prototype response preview, independent of the audio callback.
//! Common transfer functions follow the analog prototypes in the Audio EQ
//! Cookbook: https://www.w3.org/TR/audio-eq-cookbook/ . LSP's digital algorithms,
//! slope mappings and decramping differ, so this remains an approximate preview.
use super::effect_editor::value;
use super::*;
use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) struct Band {
    pub index: usize,
    pub frequency: f64,
    kind: u8,
    gain: f64,
    q: f64,
    sections: f64,
}
impl Band {
    pub fn read(insert: &Insert, ports: &[Value]) -> Vec<Self> {
        let solo = (0..8).any(|i| value(insert, ports, &format!("xs_{i}")) > 0.0);
        (0..8)
            .filter_map(|index| {
                let get = |prefix: &str| value(insert, ports, &format!("{prefix}_{index}"));
                let kind = get("ft") as u8;
                let is_solo = get("xs") > 0.0;
                if kind == 0 || get("xm") > 0.0 || (solo && !is_solo) {
                    return None;
                }
                Some(Self {
                    index,
                    kind,
                    frequency: (get("f") as f64).clamp(10.0, 24000.0),
                    gain: (get("g") as f64).clamp(1e-6, 1e6),
                    q: if get("q") > 0.0 {
                        (get("q") as f64).max(0.01)
                    } else {
                        std::f64::consts::FRAC_1_SQRT_2
                    },
                    sections: (get("s") as f64 + 1.0).clamp(1.0, 4.0),
                })
            })
            .collect()
    }
    pub fn supported(self) -> bool {
        self.kind <= 9
    }
    pub fn response(self, frequency: f64) -> f64 {
        let r = frequency / self.frequency;
        let r2 = r * r;
        let a = self.gain.powf(0.5 / self.sections);
        let damping = (r / self.q).powi(2);
        let base = (1.0 - r2).powi(2);
        let denominator = base + damping;
        let power = match self.kind {
            1 | 7 => (base + damping * a * a) / (base + damping / (a * a)),
            2 => r2 * r2 / denominator,
            4 => 1.0 / denominator,
            3 => a * a * ((1.0 - a * r2).powi(2) + a * damping) / ((a - r2).powi(2) + a * damping),
            5 => a * a * ((a - r2).powi(2) + a * damping) / ((1.0 - a * r2).powi(2) + a * damping),
            6 => base / denominator,
            9 => damping / denominator,
            // Allpass has a flat magnitude. Ladder filters are deliberately
            // omitted rather than drawing an unrelated curve for them.
            _ => 1.0,
        };
        (10.0 * power.max(1e-12).log10() * self.sections).clamp(-240.0, 240.0)
    }
}

pub(super) fn draw(
    ui: &egui::Ui,
    plot: egui::Rect,
    insert: &Insert,
    ports: &[Value],
    selected: usize,
) {
    let bands = Band::read(insert, ports);
    let visible: Vec<_> = bands.iter().copied().filter(|b| b.supported()).collect();
    // Uniform log samples plus local samples around every band resolve even
    // narrow high-Q peaks/notches, without making the whole plot extremely dense.
    let mut frequencies: Vec<f64> = (0..=256)
        .map(|i| 10.0 * 2400.0_f64.powf(i as f64 / 256.0))
        .collect();
    for band in &visible {
        let bandwidth = 2.0 * (1.0 / (2.0 * band.q)).asinh() / std::f64::consts::LN_2;
        for step in -24..=24 {
            frequencies.push(
                (band.frequency * 2.0_f64.powf(bandwidth * step as f64 / 12.0))
                    .clamp(10.0, 24000.0),
            );
        }
    }
    frequencies.sort_by(f64::total_cmp);
    frequencies.dedup();
    let point = |frequency: f64, level: f64| {
        egui::pos2(
            plot.left() + ((frequency / 10.0).ln() / 2400.0_f64.ln()) as f32 * plot.width(),
            plot.center().y - (level.clamp(-36.0, 36.0) / 72.0) as f32 * plot.height(),
        )
    };
    let painter = ui.painter_at(plot);
    let enabled = !insert.bypass
        && (!ports.iter().any(|p| p["symbol"] == "enabled")
            || value(insert, ports, "enabled") != 0.0);
    if enabled {
        if visible.len() > 1 {
            if let Some(band) = visible.iter().find(|b| b.index == selected) {
                painter.add(egui::Shape::line(
                    frequencies
                        .iter()
                        .map(|f| point(*f, band.response(*f)))
                        .collect(),
                    egui::Stroke::new(1.2, Color32::from_rgba_unmultiplied(238, 191, 113, 130)),
                ));
            }
        }
        let color = if visible.len() == 1 && visible[0].index == selected {
            Color32::from_rgb(238, 191, 113)
        } else {
            MINT
        };
        painter.add(egui::Shape::line(
            frequencies
                .iter()
                .map(|f| point(*f, visible.iter().map(|b| b.response(*f)).sum()))
                .collect(),
            egui::Stroke::new(2.0, color),
        ));
    } else {
        painter.line_segment(
            [
                egui::pos2(plot.left(), plot.center().y),
                egui::pos2(plot.right(), plot.center().y),
            ],
            (2.0, DIM),
        );
    }
    if bands.iter().any(|b| !b.supported()) {
        painter.text(
            plot.left_bottom() - Vec2::new(-6.0, 6.0),
            egui::Align2::LEFT_BOTTOM,
            "Ladder filters omitted from preview",
            egui::FontId::proportional(11.0),
            DIM,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alixer_core::effects::Effect;
    fn bell(q: f64, gain: f64) -> Band {
        Band {
            index: 0,
            frequency: 1000.0,
            kind: 1,
            gain,
            q,
            sections: 1.0,
        }
    }
    #[test]
    fn bell_reaches_its_gain_at_center_and_q_changes_width() {
        let wide = bell(0.7, 2.0);
        let narrow = bell(12.0, 2.0);
        assert!((wide.response(1000.0) - 20.0 * 2.0_f64.log10()).abs() < 1e-9);
        assert!((wide.response(1000.0) - narrow.response(1000.0)).abs() < 1e-9);
        assert!(narrow.response(1500.0) < wide.response(1500.0));
        for f in [10.0, 100.0, 500.0, 1000.0, 5000.0, 24000.0] {
            assert!((wide.response(f) + bell(0.7, 0.5).response(f)).abs() < 1e-9);
        }
    }
    #[test]
    fn filters_have_expected_asymptotes_and_remain_finite() {
        let mut band = bell(0.7, 2.0);
        band.kind = 2;
        assert!(band.response(10.0) < -70.0 && band.response(24000.0).abs() < 0.1);
        band.kind = 4;
        assert!(band.response(10.0).abs() < 0.1 && band.response(24000.0) < -50.0);
        band.kind = 3;
        assert!(
            band.response(10.0).abs() < 0.1
                && (band.response(24000.0) - 20.0 * 2.0_f64.log10()).abs() < 0.1
        );
        band.kind = 5;
        assert!(
            (band.response(10.0) - 20.0 * 2.0_f64.log10()).abs() < 0.1
                && band.response(24000.0).abs() < 0.1
        );
        for kind in 1..=11 {
            for q in [0.01, 0.7, 100.0] {
                band.kind = kind;
                band.q = q;
                for f in [10.0, 999.99, 1000.0, 1000.01, 24000.0] {
                    assert!(band.response(f).is_finite());
                }
            }
        }
    }
    #[test]
    fn muted_off_and_unsoloed_bands_are_excluded() {
        let mut insert = Insert {
            id: Id::new_v4(),
            uri: Effect::Equalizer.uri(2).unwrap(),
            bypass: false,
            controls: BTreeMap::new(),
            state: None,
        };
        for i in 0..3 {
            insert.controls.insert(format!("ft_{i}"), 1.0);
        }
        insert.controls.insert("xm_1".into(), 1.0);
        assert_eq!(Band::read(&insert, &[]).len(), 2);
        insert.controls.insert("xs_2".into(), 1.0);
        let bands = Band::read(&insert, &[]);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].index, 2);
    }
}
