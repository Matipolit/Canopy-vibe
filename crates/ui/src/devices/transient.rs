//! The transient shaper's panel: what the settings do to one hit (its
//! level before and after over its first moments, and the gain change
//! under it), worked out by the effect's own detector; beside it the last
//! few seconds as they happened, the input and output levels with the
//! boosts and cuts made; the split and clip switches above, the knobs
//! below.

use effects::transient::{
    self, METER_BOOST, METER_CUT, METER_INPUT_PEAK, METER_OUTPUT_PEAK, RANGE_DB,
};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use eframe::egui::{pos2, vec2};

use super::Panel;
use super::panel::{KNOB_TEXT, display};
use crate::widgets::scopes::{History, REDUCTION};

/// How much of a hit the hit display shows.
const SPAN_MS: f32 = 300.0;
/// Dragging this close to the onset sets the attack; further along, the
/// sustain.
const ONSET_MS: f32 = 40.0;
/// The level range the displays show.
const TOP_DB: f32 = 24.0;
const FLOOR_DB: f32 = -60.0;
/// The color boosts are drawn in; cuts are drawn as gain reduction is.
const BOOST: Color32 = Color32::from_rgb(110, 200, 140);

/// The height left for displays above a row of knobs and a row of switches.
fn display_height(ui: &Ui, panel: &Panel) -> f32 {
    (ui.available_height() - (panel.knob_size + KNOB_TEXT) - super::panel::CONTROL_ROW)
        .clamp(90.0, 180.0)
}

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let height = display_height(ui, panel);
    let split = panel.value("split") >= 0.5;
    ui.horizontal(|ui| {
        panel.toggle(ui, "split", "Split");
        panel.toggle(ui, "clip", "Clip");
    });
    let graphs = graphs(ui, panel);
    ui.horizontal_top(|ui| {
        hit_display(ui, panel, vec2(height * 1.7, height));
        live_display(ui, panel, &graphs, vec2(height * 1.7, height));
    });
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
        panel.knobs(ui, &["attack", "sustain", "pump", "speed"]);
        ui.add_enabled_ui(split, |ui| {
            panel.knob_as(ui, "split_freq", Some("Split at"))
        });
        panel.knobs(ui, &["output", "mix"]);
    });
}

/// One hit as the hit display draws it, a point per column: its level
/// going in and coming out, and the gain change between them, in dB.
struct HitCurves {
    dry: Vec<f32>,
    shaped: Vec<f32>,
    change: Vec<f32>,
}

impl HitCurves {
    fn new(values: &[f32], columns: usize) -> HitCurves {
        let points = columns.max(1) + 1;
        let mut change = vec![0.0; points];
        transient::hit_response(values, SPAN_MS, &mut change);
        let dry: Vec<f32> = (0..points)
            .map(|i| {
                let ms = SPAN_MS * i as f32 / (points - 1) as f32;
                dsp::gain_to_db(transient::hit_level(ms))
            })
            .collect();
        let shaped = dry.iter().zip(&change).map(|(d, c)| d + c).collect();
        HitCurves {
            dry,
            shaped,
            change,
        }
    }
}

/// Fills between each point of `points` and `base_y`, one quad a column.
fn fill(painter: &egui::Painter, points: &[Pos2], base_y: f32, color: Color32) {
    let mut mesh = egui::Mesh::default();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        mesh.add_colored_rect(
            Rect::from_min_max(
                pos2(a.x, a.y.min(base_y)),
                pos2(b.x.max(a.x + 0.5), a.y.max(base_y)),
            ),
            color,
        );
    }
    painter.add(mesh);
}

/// Where a gain change of `db` goes in a strip `rect`: boosts up from the
/// middle, cuts down, [`RANGE_DB`] at the edges.
fn change_y(rect: Rect) -> impl Fn(f32) -> f32 {
    move |db| rect.center().y - db.clamp(-RANGE_DB, RANGE_DB) / RANGE_DB * rect.height() / 2.0
}

/// The strip's middle line, for no change: drawn over the traces, so that
/// where there is none they don't show.
fn change_axis(painter: &egui::Painter, rect: Rect, weak: Color32) {
    painter.hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, weak.gamma_multiply(0.4)),
    );
}

/// `db` as a signed gain change, `-` for cuts.
fn signed_db(db: f32) -> String {
    if db.abs() < 0.05 {
        "0.0 dB".to_owned()
    } else {
        format!("{db:+.1} dB")
    }
}

