//! The reverb's and delay's panels: each draws what it does to a single
//! hit (the reverb's decay, the delay's echoes), editable by dragging, next
//! to its knobs.

use effects::delay::NOTES;
use eframe::egui::{self, Align2, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

use super::Panel;
use super::panel::display;
use crate::widgets::knob::format_value;

/// How far the reverb display reaches down.
const FLOOR_DB: f32 = -60.0;

pub(super) fn reverb(ui: &mut Ui, panel: &mut Panel) {
    let height = 2.0 * panel.fit_knobs(ui, 2);
    ui.horizontal_top(|ui| {
        decay_display(ui, panel, vec2(height * 1.6, height));
        ui.vertical(|ui| {
            panel.knobs(ui, &["size", "decay", "predelay", "mix"]);
            panel.knobs(ui, &["damping", "low_cut", "width"]);
        });
    });
}

/// The reverb's response to a hit: the dry sound, early reflections spread
/// by the size, then the tail falling 60 dB over the decay time, its highs
/// sooner as the damping says. Drag sideways to change the decay; drag the
/// pre-delay marker to move it.
fn decay_display(ui: &mut Ui, panel: &mut Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let rect = response.rect.shrink2(vec2(8.0, 10.0));
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();
    let predelay = panel.value("predelay") / 1000.0;
    let decay = panel.value("decay");
    let room = panel.value("size") / 100.0;
    let damping = panel.value("damping");
    let span = (predelay + decay * 1.15).max(0.3);
    let x = |t: f32| rect.left() + t / span * rect.width();
    let y = |db: f32| rect.top() + (db / FLOOR_DB).clamp(0.0, 1.0) * rect.height();

    for db in [-20.0, -40.0] {
        painter.hline(
            rect.x_range(),
            y(db),
            Stroke::new(1.0, weak.gamma_multiply(0.2)),
        );
        painter.text(
            pos2(rect.right(), y(db)),
            Align2::RIGHT_BOTTOM,
            format!("{db}"),
            FontId::proportional(crate::theme::text::TINY),
            weak.gamma_multiply(0.7),
        );
    }

    // The dry hit, then early reflections.
    painter.vline(x(0.0), rect.y_range(), Stroke::new(2.0, weak));
    let spread = 0.01 + 0.07 * room;
    for i in 0..8u32 {
        let t = predelay + spread * ((i * 37 % 8) as f32 + 0.5) / 8.0;
        let db = -6.0 - 2.5 * i as f32;
        painter.line_segment(
            [pos2(x(t), y(FLOOR_DB)), pos2(x(t), y(db))],
            Stroke::new(1.5, accent.gamma_multiply(0.7)),
        );
    }

    // The tail: lows fall over the decay, highs sooner.
    let start = predelay + spread * 0.5;
    let highs = decay * (damping / 20_000.0).powf(0.6).clamp(0.05, 1.0);
    let tail = |rt: f32| -> Vec<egui::Pos2> {
        (0..=60)
            .map(|i| {
                let t = start + rt * i as f32 / 60.0;
                pos2(x(t), y(-10.0 + (FLOOR_DB + 10.0) * i as f32 / 60.0))
            })
            .collect()
    };
    let lows = tail(decay);
    let mut fill = egui::Mesh::default();
    for pair in lows.windows(2) {
        fill.add_colored_rect(
            Rect::from_min_max(pair[0], pos2(pair[1].x, y(FLOOR_DB))),
            accent.gamma_multiply(0.18),
        );
    }
    painter.add(fill);
    painter.add(egui::Shape::line(lows, Stroke::new(1.8, accent)));
    painter.add(egui::Shape::line(
        tail(highs),
        Stroke::new(1.2, visuals.strong_text_color().gamma_multiply(0.6)),
    ));

    // The pre-delay marker.
    let marker = pos2(x(predelay), rect.top());
    painter.circle_filled(marker, 4.0, accent);
    let label = format!(
        "{} decay · highs {:.2} s",
        format_value(&panel.params[panel.index("decay").unwrap_or(0)], decay),
        highs
    );
    painter.text(
        pos2(rect.right(), rect.top() - 6.0),
        Align2::RIGHT_TOP,
        label,
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );

    let target = response.id.with("target");
    if response.drag_started()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let on_marker = (pos.x - marker.x).abs() < 10.0;
        ui.data_mut(|d| d.insert_temp(target, on_marker));
    }
    if response.dragged() {
        let on_marker = ui.data(|d| d.get_temp::<bool>(target)).unwrap_or(false);
        if on_marker {
            if let Some(pos) = response.interact_pointer_pos() {
                let t = ((pos.x - rect.left()) / rect.width() * span).max(0.0);
                panel.set_id("predelay", t * 1000.0);
            }
        } else {
            let delta = response.drag_delta();
            panel.set_id("decay", decay * ((delta.x - delta.y) / 120.0).exp());
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag to change the decay, or the dot to move the pre-delay");
}

pub(super) fn delay(ui: &mut Ui, panel: &mut Panel) {
    // Room for the mode switches above the knobs.
    let switches = crate::theme::size::CONTROL + crate::theme::space::M;
    let height = 2.0 * panel.fit_knobs_beside(ui, 2, switches) + switches;
    ui.horizontal_top(|ui| {
        echo_display(ui, panel, vec2(height * 1.7, height));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                panel.choice(ui, "sync");
                panel.choice(ui, "mode");
            });
            let synced = panel.value("sync") < 0.5;
            panel.knobs(
                ui,
                &[if synced { "note" } else { "time" }, "feedback", "mix"],
            );
            panel.knobs(ui, &["low_cut", "high_cut"]);
        });
    });
}

