//! The Guitar's panel: a fretboard showing where each note is played (dots
//! filled while held, hollow while ringing on; diamonds for harmonics over
//! the fret they're touched at, crosses for dead notes, a ring for a
//! palm-muted note), strings shaking with their level, the fretting hand's
//! position, the last strum's direction, and the body beyond the neck: the
//! sound hole, or the pickups with the ones switched in lit. Beside it the
//! model, articulation and technique; then its knobs in two rows, grouped
//! like the instrument.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, vec2};
use instruments::builtin::guitar::{
    METER_ARTICULATION, METER_BEND, METER_FRET, METER_HAND, METER_HELD, METER_KIND, METER_LEVEL,
    METER_MUTE, METER_NOTE, METER_STRUM,
};

use super::Panel;
use super::panel::{display, knob_slot};
use crate::theme::{self, radius, space, stroke, text};

/// Frets shown on the board.
const FRETS: f32 = 15.0;
const STRINGS: usize = 6;

/// Model indices (the `model` parameter's labels).
const ELECTRIC: usize = 2;

/// Where the pickups sit, as a share of the scale from the saddle: neck,
/// middle, bridge (a Stratocaster's).
const PICKUPS: [f32; 3] = [0.247, 0.154, 0.063];

/// Which pickups each switch position (`pickup`) uses.
const SWITCH: [[bool; 3]; 5] = [
    [true, false, false],
    [true, true, false],
    [false, true, false],
    [false, true, true],
    [false, false, true],
];

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    // Two rows of knobs under the fretboard, sized to leave it room.
    let row = panel.fit_knobs_beside(ui, 2, 92.0);
    let gaps = 2.0 * ui.spacing().item_spacing.y;
    let height = (ui.available_height() - 2.0 * row - gaps).clamp(64.0, 150.0);
    let electric = panel.value("model") as usize == ELECTRIC;
    ui.horizontal_top(|ui| {
        ui.set_max_height(height);
        fretboard(ui, panel, vec2(height * 4.6, height), electric);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::XS;
            panel.choice(ui, "model");
            panel.choice(ui, "articulation");
            ui.horizontal(|ui| {
                panel.choice(ui, "technique");
                ui.separator();
                panel.toggle(ui, "key_switches", "Key switches");
            });
            ui.add_enabled_ui(electric, |ui| {
                ui.horizontal(|ui| {
                    panel.choice_menu(ui, "pickup");
                    panel.choice(ui, "pickup_type");
                });
            });
            articulation_note(ui, panel);
        });
    });
    // Each row capped at its height: separators stretch to what's left.
    ui.horizontal_top(|ui| {
        ui.set_max_height(row);
        ui.spacing_mut().item_spacing.x = space::XS;
        panel.knob(ui, "tone");
        panel.knob(ui, "sustain");
        ui.add_enabled_ui(!electric, |ui| {
            panel.knob(ui, "body");
        });
        ui.separator();
        panel.knob_as(ui, "pick", Some("Hardness"));
        panel.knob_as(ui, "pluck_position", Some("Position"));
        panel.knob(ui, "mute");
        ui.separator();
        panel.knob(ui, "strum");
        menu_with_caption(ui, panel, "strum_direction", "Direction");
        panel.knob(ui, "humanize");
    });
    let amp_on = panel.value("amp") >= 0.5;
    ui.horizontal_top(|ui| {
        ui.set_max_height(row);
        ui.spacing_mut().item_spacing.x = space::XS;
        panel.knob(ui, "vibrato");
        menu_with_caption(ui, panel, "legato", "Legato");
        ui.separator();
        panel.knob(ui, "noises");
        panel.knob(ui, "squeaks");
        panel.knob(ui, "resonance");
        ui.separator();
        knob_slot(ui, panel.knob_size, "", |ui| panel.choice(ui, "amp"));
        ui.add_enabled_ui(amp_on, |ui| {
            panel.knob(ui, "drive");
            panel.knob_as(ui, "amp_tone", Some("Amp tone"));
        });
        ui.separator();
        panel.knob(ui, "level");
    });
}

/// A menu named `caption`, lined up with the knobs beside it.
fn menu_with_caption(ui: &mut Ui, panel: &mut Panel, id: &str, caption: &str) {
    knob_slot(ui, panel.knob_size, caption, |ui| panel.choice_menu(ui, id));
}

/// When a key-switch has picked another articulation than the setting
/// shows, says which.
fn articulation_note(ui: &mut Ui, panel: &Panel) {
    let playing = panel.meters[METER_ARTICULATION].round() as usize;
    if playing != panel.value("articulation").round() as usize {
        let name = ["Open", "Palm mute", "Harmonics", "Dead notes"]
            .get(playing)
            .copied()
            .unwrap_or("?");
        crate::widgets::chrome::caption(ui, &format!("Key-switched: {name}"));
    }
}