/// One hit, out of silence and dying away 60 dB over 300 ms: its level
/// going in (grey) and coming out (bright), and under it the gain change,
/// boosts up and cuts down. Drag up or down near the onset for the attack,
/// along the tail for the sustain.
fn hit_display(ui: &mut Ui, panel: &mut Panel, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let full = response.rect.shrink2(vec2(6.0, 6.0));
    let strip = (full.height() * 0.3).clamp(18.0, 44.0);
    let levels = Rect::from_min_max(
        full.min + vec2(0.0, 12.0),
        pos2(full.right(), full.bottom() - strip - 4.0),
    );
    let changes = Rect::from_min_max(pos2(full.left(), full.bottom() - strip), full.max);
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();

    let columns = levels.width().max(2.0) as usize;
    let curves = HitCurves::new(panel.values(), columns);
    let x_of = |i: usize| levels.left() + levels.width() * i as f32 / columns as f32;
    let x_at_ms = |ms: f32| levels.left() + levels.width() * ms / SPAN_MS;
    let y_of = |db: f32| {
        levels.top() + (TOP_DB - db.clamp(FLOOR_DB, TOP_DB)) / (TOP_DB - FLOOR_DB) * levels.height()
    };

    // Grid: 0 dB, and every 100 ms; the onset's stretch shaded.
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.2));
    painter.rect_filled(
        Rect::from_x_y_ranges(levels.left()..=x_at_ms(ONSET_MS), full.y_range()),
        0.0,
        accent.gamma_multiply(0.05),
    );
    painter.hline(levels.x_range(), y_of(0.0), grid);
    for ms in [100.0, 200.0] {
        let x = x_at_ms(ms);
        painter.vline(x, full.y_range(), grid);
        painter.text(
            pos2(x + 2.0, levels.bottom() - 1.0),
            Align2::LEFT_BOTTOM,
            format!("{ms:.0} ms"),
            FontId::proportional(crate::theme::text::TINY),
            weak.gamma_multiply(0.8),
        );
    }

    let dry: Vec<Pos2> = curves
        .dry
        .iter()
        .enumerate()
        .map(|(i, &db)| pos2(x_of(i), y_of(db)))
        .collect();
    fill(&painter, &dry, levels.bottom(), weak.gamma_multiply(0.18));
    painter.add(Shape::line(dry, Stroke::new(1.0, weak.gamma_multiply(0.6))));
    let shaped: Vec<Pos2> = curves
        .shaped
        .iter()
        .enumerate()
        .map(|(i, &db)| pos2(x_of(i), y_of(db)))
        .collect();
    painter.add(Shape::line(shaped, Stroke::new(1.8, accent)));
    let y_of_change = change_y(changes);
    for (color, side) in [(BOOST, 1.0f32), (REDUCTION, -1.0)] {
        // The boosts, then the cuts, each from the middle in its color.
        let points: Vec<Pos2> = curves
            .change
            .iter()
            .enumerate()
            .map(|(i, &db)| pos2(x_of(i), y_of_change(side * (side * db).max(0.0))))
            .collect();
        fill(
            &painter,
            &points,
            changes.center().y,
            color.gamma_multiply(0.35),
        );
        painter.add(Shape::line(points, Stroke::new(1.2, color)));
    }
    change_axis(&painter, changes, weak);

    // The change at the onset and late in the tail, in numbers.
    let onset = curves.change.first().copied().unwrap_or(0.0);
    let tail = curves.change.last().copied().unwrap_or(0.0);
    painter.text(
        pos2(full.left(), full.top() - 2.0),
        Align2::LEFT_TOP,
        format!("Onset {}", signed_db(onset)),
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );
    painter.text(
        pos2(full.right(), full.top() - 2.0),
        Align2::RIGHT_TOP,
        format!("Tail {}", signed_db(tail)),
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );

    if response.dragged() {
        let delta = response.drag_delta();
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos());
        if delta.y != 0.0
            && let Some(origin) = origin
        {
            let id = if origin.x < x_at_ms(ONSET_MS) {
                "attack"
            } else {
                "sustain"
            };
            panel.set_id(id, panel.value(id) - delta.y * 0.8);
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text(
        "Drag up or down: near the onset for the attack, along the tail for the sustain",
    );
}

/// What the live display remembers between frames: the last few seconds
/// of input and output level and of the boosts and cuts.
#[derive(Clone)]
struct Graphs {
    input: History,
    output: History,
    boost: History,
    cut: History,
    pass: u64,
}

impl Default for Graphs {
    fn default() -> Self {
        Graphs {
            input: History::new(4.0, 60.0),
            output: History::new(4.0, 60.0),
            boost: History::new(4.0, 60.0),
            cut: History::new(4.0, 60.0),
            pass: u64::MAX,
        }
    }
}

/// Records this frame's meters, once per frame, and hands the graphs back.
fn graphs(ui: &Ui, panel: &Panel) -> Graphs {
    let id = panel.id("graphs");
    let mut graphs: Graphs = ui.data_mut(|d| d.remove_temp(id)).unwrap_or_default();
    let pass = ui.ctx().cumulative_pass_nr();
    if graphs.pass != pass {
        graphs.pass = pass;
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let m = panel.meters;
        graphs.input.push(m[METER_INPUT_PEAK], dt);
        graphs.output.push(m[METER_OUTPUT_PEAK], dt);
        graphs.boost.push(m[METER_BOOST], dt);
        graphs.cut.push(m[METER_CUT], dt);
    }
    ui.data_mut(|d| d.insert_temp(id, graphs.clone()));
    graphs
}

