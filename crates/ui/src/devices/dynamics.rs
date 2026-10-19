//! The dynamics panels: the compressor's (its transfer curve with where the
//! sound sits on it, a scrolling graph of level and gain reduction, and
//! in, reduction and out meters), the limiter's and the multiband's.

use eframe::egui::{
    self, Align2, Color32, FontId, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};

use super::Panel;
use super::panel::{KNOB_TEXT, display};
use crate::widgets::scopes::{self, History, REDUCTION};

/// The level range the compressor's displays show.
const LOW_DB: f32 = -60.0;
const HIGH_DB: f32 = 6.0;
/// How much reduction the graphs and meters have room for.
const REDUCTION_RANGE: f32 = 24.0;

/// Compressor meter slots.
const GAIN_REDUCTION: usize = 0;
const IN_PEAK: usize = 1;
const OUT_PEAK: usize = 2;
const IN_RMS: usize = 3;
const OUT_RMS: usize = 4;
const DETECTOR: usize = 5;

/// What a dynamics panel remembers between frames: the last few seconds
/// of input, output and reduction.
#[derive(Clone)]
struct Graphs {
    input: History,
    output: History,
    reduction: History,
    pass: u64,
}

impl Default for Graphs {
    fn default() -> Self {
        Graphs {
            input: History::new(4.0, 60.0),
            output: History::new(4.0, 60.0),
            reduction: History::new(4.0, 60.0),
            pass: u64::MAX,
        }
    }
}

/// Records this frame's levels, once per frame, and hands the graphs back.
fn graphs(ui: &Ui, panel: &Panel, levels: [f32; 3]) -> Graphs {
    let id = panel.id("graphs");
    let mut graphs: Graphs = ui.data_mut(|d| d.remove_temp(id)).unwrap_or_default();
    let pass = ui.ctx().cumulative_pass_nr();
    if graphs.pass != pass {
        graphs.pass = pass;
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        graphs.input.push(levels[0], dt);
        graphs.output.push(levels[1], dt);
        graphs.reduction.push(levels[2], dt);
    }
    ui.data_mut(|d| d.insert_temp(id, graphs.clone()));
    graphs
}

/// The height left for displays above a row of knobs and a row of switches.
fn display_height(ui: &Ui, panel: &Panel) -> f32 {
    (ui.available_height() - (panel.knob_size + KNOB_TEXT) - super::panel::CONTROL_ROW)
        .clamp(90.0, 180.0)
}

pub(super) fn compressor(ui: &mut Ui, panel: &mut Panel) {
    let height = display_height(ui, panel);
    ui.horizontal(|ui| {
        panel.choice(ui, "model");
        panel.toggle(ui, "auto_makeup", "Auto makeup");
    });
    let meters = panel.meters;
    let graphs = graphs(
        ui,
        panel,
        [meters[IN_PEAK], meters[OUT_PEAK], meters[GAIN_REDUCTION]],
    );
    ui.horizontal_top(|ui| {
        transfer_curve(ui, panel, height);
        level_graph(ui, panel, &graphs, vec2(height * 1.8, height));
        in_out_meters(ui, &meters, height);
    });
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
        panel.knobs(
            ui,
            &["threshold", "ratio", "knee", "attack", "release", "makeup"],
        );
        panel.knob_as(ui, "sc_hpf", Some("SC filter"));
        panel.knob(ui, "mix");
    });
}