fn fretboard(ui: &mut Ui, panel: &Panel, size: egui::Vec2, electric: bool) {
    let (response, painter) = display(ui, size, Sense::hover());
    let inner = response.rect.shrink2(vec2(22.0, 12.0));
    // The neck takes most of the width; the body beyond it the rest.
    let board = Rect::from_min_max(
        inner.min,
        Pos2::new(inner.left() + inner.width() * 0.8, inner.bottom()),
    );
    let body = Rect::from_min_max(Pos2::new(board.right() + space::S, inner.top()), inner.max);
    let visuals = ui.visuals().clone();
    let meters = panel.meters;
    // Frets get closer up the neck, as on a guitar.
    let span = 1.0 - 2f32.powf(-FRETS / 12.0);
    let fret_x =
        |fret: f32| board.left() + (1.0 - 2f32.powf(-fret.max(0.0) / 12.0)) / span * board.width();
    // Where a finger goes: between a fret and the one before; open strings
    // just left of the nut.
    let finger_x = |fret: f32| {
        if fret <= 0.0 {
            board.left() - 10.0
        } else if fret > FRETS {
            board.right() - 4.0
        } else {
            (fret_x(fret - 1.0) + fret_x(fret)) / 2.0
        }
    };
    let string_y = |string: usize| {
        // High E on top, as in tab.
        board.top() + (STRINGS - 1 - string) as f32 / (STRINGS - 1) as f32 * board.height()
    };
    let weak = visuals.weak_text_color();
    let accent = theme::accent(&visuals);

    // The hand's reach.
    let hand = meters[METER_HAND];
    if hand > 0.0 {
        painter.rect_filled(
            Rect::from_x_y_ranges(
                fret_x(hand - 1.0)..=fret_x(hand + 3.0).min(board.right()),
                board.y_range(),
            ),
            radius::SMALL,
            visuals.selection.bg_fill.gamma_multiply(0.18),
        );
    }
    // Frets, the nut, and inlays.
    painter.vline(board.left(), board.y_range(), Stroke::new(3.0, weak));
    for fret in 1..=(FRETS as usize) {
        painter.vline(
            fret_x(fret as f32),
            board.y_range(),
            Stroke::new(stroke::HAIRLINE, weak.gamma_multiply(0.5)),
        );
        let middle = finger_x(fret as f32);
        if matches!(fret, 3 | 5 | 7 | 9 | 15) {
            painter.circle_filled(
                Pos2::new(middle, board.center().y),
                3.0,
                weak.gamma_multiply(0.35),
            );
        } else if fret == 12 {
            for y in [string_y(1), string_y(3)] {
                painter.circle_filled(
                    Pos2::new(middle, (y + board.center().y) / 2.0),
                    3.0,
                    weak.gamma_multiply(0.35),
                );
            }
        }
        if matches!(fret, 3 | 5 | 7 | 9 | 12 | 15) {
            painter.text(
                Pos2::new(middle, response.rect.bottom() - 1.0),
                Align2::CENTER_BOTTOM,
                format!("{fret}"),
                FontId::proportional(text::TINY),
                weak,
            );
        }
    }
    body_view(&painter, body, panel, electric, &visuals);

    // The strings, shaking with their level, on across the body.
    let time = ui.input(|i| i.time) as f32;
    let string_end = body.right();
    for string in 0..STRINGS {
        let y = string_y(string);
        let level = meters[METER_LEVEL + string].clamp(0.0, 1.0);
        let thickness = 0.8 + (STRINGS - 1 - string) as f32 * 0.25;
        let color = if level > 0.01 {
            visuals.strong_text_color()
        } else {
            visuals.text_color().gamma_multiply(0.6)
        };
        if level > 0.01 {
            let amplitude = level.sqrt() * 3.0;
            let points: Vec<Pos2> = (0..=80)
                .map(|i| {
                    let t = i as f32 / 80.0;
                    let x = board.left() + t * (string_end - board.left());
                    let wobble = (std::f32::consts::PI * t).sin()
                        * (time * 40.0 + string as f32).sin()
                        * amplitude;
                    Pos2::new(x, y + wobble)
                })
                .collect();
            painter.add(Shape::line(points, Stroke::new(thickness, color)));
        } else {
            painter.hline(board.left()..=string_end, y, Stroke::new(thickness, color));
        }
    }

    // Where each sounding note is, marked by what kind of note it is.
    for string in 0..STRINGS {
        let note = meters[METER_NOTE + string];
        if note < 0.0 {
            continue;
        }
        let fret = meters[METER_FRET + string];
        let held = meters[METER_HELD + string] >= 0.5;
        let at = Pos2::new(
            finger_x(fret).clamp(board.left() - 12.0, board.right()),
            string_y(string),
        );
        let kind = meters[METER_KIND + string].round() as usize;
        let fill = if held { accent } else { Color32::TRANSPARENT };
        let outline = Stroke::new(stroke::EMPHASIS, accent);
        match kind {
            // A harmonic: a diamond over the fret it's touched at.
            2 => {
                let r = 7.5;
                painter.add(Shape::convex_polygon(
                    vec![
                        at + vec2(0.0, -r),
                        at + vec2(r, 0.0),
                        at + vec2(0.0, r),
                        at + vec2(-r, 0.0),
                    ],
                    fill,
                    outline,
                ));
            }
            // A dead note: a cross.
            3 => {
                let r = 5.0;
                painter.line_segment(
                    [at + vec2(-r, -r), at + vec2(r, r)],
                    Stroke::new(2.0, accent),
                );
                painter.line_segment(
                    [at + vec2(-r, r), at + vec2(r, -r)],
                    Stroke::new(2.0, accent),
                );
                continue;
            }
            _ => {
                painter.circle(at, 7.0, fill, outline);
                // Palm-muted: a second ring.
                if kind == 1 {
                    painter.circle_stroke(
                        at,
                        9.5,
                        Stroke::new(stroke::HAIRLINE, accent.gamma_multiply(0.6)),
                    );
                }
            }
        }
        let name = project::NOTE_NAMES[(note.round() as i32).rem_euclid(12) as usize];
        painter.text(
            at,
            Align2::CENTER_CENTER,
            name,
            FontId::proportional(text::TINY),
            if held {
                Color32::from_black_alpha(230)
            } else {
                accent
            },
        );
    }
    // The last strum's direction, the palm on the strings, and any bend.
    let strum = meters[METER_STRUM];
    if strum != 0.0 {
        let x = response.rect.right() - 10.0;
        let (top, bottom) = (string_y(STRINGS - 1), string_y(0));
        // A down-stroke goes low string to high: up the screen in tab.
        let (from, to) = if strum > 0.0 {
            (bottom, top)
        } else {
            (top, bottom)
        };
        painter.arrow(
            Pos2::new(x, from),
            vec2(0.0, to - from),
            Stroke::new(stroke::EMPHASIS, weak),
        );
    }
    let mute = meters[METER_MUTE];
    if mute > 0.05 {
        // The palm rests on the strings by the bridge.
        let x = body.right() - 6.0;
        painter.rect_filled(
            Rect::from_x_y_ranges(x - 6.0..=x, board.y_range()),
            radius::CONTROL,
            accent.gamma_multiply(0.15 + 0.4 * mute),
        );
        painter.text(
            Pos2::new(x - 3.0, response.rect.top() + 1.0),
            Align2::CENTER_TOP,
            "P.M.",
            FontId::proportional(text::TINY),
            weak,
        );
    }
    let bend = meters[METER_BEND];
    if bend.abs() > 0.05 {
        painter.text(
            response.rect.left_top() + vec2(space::S, 1.0),
            Align2::LEFT_TOP,
            format!("bend {bend:+.1}"),
            FontId::proportional(text::TINY),
            visuals.text_color(),
        );
    }
    if meters[METER_LEVEL..METER_LEVEL + STRINGS]
        .iter()
        .any(|l| *l > 0.01)
    {
        ui.ctx().request_repaint();
    }
}

