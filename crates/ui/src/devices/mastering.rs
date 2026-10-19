//! The clipper's panel, in the spirit of StandardCLIP: the transfer curve
//! with the ceiling and knee, the last few seconds as a waveform with what
//! was clipped off in red above what came out, in, clip and out meters,
//! and big readouts of how much is clipped, how often, the loudest true
//! peak out (click to clear) and how much louder it all got.

use std::collections::VecDeque;

use effects::clipper::{
    self, METER_CLIP, METER_CLIPPING, METER_INPUT_PEAK, METER_INPUT_RMS, METER_OUTPUT_PEAK,
    METER_OUTPUT_RMS, METER_SAFETY,
};
use eframe::egui::{
    self, Align2, Color32, FontId, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};

use super::Panel;
use super::panel::{KNOB_TEXT, display};
use crate::widgets::scopes::{self, REDUCTION};

/// The level range the curve and the waveform show.
const LOW_DB: f32 = -36.0;
const HIGH_DB: f32 = 12.0;
/// How much clipping the clip meter has room for.
const CLIP_RANGE: f32 = 12.0;
/// How much the waveform shows.
const SECONDS: f32 = 3.0;
/// What's clipped off.
const CLIPPED: Color32 = Color32::from_rgb(235, 80, 65);

/// What the panel remembers between frames: the waveform's columns, built
/// from the tap with the input lined up with the output.
#[derive(Clone, Default)]
struct View {
    /// Per column: the loudest driven input and output, as amplitudes.
    columns: VecDeque<[f32; 2]>,
    current: [f32; 2],
    count: usize,
    seen: u64,
    /// The driven input, waiting to meet the output it became.
    waiting: VecDeque<[f32; 2]>,
    /// What the columns were made for: width, frames per column, latency.
    shape: (usize, usize, usize),
    /// The loudest true peak out since it was last cleared (dB).
    loudest: f32,
    pass: u64,
}

pub(super) fn clipper(ui: &mut Ui, panel: &mut Panel) {
    let height =
        (ui.available_height() - (panel.knob_size + KNOB_TEXT) - super::panel::CONTROL_ROW)
            .clamp(90.0, 170.0);
    ui.horizontal(|ui| {
        panel.choice(ui, "shape");
        ui.separator();
        panel.choice_menu(ui, "oversampling");
        ui.separator();
        panel.choice(ui, "safety");
        ui.separator();
        panel.toggle(ui, "delta", "Delta");
    });
    let id = panel.id("clipper view");
    let mut view: View = ui.data_mut(|d| d.remove_temp(id)).unwrap_or_else(|| View {
        loudest: f32::NEG_INFINITY,
        ..View::default()
    });
    let fresh = view.pass != ui.ctx().cumulative_pass_nr();
    view.pass = ui.ctx().cumulative_pass_nr();
    if fresh {
        view.loudest = view.loudest.max(panel.meters[METER_OUTPUT_PEAK]);
    }
    ui.horizontal_top(|ui| {
        transfer(ui, panel, height);
        waveform(ui, panel, &mut view, vec2(height * 2.3, height), fresh);
        meters(ui, panel, height);
        readouts(ui, panel, &mut view, vec2(150.0, height));
    });
    ui.data_mut(|d| d.insert_temp(id, view));
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
        panel.knobs(ui, &["input", "ceiling"]);
        let soft = panel.value("shape") >= 0.5;
        ui.add_enabled_ui(soft, |ui| panel.knob(ui, "softness"));
        panel.knob(ui, "output");
    });
}

/// `db` as a share of the displays' range, 0 at the bottom.
fn share(db: f32) -> f32 {
    ((db - LOW_DB) / (HIGH_DB - LOW_DB)).clamp(0.0, 1.0)
}