/// Input level in, output level out, as the settings shape it, with a dot
/// where the sound is now. Drag sideways to move the threshold; the wheel
/// changes the ratio.
fn transfer_curve(ui: &mut Ui, panel: &mut Panel, size: f32) {
    let (response, painter) = display(ui, Vec2::splat(size), Sense::click_and_drag());
    let rect = response.rect.shrink(6.0);
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let to_x = |db: f32| rect.left() + (db - LOW_DB) / (HIGH_DB - LOW_DB) * rect.width();
    let to_y = |db: f32| {
        rect.bottom() - ((db - LOW_DB) / (HIGH_DB - LOW_DB)).clamp(0.0, 1.0) * rect.height()
    };
    let grid = visuals.weak_text_color().gamma_multiply(0.18);
    for db in [-48.0, -36.0, -24.0, -12.0, 0.0] {
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
    let threshold = panel.value("threshold");
    painter.vline(
        to_x(threshold),
        rect.y_range(),
        Stroke::new(1.0, accent.gamma_multiply(0.5)),
    );
    let points: Vec<egui::Pos2> = (0..=120)
        .map(|i| {
            let input = LOW_DB + (HIGH_DB - LOW_DB) * i as f32 / 120.0;
            pos2(
                to_x(input),
                to_y(effects::compressor::static_curve(&values, input)),
            )
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, accent)));
    let level = panel.meters[DETECTOR];
    if level > LOW_DB {
        let at = pos2(
            to_x(level.min(HIGH_DB)),
            to_y(effects::compressor::static_curve(&values, level)),
        );
        painter.circle(
            at,
            4.5,
            visuals.strong_text_color(),
            Stroke::new(1.5, accent),
        );
    }
    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        format!("{:.0} dB · {:.1}:1", threshold, panel.value("ratio")),
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );

    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let db = LOW_DB + (pos.x - rect.left()) / rect.width() * (HIGH_DB - LOW_DB);
        panel.set_id("threshold", db);
    }
    if response.drag_stopped() {
        panel.finish();
    }
    if response.hovered() {
        let notches = crate::widgets::knob::wheel_notches(ui);
        if notches != 0.0 {
            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            let ratio = panel.value("ratio") * (notches * 0.08).exp();
            panel.set_id("ratio", ratio);
            panel.finish();
        }
    }
    response.on_hover_text("Drag for the threshold, scroll for the ratio");
}

/// The last few seconds: input level filled in grey, output as a line,
/// gain reduction hanging from the top, and the threshold.
fn level_graph(ui: &mut Ui, panel: &Panel, graphs: &Graphs, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(2.0, 4.0));
    let visuals = ui.visuals().clone();
    let to_y = |db: f32| {
        let t = ((db - LOW_DB) / (0.0 - LOW_DB)).clamp(0.0, 1.0);
        rect.bottom() - t * rect.height()
    };
    let grid = visuals.weak_text_color().gamma_multiply(0.15);
    for db in [-48.0, -36.0, -24.0, -12.0] {
        painter.hline(rect.x_range(), to_y(db), Stroke::new(1.0, grid));
    }
    let threshold = panel.value("threshold");
    painter.hline(
        rect.x_range(),
        to_y(threshold),
        Stroke::new(1.0, crate::theme::accent(&visuals).gamma_multiply(0.5)),
    );
    graphs.input.paint(
        &painter,
        rect,
        to_y,
        rect.bottom(),
        Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.6)),
        visuals.weak_text_color().gamma_multiply(0.18),
    );
    graphs.output.paint(
        &painter,
        rect,
        to_y,
        rect.bottom(),
        Stroke::new(1.5, crate::theme::accent(&visuals)),
        Color32::TRANSPARENT,
    );
    let reduction_y =
        |db: f32| rect.top() + (db / REDUCTION_RANGE).clamp(0.0, 1.0) * rect.height() * 0.5;
    graphs.reduction.paint(
        &painter,
        rect,
        reduction_y,
        rect.top(),
        Stroke::new(1.5, REDUCTION),
        REDUCTION.gamma_multiply(0.2),
    );
    reduction_readout(&painter, rect, panel.meters[GAIN_REDUCTION], &visuals);
}

/// The gain reduction in numbers, top right, on a backing so the trace
/// hanging from the top doesn't run through it.
fn reduction_readout(painter: &egui::Painter, rect: Rect, db: f32, visuals: &egui::Visuals) {
    let galley = painter.layout_no_wrap(
        format!("-{:.1} dB", db.max(0.0)),
        FontId::monospace(crate::theme::text::SMALL),
        REDUCTION,
    );
    let at = rect.right_top() + vec2(-4.0 - galley.size().x, 2.0);
    let backing = Rect::from_min_size(at, galley.size()).expand2(vec2(3.0, 1.0));
    painter.rect_filled(
        backing,
        crate::theme::radius::SMALL,
        visuals.extreme_bg_color.gamma_multiply(0.85),
    );
    painter.galley(at, galley, REDUCTION);
}