/// The body beyond the neck: an acoustic's sound hole, or an electric's
/// pickups at their places along the string, lit when switched in.
fn body_view(
    painter: &egui::Painter,
    body: Rect,
    panel: &Panel,
    electric: bool,
    visuals: &egui::Visuals,
) {
    let weak = visuals.weak_text_color();
    let accent = theme::accent(visuals);
    if electric {
        // The bridge at the right edge; the pickups this far from it.
        let length = body.width() / 0.32;
        let bridge = body.right() - 2.0;
        painter.vline(bridge, body.y_range(), Stroke::new(2.0, weak));
        let switch = SWITCH[(panel.value("pickup") as usize).min(4)];
        let humbuckers = panel.value("pickup_type") >= 0.5;
        for (place, &on) in PICKUPS.iter().zip(&switch) {
            let x = bridge - place * length;
            let width = if humbuckers { 9.0 } else { 5.0 };
            let rect = Rect::from_x_y_ranges(
                x - width / 2.0..=x + width / 2.0,
                body.y_range().expand(3.0),
            );
            if on {
                painter.rect_filled(rect, radius::SMALL, accent.gamma_multiply(0.45));
            }
            painter.rect_stroke(
                rect,
                radius::SMALL,
                Stroke::new(
                    stroke::HAIRLINE,
                    if on { accent } else { weak.gamma_multiply(0.6) },
                ),
                egui::StrokeKind::Inside,
            );
        }
    } else {
        let radius = (body.height() * 0.42).min(body.width() * 0.45);
        painter.circle_stroke(
            Pos2::new(body.left() + body.width() * 0.35, body.center().y),
            radius,
            Stroke::new(1.5, weak.gamma_multiply(0.6)),
        );
        painter.vline(
            body.right() - 4.0,
            body.y_range(),
            Stroke::new(3.0, weak.gamma_multiply(0.6)),
        );
    }
}