/// Level going in (before input gain) across, level coming out up, as the
/// settings shape it: the ceiling in red, where the knee starts, and a dot
/// where the sound peaks now.
fn transfer(ui: &mut Ui, panel: &Panel, size: f32) {
    let (response, painter) = display(ui, Vec2::splat(size), Sense::hover());
    let rect = response.rect.shrink(6.0);
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let to_x = |db: f32| rect.left() + share(db) * rect.width();
    let to_y = |db: f32| rect.bottom() - share(db) * rect.height();
    let grid = visuals.weak_text_color().gamma_multiply(0.18);
    for db in [-24.0, -12.0, 0.0] {
        painter.vline(to_x(db), rect.y_range(), Stroke::new(1.0, grid));
        painter.hline(rect.x_range(), to_y(db), Stroke::new(1.0, grid));
    }
    painter.line_segment(
        [
            pos2(to_x(LOW_DB), to_y(LOW_DB)),
            pos2(to_x(HIGH_DB), to_y(HIGH_DB)),
        ],
        Stroke::new(1.0, grid.gamma_multiply(2.0)),
    );
    let values = panel.values().to_vec();
    let (ceiling, knee) = clipper::knee(&values);
    let ceiling_db = dsp::gain_to_db(ceiling);
    painter.hline(
        rect.x_range(),
        to_y(ceiling_db),
        Stroke::new(1.0, CLIPPED.gamma_multiply(0.8)),
    );
    // Where the knee starts, in input terms.
    let gain_db = panel.value("input");
    if knee < ceiling && knee > 0.0 {
        painter.vline(
            to_x(dsp::gain_to_db(knee) - gain_db),
            rect.y_range(),
            Stroke::new(1.0, accent.gamma_multiply(0.35)),
        );
    }
    let out_db =
        |input_db: f32| dsp::gain_to_db(clipper::transfer(&values, dsp::db_to_gain(input_db)));
    let points: Vec<egui::Pos2> = (0..=160)
        .map(|i| {
            let db = LOW_DB + (HIGH_DB - LOW_DB) * i as f32 / 160.0;
            pos2(to_x(db), to_y(out_db(db)))
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, accent)));
    let level = panel.meters[METER_INPUT_PEAK] - gain_db;
    if level > LOW_DB {
        let at = pos2(to_x(level.min(HIGH_DB)), to_y(out_db(level.min(HIGH_DB))));
        painter.circle(
            at,
            4.5,
            visuals.strong_text_color(),
            Stroke::new(1.5, accent),
        );
    }
    let shape = ["Hard", "Smooth", "Tanh"][(panel.value("shape") as usize).min(2)];
    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        format!("{shape} · {ceiling_db:.1} dB"),
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );
}