/// The last few seconds: the input level filled in grey, the output as a
/// line, and under them the boosts and cuts made, with the latest in
/// numbers.
fn live_display(ui: &mut Ui, panel: &Panel, graphs: &Graphs, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let full = response.rect.shrink2(vec2(2.0, 6.0));
    let strip = (full.height() * 0.3).clamp(18.0, 44.0);
    let levels = Rect::from_min_max(
        full.min + vec2(0.0, 12.0),
        pos2(full.right(), full.bottom() - strip - 4.0),
    );
    let changes = Rect::from_min_max(pos2(full.left(), full.bottom() - strip), full.max);
    let visuals = ui.visuals().clone();
    let weak = visuals.weak_text_color();
    let y_of = |db: f32| {
        let t = ((db - FLOOR_DB) / (6.0 - FLOOR_DB)).clamp(0.0, 1.0);
        levels.bottom() - t * levels.height()
    };
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.15));
    for db in [-48.0, -24.0, 0.0] {
        painter.hline(levels.x_range(), y_of(db), grid);
    }
    graphs.input.paint(
        &painter,
        levels,
        y_of,
        levels.bottom(),
        Stroke::new(1.0, weak.gamma_multiply(0.6)),
        weak.gamma_multiply(0.18),
    );
    graphs.output.paint(
        &painter,
        levels,
        y_of,
        levels.bottom(),
        Stroke::new(1.5, crate::theme::accent(&visuals)),
        Color32::TRANSPARENT,
    );
    for (history, color) in [(&graphs.boost, BOOST), (&graphs.cut, REDUCTION)] {
        history.paint(
            &painter,
            changes,
            change_y(changes),
            changes.center().y,
            Stroke::new(1.2, color),
            color.gamma_multiply(0.35),
        );
    }
    change_axis(&painter, changes, weak);

    let m = panel.meters;
    let latest = if m[METER_BOOST] >= -m[METER_CUT] {
        m[METER_BOOST]
    } else {
        m[METER_CUT]
    };
    let color = if latest > 0.05 {
        BOOST
    } else if latest < -0.05 {
        REDUCTION
    } else {
        weak
    };
    painter.text(
        pos2(full.left() + 4.0, full.top() - 2.0),
        Align2::LEFT_TOP,
        "In · Out",
        FontId::proportional(crate::theme::text::TINY),
        weak,
    );
    painter.text(
        pos2(full.right() - 4.0, full.top() - 2.0),
        Align2::RIGHT_TOP,
        signed_db(latest),
        FontId::monospace(crate::theme::text::SMALL),
        color,
    );
    response.on_hover_text("The last four seconds: input and output peaks, and the gain change");
}

#[cfg(test)]
mod tests {
    use super::*;
    use effects::transient::{ATTACK, MIX, PUMP, SUSTAIN};

    fn values(settings: &[(usize, f32)]) -> Vec<f32> {
        let mut values: Vec<f32> = transient::DESCRIPTOR
            .params
            .iter()
            .map(|p| p.default_value())
            .collect();
        for &(index, value) in settings {
            values[index] = value;
        }
        values
    }

    #[test]
    fn the_hit_curves_show_what_the_knobs_do() {
        let columns = 300;
        // Untouched: out as in.
        let flat = HitCurves::new(&values(&[]), columns);
        assert_eq!(flat.dry, flat.shaped);
        assert!(flat.change.iter().all(|&c| c == 0.0));
        assert_eq!(flat.dry.len(), columns + 1);
        assert_eq!(flat.dry[0], 0.0);
        assert!((flat.dry[columns] + 60.0).abs() < 0.01);

        // Attack lifts the onset (each column is a millisecond) and leaves
        // the tail; sustain the other way round.
        let attack = HitCurves::new(&values(&[(ATTACK, 100.0)]), columns);
        assert!(attack.change[0] > 20.0 && attack.change[200].abs() < 0.01);
        let sustain = HitCurves::new(&values(&[(SUSTAIN, -100.0)]), columns);
        assert!(sustain.change[0].abs() < 0.01 && sustain.change[200] < -12.0);
        assert!(sustain.shaped[200] < sustain.dry[200] - 12.0);
        // The pump dips just after the onset; with no mix, nothing.
        let pump = HitCurves::new(&values(&[(PUMP, 100.0)]), columns);
        assert!(pump.change[0].abs() < 0.01 && pump.change[30] < -4.0);
        let dry = HitCurves::new(&values(&[(PUMP, 100.0), (MIX, 0.0)]), columns);
        assert!(dry.change.iter().all(|&c| c == 0.0));

        assert_eq!(signed_db(0.01), "0.0 dB");
        assert_eq!(signed_db(-3.04), "-3.0 dB");
        assert_eq!(signed_db(12.0), "+12.0 dB");
    }
}
