//! Convolve's panel: the impulse it convolves with, drawn from the same
//! maker the effect uses (a space's as its envelope over time, in dB,
//! with its length; a box's as its frequency response), redrawn as Size,
//! Attack, Decay and Damp reshape it, with a note while the effect is
//! making the new one; the wet level beside it; then the impulse menu and
//! the knobs.

use effects::convolve::{self, Impulse, METER_LENGTH, METER_MAKING, METER_WET, Shaping};
use eframe::egui::{self, Align2, FontId, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use super::Panel;
use super::panel::display;
use crate::widgets::scopes;

/// The rate the panel makes a space's impulse at to draw it: plenty for an
/// envelope, and quick.
const DRAW_RATE: f32 = 8000.0;
/// How far down the envelope is drawn.
const FLOOR_DB: f32 = -60.0;

/// The drawing worked out for one shaping, kept between frames.
#[derive(Clone, Default)]
struct Drawing {
    key: Vec<u32>,
    /// A space's envelope (dB, one value per column) or a box's response
    /// (dB at log-spaced frequencies from 20 Hz to 20 kHz).
    values: Vec<f32>,
    seconds: f32,
}

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let switches = crate::theme::size::CONTROL + crate::theme::space::M;
    let height = 2.0 * panel.fit_knobs_beside(ui, 2, switches) + switches;
    ui.horizontal_top(|ui| {
        impulse_display(ui, panel, vec2(height * 1.9, height));
        let (rect, _) = ui.allocate_exact_size(
            vec2(crate::widgets::scopes::level_bars_width(1), height),
            Sense::hover(),
        );
        let wet = dsp::gain_to_db(panel.meters[METER_WET].max(1e-6));
        scopes::level_bars(ui.painter(), rect, &[wet], -60.0, 6.0, ui.visuals());
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.weak("Impulse");
                panel.choice_menu(ui, "impulse");
            });
            panel.knobs(ui, &["size", "predelay", "attack", "decay", "damp"]);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                panel.knob(ui, "tone");
                panel.knob(ui, "ir_gain");
                panel.knobs(ui, &["mix", "output"]);
            });
        });
    });
}

/// Works the drawing out for these values, `columns` wide.
fn draw(values: &[f32], columns: usize, sample_rate: f32) -> Drawing {
    let shaping = Shaping::of(values);
    let key = draw_key(values, columns);
    if shaping.impulse.is_box() {
        // The response at log-spaced frequencies, from the impulse at the
        // session's rate (it's short).
        let ir = convolve::impulse(values, sample_rate);
        let values = (0..columns)
            .map(|i| {
                let hz = 20.0 * 1000f32.powf(i as f32 / (columns - 1).max(1) as f32);
                let step = std::f64::consts::TAU * f64::from(hz) / f64::from(sample_rate);
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (n, &h) in ir[0].iter().enumerate() {
                    re += f64::from(h) * (step * n as f64).cos();
                    im -= f64::from(h) * (step * n as f64).sin();
                }
                dsp::gain_to_db((re * re + im * im).sqrt() as f32)
            })
            .collect();
        return Drawing {
            key,
            values,
            seconds: shaping.seconds(),
        };
    }
    let ir = convolve::impulse(values, DRAW_RATE);
    let len = ir[0].len().max(1);
    let per = len.div_ceil(columns.max(1));
    let loudest = ir[0]
        .iter()
        .chain(&ir[1])
        .fold(1e-9f32, |m, s| m.max(s.abs()));
    let values = (0..columns)
        .map(|i| {
            let span = (i * per).min(len)..((i + 1) * per).min(len);
            let peak = ir[0][span.clone()]
                .iter()
                .chain(&ir[1][span])
                .fold(0.0f32, |m, s| m.max(s.abs()));
            dsp::gain_to_db(peak / loudest)
        })
        .collect();
    Drawing {
        key,
        values,
        seconds: len as f32 / DRAW_RATE,
    }
}