pub(super) fn limiter(ui: &mut Ui, panel: &mut Panel) {
    use effects::limiter::{METER_INPUT_PEAK, METER_OUTPUT_PEAK, METER_REDUCTION};
    let height = display_height(ui, panel);
    ui.horizontal(|ui| {
        panel.toggle(ui, "auto_release", "Auto release");
        panel.toggle(ui, "true_peak", "True peak");
    });
    let meters = panel.meters;
    let graphs = graphs(
        ui,
        panel,
        [
            meters[METER_INPUT_PEAK],
            meters[METER_OUTPUT_PEAK],
            meters[METER_REDUCTION],
        ],
    );
    ui.horizontal_top(|ui| {
        // The level graph, with the ceiling where the threshold would be.
        let (response, painter) = display(ui, vec2(height * 2.4, height), Sense::hover());
        let rect = response.rect.shrink2(vec2(2.0, 4.0));
        let visuals = ui.visuals().clone();
        let to_y = |db: f32| {
            let t = ((db - LOW_DB) / (6.0 - LOW_DB)).clamp(0.0, 1.0);
            rect.bottom() - t * rect.height()
        };
        graphs.input.paint(
            &painter,
            rect,
            to_y,
            rect.bottom(),
            Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.6)),
            visuals.weak_text_color().gamma_multiply(0.18),
        );
        graphs.output.paint(
            &painter,
            rect,
            to_y,
            rect.bottom(),
            Stroke::new(1.5, crate::theme::accent(&visuals)),
            Color32::TRANSPARENT,
        );
        let ceiling = panel.value("ceiling");
        painter.hline(
            rect.x_range(),
            to_y(ceiling),
            Stroke::new(1.0, Color32::from_rgb(235, 90, 70)),
        );
        let reduction_y =
            |db: f32| rect.top() + (db / REDUCTION_RANGE).clamp(0.0, 1.0) * rect.height() * 0.5;
        graphs.reduction.paint(
            &painter,
            rect,
            reduction_y,
            rect.top(),
            Stroke::new(1.5, REDUCTION),
            REDUCTION.gamma_multiply(0.2),
        );
        reduction_readout(&painter, rect, meters[METER_REDUCTION], &visuals);
        // The compressor's meters, in its slots; a limiter has no RMS.
        let mut levels = [f32::MIN; 6];
        levels[GAIN_REDUCTION] = meters[METER_REDUCTION];
        levels[IN_PEAK] = meters[METER_INPUT_PEAK];
        levels[OUT_PEAK] = meters[METER_OUTPUT_PEAK];
        in_out_meters(ui, &levels, height);
    });
    panel.knobs(ui, &["input", "ceiling", "release"]);
}

pub(super) fn multiband(ui: &mut Ui, panel: &mut Panel) {
    use effects::multiband::{self as mb, BANDS};
    let selected_id = panel.id("band");
    let mut selected: usize = ui.data(|d| d.get_temp(selected_id)).unwrap_or(1);
    let height = display_height(ui, panel);
    bands_display(ui, panel, vec2(height * 3.4, height), &mut selected);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for (band, name) in ["Low", "Mid", "High"].iter().enumerate() {
                    if ui.selectable_label(selected == band, *name).clicked() {
                        selected = band;
                    }
                }
            });
            ui.horizontal(|ui| {
                let solo = panel.params[mb::param(selected, mb::SOLO)].id;
                let bypass = panel.params[mb::param(selected, mb::BYPASS)].id;
                panel.toggle(ui, solo, "Solo");
                panel.toggle(ui, bypass, "Bypass");
            });
        });
        ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
        for (which, label) in [
            (mb::THRESHOLD, "Down at"),
            (mb::RATIO, "Down ratio"),
            (mb::UP_THRESHOLD, "Up below"),
            (mb::UP_RATIO, "Up ratio"),
            (mb::ATTACK, "Attack"),
            (mb::RELEASE, "Release"),
            (mb::GAIN, "Gain"),
        ] {
            let id = panel.params[mb::param(selected.min(BANDS - 1), which)].id;
            panel.knob_as(ui, id, Some(label));
        }
        ui.separator();
        panel.knobs(ui, &["depth", "input", "output", "mix"]);
    });
    ui.data_mut(|d| d.insert_temp(selected_id, selected));
}