/// Note values, shortest first, as indices into the delay's list.
fn notes_by_length() -> Vec<usize> {
    let mut order: Vec<usize> = (0..NOTES.len()).collect();
    order.sort_by(|a, b| NOTES[*a].1.total_cmp(&NOTES[*b].1));
    order
}

/// The echoes of a single hit: each repeat's level and side (left above,
/// right below), and the band the feedback keeps. Drag sideways for the
/// time and up or down for the feedback.
fn echo_display(ui: &mut Ui, panel: &mut Panel, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let full = response.rect.shrink2(vec2(8.0, 8.0));
    let strip = 8.0;
    let rect = Rect::from_min_max(
        full.min + vec2(0.0, 12.0),
        full.max - vec2(0.0, strip + 4.0),
    );
    let visuals = ui.visuals().clone();
    let accent = crate::theme::accent(&visuals);
    let weak = visuals.weak_text_color();

    let synced = panel.value("sync") < 0.5;
    let note = (panel.value("note") as usize).min(NOTES.len() - 1);
    let bpm = panel.session.project.tempo.tempo.to_bpm() as f32;
    let seconds = if synced {
        NOTES[note].1 as f32 * 60.0 / bpm.max(1.0)
    } else {
        panel.value("time") / 1000.0
    };
    let feedback = panel.value("feedback") / 100.0;
    let mix = panel.value("mix") / 100.0;
    let ping_pong = panel.value("mode") >= 0.5;
    // Echoes until they're 48 dB down, on a dB scale so the tail shows.
    let height_of = |gain: f32| ((20.0 * gain.max(1e-6).log10() + 48.0) / 48.0).clamp(0.0, 1.0);
    let echoes = (1..=32)
        .take_while(|k| height_of(feedback.powi(k - 1)) > 0.0)
        .count()
        .max(2);
    let span = seconds * (echoes as f32 + 0.6);
    let x = |t: f32| rect.left() + t / span * rect.width();
    let center = rect.center().y;
    let half = rect.height() / 2.0;
    painter.hline(
        rect.x_range(),
        center,
        Stroke::new(1.0, weak.gamma_multiply(0.25)),
    );
    painter.vline(
        x(0.0),
        (center - half)..=(center + half),
        Stroke::new(3.0, weak.gamma_multiply(1.0 - mix * 0.7)),
    );
    for k in 1..=echoes {
        let level = height_of(feedback.powi(k as i32 - 1)) * 0.95;
        let at = x(seconds * k as f32);
        let (up, down) = match (ping_pong, k % 2 == 1) {
            (false, _) => (level, level),
            (true, true) => (level, 0.0),
            (true, false) => (0.0, level),
        };
        // Brighter the more of the echoes the mix lets through.
        let stroke = Stroke::new(3.0, accent.gamma_multiply(0.35 + 0.65 * mix.sqrt()));
        painter.line_segment(
            [pos2(at, center - up * half), pos2(at, center + down * half)],
            stroke,
        );
    }
    painter.text(
        pos2(rect.left(), rect.top() - 2.0),
        Align2::LEFT_BOTTOM,
        "L",
        FontId::proportional(crate::theme::text::TINY),
        weak,
    );
    painter.text(
        pos2(rect.left(), rect.bottom() + 2.0),
        Align2::LEFT_TOP,
        "R",
        FontId::proportional(crate::theme::text::TINY),
        weak,
    );
    let label = if synced {
        format!("{} · {:.0} ms", NOTES[note].0, seconds * 1000.0)
    } else {
        format!("{:.0} ms", seconds * 1000.0)
    };
    painter.text(
        pos2(full.right(), full.top()),
        Align2::RIGHT_TOP,
        format!("{label} · {:.0}% feedback", feedback * 100.0),
        FontId::proportional(crate::theme::text::SMALL),
        visuals.text_color(),
    );

    // The band the repeats keep, on a 20 Hz to 20 kHz scale.
    let band = Rect::from_min_max(pos2(full.left(), full.bottom() - strip), full.max);
    painter.rect_filled(band, crate::theme::radius::SMALL, weak.gamma_multiply(0.15));
    let hz_x = |hz: f32| band.left() + (hz / 20.0).ln() / 1000f32.ln() * band.width();
    let (low, high) = (panel.value("low_cut"), panel.value("high_cut"));
    painter.rect_filled(
        Rect::from_x_y_ranges(hz_x(low)..=hz_x(high).max(hz_x(low) + 1.0), band.y_range()),
        2.0,
        accent.gamma_multiply(0.5),
    );

    // Sideways drags in tempo mode step through note values, carrying the
    // remainder over between frames.
    let carry = response.id.with("carry");
    if response.drag_started() {
        ui.data_mut(|d| d.insert_temp(carry, 0.0f32));
    }
    if response.dragged() {
        let delta = response.drag_delta();
        let feedback_index = panel.index("feedback");
        if delta.y != 0.0
            && let Some(index) = feedback_index
        {
            panel.set(index, feedback * 100.0 - delta.y * 0.5);
        }
        if synced {
            let moved = ui.data(|d| d.get_temp::<f32>(carry)).unwrap_or(0.0) + delta.x;
            let steps = (moved / 24.0).trunc();
            ui.data_mut(|d| d.insert_temp(carry, moved - steps * 24.0));
            if steps != 0.0 {
                let order = notes_by_length();
                let at = order.iter().position(|n| *n == note).unwrap_or(0) as f32;
                let to = (at + steps).clamp(0.0, (order.len() - 1) as f32) as usize;
                panel.set_id("note", order[to] as f32);
            }
        } else if delta.x != 0.0 {
            panel.set_id("time", seconds * 1000.0 * (delta.x / 150.0).exp());
        }
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text("Drag sideways for the time, up or down for the feedback");
}