fn impulse_display(ui: &mut Ui, panel: &mut Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(8.0, 14.0));
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();
    let values = panel.values().to_vec();
    let impulse = Impulse::from_index(panel.value("impulse"));
    let columns = (rect.width() / 2.0).max(8.0) as usize;

    let state_id = panel.id("impulse drawing");
    let cached: Option<Drawing> = ui.data(|d| d.get_temp(state_id));
    let wanted = draw_key(&values, columns);
    let drawing = match cached {
        Some(drawing) if drawing.key == wanted => drawing,
        _ => {
            let drawing = draw(&values, columns, panel.sample_rate);
            ui.data_mut(|d| d.insert_temp(state_id, drawing.clone()));
            drawing
        }
    };

    let grid = Stroke::new(1.0, weak.gamma_multiply(0.2));
    let x_of = |i: usize| rect.left() + rect.width() * i as f32 / (columns - 1).max(1) as f32;
    if impulse.is_box() {
        // Its response, 20 Hz to 20 kHz, -36 to +12 dB.
        let y_of = |db: f32| rect.top() + (12.0 - db.clamp(-36.0, 12.0)) / 48.0 * rect.height();
        for db in [0.0, -24.0] {
            painter.hline(rect.x_range(), y_of(db), grid);
        }
        let span = 1000f32.ln();
        for (hz, text) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
            let x = rect.left() + (hz / 20.0f32).ln() / span * rect.width();
            painter.vline(x, rect.y_range(), grid);
            painter.text(
                pos2(x + 2.0, rect.bottom() + 1.0),
                Align2::LEFT_TOP,
                text,
                FontId::proportional(crate::theme::text::TINY),
                weak,
            );
        }
        let peak = drawing.values.iter().fold(f32::MIN, |m, &v| m.max(v));
        let points: Vec<egui::Pos2> = drawing
            .values
            .iter()
            .enumerate()
            .map(|(i, &db)| pos2(x_of(i), y_of(db - peak)))
            .collect();
        painter.add(Shape::line(points, Stroke::new(1.8, accent)));
    } else {
        // Its envelope over time, in dB.
        let y_of = |db: f32| rect.top() + (db / FLOOR_DB).clamp(0.0, 1.0) * rect.height();
        for db in [-20.0, -40.0] {
            painter.hline(rect.x_range(), y_of(db), grid);
        }
        let mut fill = egui::Mesh::default();
        let points: Vec<egui::Pos2> = drawing
            .values
            .iter()
            .enumerate()
            .map(|(i, &db)| pos2(x_of(i), y_of(db)))
            .collect();
        for pair in points.windows(2) {
            fill.add_colored_rect(
                Rect::from_min_max(pair[0], pos2(pair[1].x, rect.bottom())),
                accent.gamma_multiply(0.18),
            );
        }
        painter.add(fill);
        painter.add(Shape::line(points, Stroke::new(1.4, accent)));
    }

    let length = if panel.meters[METER_LENGTH] > 0.0 {
        panel.meters[METER_LENGTH]
    } else {
        drawing.seconds
    };
    let name = convolve::IMPULSES[impulse as usize];
    let label = if length >= 1.0 {
        format!("{name} · {length:.2} s")
    } else {
        format!("{name} · {:.0} ms", length * 1000.0)
    };
    painter.text(
        response.rect.left_top() + super::panel::DISPLAY_INSET,
        Align2::LEFT_TOP,
        label,
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );
    if panel.meters[METER_MAKING] > 0.0 {
        painter.text(
            response.rect.right_top()
                + vec2(
                    -super::panel::DISPLAY_INSET.x,
                    super::panel::DISPLAY_INSET.y,
                ),
            Align2::RIGHT_TOP,
            "making…",
            FontId::proportional(crate::theme::text::SMALL),
            weak,
        );
    }
}

/// [`Drawing::key`] for these values without drawing.
fn draw_key(values: &[f32], columns: usize) -> Vec<u32> {
    let shaping = Shaping::of(values);
    [
        shaping.impulse as u32,
        shaping.stretch.to_bits(),
        shaping.attack.to_bits(),
        shaping.decay.to_bits(),
        shaping.damp.to_bits(),
        columns as u32,
    ]
    .to_vec()
}