/// The three bands side by side over the frequencies they cover: each
/// one's level going in (grey) and coming out, where it's turned down
/// (above the upper line) and up (below the lower one), and by how much.
/// Drag the splits sideways and the lines up or down; click a band to
/// show its knobs.
fn bands_display(ui: &mut Ui, panel: &mut Panel, size: Vec2, selected: &mut usize) {
    use effects::multiband::{self as mb, BANDS, METER_GAIN_CHANGE, METER_INPUT, METER_OUTPUT};
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let rect = response.rect.shrink2(vec2(6.0, 14.0));
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let to_x = |hz: f32| rect.left() + (hz / 20.0).max(1e-6).ln() / 1000f32.ln() * rect.width();
    let hz_at = |x: f32| 20.0 * 1000f32.powf(((x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let to_y = |db: f32| rect.bottom() - ((db - LOW_DB) / -LOW_DB).clamp(0.0, 1.0) * rect.height();
    let db_at = |y: f32| LOW_DB + (rect.bottom() - y) / rect.height() * -LOW_DB;
    let splits = [
        panel.values()[mb::LOW_SPLIT],
        panel.values()[mb::HIGH_SPLIT],
    ];
    let edges = [rect.left(), to_x(splits[0]), to_x(splits[1]), rect.right()];
    let meters = panel.meters;
    for band in 0..BANDS {
        let area = Rect::from_x_y_ranges(edges[band] + 2.0..=edges[band + 1] - 2.0, rect.y_range());
        if band == *selected {
            painter.rect_filled(
                area,
                crate::theme::radius::SMALL,
                accent.gamma_multiply(0.06),
            );
        }
        let input = meters[mb::meter(band, METER_INPUT)];
        let output = meters[mb::meter(band, METER_OUTPUT)];
        let width = area.width() * 0.22;
        let bar = |x: f32, db: f32, color: Color32| {
            painter.rect_filled(
                Rect::from_x_y_ranges(x - width / 2.0..=x + width / 2.0, to_y(db)..=rect.bottom()),
                1.0,
                color,
            );
        };
        bar(
            area.center().x - width * 0.6,
            input,
            visuals.weak_text_color().gamma_multiply(0.4),
        );
        bar(
            area.center().x + width * 0.6,
            output,
            accent.gamma_multiply(0.75),
        );
        let down = panel.values()[mb::param(band, mb::THRESHOLD)];
        let up = panel.values()[mb::param(band, mb::UP_THRESHOLD)];
        painter.hline(area.x_range(), to_y(down), Stroke::new(1.5, REDUCTION));
        painter.hline(
            area.x_range(),
            to_y(up),
            Stroke::new(1.5, Color32::from_rgb(110, 200, 140)),
        );
        let change = meters[mb::meter(band, METER_GAIN_CHANGE)];
        painter.text(
            pos2(area.center().x, response.rect.top() + 2.0),
            Align2::CENTER_TOP,
            format!("{change:+.1} dB"),
            FontId::monospace(crate::theme::text::SMALL),
            if change > 0.05 {
                Color32::from_rgb(110, 200, 140)
            } else if change < -0.05 {
                REDUCTION
            } else {
                visuals.weak_text_color()
            },
        );
    }
    for split in &edges[1..3] {
        painter.vline(
            *split,
            rect.y_range(),
            Stroke::new(1.5, visuals.strong_text_color()),
        );
    }

    // What a drag moves: a split, or a band's upper or lower line.
    #[derive(Clone, Copy)]
    enum Grip {
        Split(usize),
        Line(usize),
    }
    let grip_id = response.id.with("grip");
    if let Some(pos) = response.interact_pointer_pos() {
        let band = (0..BANDS)
            .find(|b| pos.x < edges[b + 1])
            .unwrap_or(BANDS - 1);
        if response.clicked() || response.drag_started() {
            *selected = band;
        }
        if response.drag_started() {
            let grip = if (pos.x - edges[1]).abs() < 6.0 {
                Some(Grip::Split(mb::LOW_SPLIT))
            } else if (pos.x - edges[2]).abs() < 6.0 {
                Some(Grip::Split(mb::HIGH_SPLIT))
            } else {
                let down = mb::param(band, mb::THRESHOLD);
                let up = mb::param(band, mb::UP_THRESHOLD);
                let nearest = if (pos.y - to_y(panel.values()[down])).abs()
                    <= (pos.y - to_y(panel.values()[up])).abs()
                {
                    down
                } else {
                    up
                };
                Some(Grip::Line(nearest))
            };
            ui.data_mut(|d| d.insert_temp(grip_id, grip));
        }
        if response.dragged() {
            match ui.data(|d| d.get_temp::<Option<Grip>>(grip_id)).flatten() {
                Some(Grip::Split(index)) => panel.set(index, hz_at(pos.x)),
                Some(Grip::Line(index)) => panel.set(index, db_at(pos.y)),
                None => {}
            }
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag a split sideways, or a band's lines up or down");
}

/// Ducker meter slots.
const DUCK_REDUCTION: usize = 0;
const DUCK_SIDECHAIN: usize = 1;
const DUCK_SINCE: usize = 2;

pub(super) fn ducker(ui: &mut Ui, panel: &mut Panel) {
    let height = display_height(ui, panel);
    let mode = panel.value("mode") as usize;
    ui.horizontal(|ui| {
        panel.choice(ui, "mode");
        if mode == 2 {
            panel.choice_menu(ui, "rate");
        }
    });
    let meters = panel.meters;
    let graphs = graphs(
        ui,
        panel,
        [meters[DUCK_SIDECHAIN], 0.0, meters[DUCK_REDUCTION]],
    );
    ui.horizontal_top(|ui| {
        duck_shape(ui, panel, vec2(height * 1.6, height));
        sidechain_graph(ui, panel, &graphs, vec2(height * 1.4, height), mode != 2);
    });
    let knobs: &[&str] = match mode {
        0 => &[
            "threshold",
            "depth",
            "attack",
            "hold",
            "release",
            "curve",
            "lookahead",
            "sc_lowpass",
            "mix",
        ],
        1 => &[
            "threshold",
            "depth",
            "attack",
            "release",
            "lookahead",
            "sc_lowpass",
            "mix",
        ],
        _ => &["depth", "attack", "hold", "release", "curve", "mix"],
    };
    panel.knobs(ui, knobs);
}

/// The duck after a hit, gain over time, with a playhead where it is now.
/// Drag up or down for the depth, sideways for the release.
fn duck_shape(ui: &mut Ui, panel: &mut Panel, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    // Room above the curve for the caption.
    let rect = Rect::from_min_max(
        response.rect.min + vec2(6.0, 18.0),
        response.rect.max - vec2(6.0, 8.0),
    );
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let values = panel.values().to_vec();
    let length = effects::ducker::duck_length(&values);
    let span = length * 1.15;
    let to_x = |ms: f32| rect.left() + ms / span * rect.width();
    let to_y = |gain: f32| rect.bottom() - gain.clamp(0.0, 1.0) * rect.height();
    let grid = visuals.weak_text_color().gamma_multiply(0.18);
    painter.hline(rect.x_range(), to_y(1.0), Stroke::new(1.0, grid));
    painter.hline(rect.x_range(), to_y(0.5), Stroke::new(1.0, grid));
    let points: Vec<egui::Pos2> = (0..=160)
        .map(|i| {
            let ms = span * i as f32 / 160.0;
            pos2(to_x(ms), to_y(effects::ducker::duck_gain(&values, ms)))
        })
        .collect();
    let mut fill = egui::Mesh::default();
    for pair in points.windows(2) {
        fill.add_colored_rect(
            Rect::from_min_max(pair[0], pos2(pair[1].x, rect.bottom())),
            accent.gamma_multiply(0.16),
        );
    }
    painter.add(fill);
    painter.add(Shape::line(points, Stroke::new(2.0, accent)));
    let since = panel.meters[DUCK_SINCE];
    if since < length {
        let x = to_x(since);
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(1.0, visuals.strong_text_color()),
        );
        painter.circle_filled(
            pos2(x, to_y(effects::ducker::duck_gain(&values, since))),
            3.5,
            visuals.strong_text_color(),
        );
    }
    painter.text(
        response.rect.right_top()
            + vec2(
                -super::panel::DISPLAY_INSET.x,
                super::panel::DISPLAY_INSET.y,
            ),
        Align2::RIGHT_TOP,
        format!("-{:.0} dB · {:.0} ms", panel.value("depth"), length),
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );
    if response.dragged() {
        let delta = response.drag_delta();
        if delta.y != 0.0 {
            let depth = panel.value("depth") + delta.y * 0.25;
            panel.set_id("depth", depth);
        }
        if delta.x != 0.0 {
            let release = panel.value("release") * (delta.x / 150.0).exp();
            panel.set_id("release", release);
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag up or down for the depth, sideways for the release");
}

/// The sidechain's level over the last seconds, with the threshold, and
/// the gain reduction hanging from the top.
fn sidechain_graph(ui: &mut Ui, panel: &Panel, graphs: &Graphs, size: Vec2, threshold: bool) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(2.0, 4.0));
    let visuals = ui.visuals().clone();
    let to_y = |db: f32| {
        let t = ((db - LOW_DB) / (0.0 - LOW_DB)).clamp(0.0, 1.0);
        rect.bottom() - t * rect.height()
    };
    graphs.input.paint(
        &painter,
        rect,
        to_y,
        rect.bottom(),
        Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.7)),
        visuals.weak_text_color().gamma_multiply(0.2),
    );
    if threshold {
        painter.hline(
            rect.x_range(),
            to_y(panel.value("threshold")),
            Stroke::new(1.0, crate::theme::accent(&visuals)),
        );
    }
    let reduction_y = |db: f32| rect.top() + (db / 48.0).clamp(0.0, 1.0) * rect.height();
    graphs.reduction.paint(
        &painter,
        rect,
        reduction_y,
        rect.top(),
        Stroke::new(1.5, REDUCTION),
        REDUCTION.gamma_multiply(0.2),
    );
    painter.text(
        rect.left_top() + vec2(4.0, 2.0),
        Align2::LEFT_TOP,
        if panel.session_has_sidechain() || !threshold {
            "Sidechain"
        } else {
            "No sidechain"
        },
        FontId::proportional(crate::theme::text::TINY),
        visuals.weak_text_color(),
    );
}

/// Input, gain reduction and output side by side, peak with RMS inside.
fn in_out_meters(ui: &mut Ui, meters: &[f32], height: f32) {
    let (response, painter) = display(ui, vec2(78.0, height), Sense::hover());
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
    let level = |rect: Rect, peak: f32, rms: f32| {
        scopes::level_bars(&painter, rect, &[peak], LOW_DB, 0.0, &visuals);
        let t = ((rms - LOW_DB) / -LOW_DB).clamp(0.0, 1.0);
        if t > 0.0 {
            let y = rect.bottom() - t * rect.height();
            painter.hline(
                rect.x_range(),
                y,
                Stroke::new(2.0, visuals.strong_text_color()),
            );
        }
    };
    level(column(0), meters[IN_PEAK], meters[IN_RMS]);
    scopes::reduction_bar(
        &painter,
        column(1),
        meters[GAIN_REDUCTION],
        REDUCTION_RANGE,
        &visuals,
    );
    level(column(2), meters[OUT_PEAK], meters[OUT_RMS]);
    for (i, (name, value)) in [
        ("In", meters[IN_PEAK]),
        ("GR", -meters[GAIN_REDUCTION].max(0.0)),
        ("Out", meters[OUT_PEAK]),
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
        let text = if *value <= -99.0 {
            "-inf".to_owned()
        } else {
            format!("{value:.0}")
        };
        painter.text(
            pos2(x, rect.bottom() - 12.0),
            Align2::CENTER_TOP,
            text,
            FontId::monospace(crate::theme::text::TINY),
            visuals.text_color(),
        );
    }
}