/// The last few seconds, newest at the right: per column the driven input's
/// peak in red and the output's over it, on a dB scale mirrored about the
/// middle, so whatever red shows beyond the output was clipped off. The
/// ceiling is the pair of red lines.
fn waveform(ui: &mut Ui, panel: &Panel, view: &mut View, size: Vec2, fresh: bool) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(2.0, 6.0));
    let visuals = ui.visuals().clone();
    let values = panel.values().to_vec();
    let rate = panel.sample_rate;
    let columns = rect.width().max(1.0) as usize;
    let per_column = ((SECONDS * rate) / columns as f32).max(1.0) as usize;
    let latency = (effects::clipper::DESCRIPTOR.latency)(&values, rate);
    if let Some(tap) = panel.tap() {
        if view.shape != (columns, per_column, latency) {
            // A new scale or delay: start over rather than mix them.
            view.columns.clear();
            view.count = 0;
            view.waiting.clear();
            view.shape = (columns, per_column, latency);
            view.seen = tap.total().saturating_sub((columns * per_column) as u64);
        }
        if fresh {
            let gain = dsp::db_to_gain(panel.value("input"));
            let output = dsp::db_to_gain(panel.value("output"));
            let delta = panel.value("delta") >= 0.5;
            for frame in tap.since(view.seen) {
                let driven = [frame[0] * gain, frame[1] * gain];
                view.waiting.push_back(driven);
                if view.waiting.len() <= latency {
                    continue;
                }
                let input = view.waiting.pop_front().unwrap_or_default();
                // What came out of the clipper, before the output trim (in
                // Delta, what's left of the input after the delta).
                let out = |c: usize| {
                    let heard = frame[2 + c] / output;
                    if delta { input[c] - heard } else { heard }
                };
                let c = &mut view.current;
                c[0] = c[0].max(input[0].abs()).max(input[1].abs());
                c[1] = c[1].max(out(0).abs()).max(out(1).abs());
                view.count += 1;
                if view.count >= per_column {
                    view.columns.push_back(std::mem::take(&mut view.current));
                    view.count = 0;
                }
            }
            while view.columns.len() > columns {
                view.columns.pop_front();
            }
            view.seen = tap.total();
        }
    }
    let middle = rect.center().y;
    let half = rect.height() / 2.0;
    let reach = |level: f32| share(dsp::gain_to_db(level.max(1e-6))) * half;
    painter.hline(
        rect.x_range(),
        middle,
        Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.2)),
    );
    let accent = crate::theme::accent(&visuals);
    let mut mesh = egui::Mesh::default();
    for (i, [input, output]) in view.columns.iter().rev().enumerate() {
        let x = rect.right() - 1.0 - i as f32;
        let (input, output) = (reach(*input), reach(*output));
        if input > output + 0.5 {
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, middle - input), pos2(x + 1.0, middle + input)),
                CLIPPED,
            );
        }
        mesh.add_colored_rect(
            Rect::from_min_max(
                pos2(x, middle - output),
                pos2(x + 1.0, middle + output.max(0.5)),
            ),
            accent.gamma_multiply(0.85),
        );
    }
    painter.add(mesh);
    let ceiling = reach(clipper::knee(&values).0);
    for y in [middle - ceiling, middle + ceiling] {
        painter.hline(
            rect.x_range(),
            y,
            Stroke::new(1.0, CLIPPED.gamma_multiply(0.7)),
        );
    }
    for db in [0.0, -12.0, -24.0] {
        painter.text(
            pos2(rect.left() + 3.0, middle - share(db) * half),
            Align2::LEFT_CENTER,
            format!("{db:.0}"),
            FontId::proportional(crate::theme::text::TINY),
            visuals.weak_text_color().gamma_multiply(0.7),
        );
    }
    painter.text(
        response.rect.right_top()
            + vec2(
                -super::panel::DISPLAY_INSET.x,
                super::panel::DISPLAY_INSET.y,
            ),
        Align2::RIGHT_TOP,
        format!("In · Out · {SECONDS:.0} s"),
        FontId::proportional(crate::theme::text::TINY),
        visuals.weak_text_color(),
    );
}

/// Input peak, how much is clipped, and output true peak, side by side
/// with their numbers.
fn meters(ui: &mut Ui, panel: &Panel, height: f32) {
    let (response, painter) = display(ui, vec2(84.0, height), Sense::hover());
    let rect = response.rect.shrink2(vec2(5.0, 5.0));
    let visuals = ui.visuals().clone();
    let bars = Rect::from_min_max(rect.min, rect.max - vec2(0.0, 26.0));
    let width = (bars.width() - 8.0) / 3.0;
    let column = |i: usize| {
        Rect::from_min_size(
            pos2(bars.left() + i as f32 * (width + 4.0), bars.top()),
            vec2(width, bars.height()),
        )
    };
    let m = panel.meters;
    scopes::level_bars(
        &painter,
        column(0),
        &[m[METER_INPUT_PEAK]],
        -60.0,
        6.0,
        &visuals,
    );
    scopes::reduction_bar(&painter, column(1), m[METER_CLIP], CLIP_RANGE, &visuals);
    scopes::level_bars(
        &painter,
        column(2),
        &[m[METER_OUTPUT_PEAK]],
        -60.0,
        6.0,
        &visuals,
    );
    let ceiling = dsp::gain_to_db(clipper::knee(panel.values()).0);
    for i in [0, 2] {
        let r = column(i);
        let y = r.bottom() - ((ceiling + 60.0) / 66.0).clamp(0.0, 1.0) * r.height();
        painter.hline(r.x_range(), y, Stroke::new(1.0, CLIPPED));
    }
    let number = |db: f32| {
        if db <= -99.0 {
            "-inf".to_owned()
        } else {
            format!("{db:.1}")
        }
    };
    for (i, (name, text)) in [
        ("In", number(m[METER_INPUT_PEAK])),
        ("Clip", format!("{:.1}", -m[METER_CLIP])),
        ("Out", number(m[METER_OUTPUT_PEAK])),
    ]
    .iter()
    .enumerate()
    {
        let x = column(i).center().x;
        painter.text(
            pos2(x, rect.bottom() - 24.0),
            Align2::CENTER_TOP,
            *name,
            FontId::proportional(crate::theme::text::TINY),
            visuals.weak_text_color(),
        );
        painter.text(
            pos2(x, rect.bottom() - 12.0),
            Align2::CENTER_TOP,
            text,
            FontId::monospace(crate::theme::text::TINY),
            visuals.text_color(),
        );
    }
}

/// The numbers that matter, big: how much and how often it clips, the
/// loudest true peak out (red over the ceiling; click to clear), how much
/// louder the sound came out, and what the true-peak safety takes.
fn readouts(ui: &mut Ui, panel: &Panel, view: &mut View, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::click());
    let rect = response.rect.shrink2(vec2(8.0, 6.0));
    let visuals = ui.visuals().clone();
    let m = panel.meters;
    let ceiling = dsp::gain_to_db(clipper::knee(panel.values()).0);
    let true_peak = if view.loudest > -99.0 {
        format!("{:.2}", view.loudest)
    } else {
        "-inf".to_owned()
    };
    let over = view.loudest > ceiling + 0.01;
    let louder = if m[METER_INPUT_RMS] > -99.0 && m[METER_OUTPUT_RMS] > -99.0 {
        format!("{:+.1} dB", m[METER_OUTPUT_RMS] - m[METER_INPUT_RMS])
    } else {
        "–".to_owned()
    };
    let safety = panel.value("safety") >= 1.5;
    let rows: [(&str, String, Color32, f32); 5] = [
        (
            "Clipping",
            format!("{:.1} dB", m[METER_CLIP]),
            REDUCTION,
            18.0,
        ),
        (
            "True peak max",
            format!("{true_peak} dBTP"),
            if over {
                CLIPPED
            } else {
                visuals.strong_text_color()
            },
            18.0,
        ),
        (
            "Clipped",
            format!("{:.0} %", m[METER_CLIPPING]),
            visuals.text_color(),
            13.0,
        ),
        ("Louder by", louder, visuals.text_color(), 13.0),
        (
            "Safety",
            if safety {
                format!("{:.2} dB", -m[METER_SAFETY])
            } else {
                "off".to_owned()
            },
            visuals.text_color(),
            13.0,
        ),
    ];
    let total: f32 = rows.iter().map(|r| r.3 + 12.0).sum();
    let gap = ((rect.height() - total) / (rows.len() - 1) as f32).max(0.0);
    let mut y = rect.top();
    for (label, value, color, size) in rows {
        painter.text(
            pos2(rect.left(), y),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(crate::theme::text::TINY),
            visuals.weak_text_color(),
        );
        painter.text(
            pos2(rect.right(), y + 10.0),
            Align2::RIGHT_TOP,
            value,
            FontId::monospace(size),
            color,
        );
        y += size + 12.0 + gap;
    }
    if response.clicked() {
        view.loudest = f32::NEG_INFINITY;
    }
    response.on_hover_text("Click to clear the true peak max");
}
