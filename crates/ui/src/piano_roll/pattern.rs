//! The Strum Pattern tool's window: a preset menu, the pattern's text
//! (checked as it's typed), a preview of its steps, and the rest of
//! [`StrumPattern`]'s settings. The tool itself is
//! `project::note_tools::patterns`; this only edits its settings, and the
//! Tools window previews and applies them like the other tools'.
//!
//! The step preview draws one cycle of the pattern on its grid, as it will
//! play: each stroke a block as long as it sounds (its gate, its ties, its
//! swing), accents bright and the rest dimmer, palm mutes amber and low,
//! dead notes as crosses, a bass note low in its step and a chord without
//! its bass high, and above each stroke an arrow for the way the hand
//! moves there (from where it falls, as the Guitar reads it).

use eframe::egui::{
    self, Color32, ComboBox, DragValue, Painter, Pos2, Rect, RichText, Sense, Stroke, TextEdit,
    TextStyle, pos2, vec2,
};
use project::TimeSignature;
use project::note_tools::patterns::{
    Articulation, Direction, Hit, MAX_SWING, MIN_GATE, PRESETS, Pattern, PatternError, Resolution,
    Step, StrumPattern,
};

use crate::theme::{self, color, radius, space, stroke, text};
use crate::widgets::chrome;

/// How wide the step preview is drawn, at most, and how tall.
const PREVIEW_WIDTH: f32 = 320.0;
const PREVIEW_HEIGHT: f32 = 46.0;
/// The widest and narrowest a step is drawn.
const MAX_CELL: f32 = 40.0;
const MIN_CELL: f32 = 5.0;
/// The band along the preview's top for the arrows.
const ARROW_BAND: f32 = 13.0;
/// How wide the text field and the preset menu are, and how tall the
/// menu opens (room for every preset).
const FIELD_WIDTH: f32 = 190.0;
const PRESETS_HEIGHT: f32 = 420.0;

/// What the pattern syntax is, for the text field's tooltip.
const SYNTAX: &str = "One character per step (upper case is accented):\n\
    D or U  strum (up or down comes from where it falls)\n\
    C  the chord without its bass\n\
    B  the bass note only\n\
    X  dead-note chuck\n\
    P  palm mute\n\
    -  rest\n\
    _  hold the stroke before\n\
    Spaces and | only group steps.";

/// What the window shows about where it's used.
pub(super) struct Context {
    /// The song's meter, to say how long a pattern is.
    pub meter: TimeSignature,
    /// Whether any notes are selected to strum.
    pub selected: bool,
    /// How the track's instrument plays dead notes and palm mutes.
    pub articulation: Articulation,
}

/// The tool's settings, and the pattern as typed.
pub(super) struct PatternTool {
    /// What Apply plays: its pattern is the last one typed that could be.
    pub settings: StrumPattern,
    /// The text field, which may not parse yet.
    text: String,
    error: Option<PatternError>,
}

impl Default for PatternTool {
    fn default() -> Self {
        let settings = StrumPattern::default();
        PatternTool {
            text: settings.pattern.text().to_owned(),
            settings,
            error: None,
        }
    }
}

impl PatternTool {
    /// Whether the typed pattern can be played.
    pub fn valid(&self) -> bool {
        self.error.is_none()
    }

    /// The window's settings.
    pub fn ui(&mut self, ui: &mut egui::Ui, context: &Context) {
        let PatternTool {
            settings: s,
            text,
            error,
        } = self;
        egui::Grid::new("strum_pattern_text")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Preset");
                let current = s.preset().map_or("Custom", |p| p.name);
                ComboBox::from_id_salt("strum_preset")
                    .selected_text(current)
                    .width(FIELD_WIDTH)
                    .height(PRESETS_HEIGHT)
                    .show_ui(ui, |ui| {
                        for preset in PRESETS {
                            let item = ui
                                .selectable_label(current == preset.name, preset.name)
                                .on_hover_text(preset.about);
                            if item.clicked() {
                                preset.apply(s);
                                *text = preset.pattern.to_owned();
                                *error = None;
                            }
                        }
                    });
                ui.end_row();

                ui.label("Pattern");
                let field = ui
                    .add(
                        TextEdit::singleline(text)
                            .font(TextStyle::Monospace)
                            .desired_width(FIELD_WIDTH)
                            .hint_text("D_du_uDu"),
                    )
                    .on_hover_text(SYNTAX);
                if field.changed() {
                    match Pattern::parse(text) {
                        Ok(pattern) => {
                            s.pattern = pattern;
                            *error = None;
                        }
                        Err(e) => *error = Some(e),
                    }
                }
                ui.end_row();
            });
        match error {
            Some(e) => {
                ui.add(
                    egui::Label::new(
                        RichText::new(e.to_string())
                            .size(text::SMALL)
                            .color(ui.visuals().error_fg_color),
                    )
                    .wrap(),
                );
            }
            None => chrome::caption(ui, &length_text(s, context.meter)),
        }
        ui.add_space(space::XS);
        preview(ui, s, error.is_some(), context.meter);
        ui.add_space(space::S);

        egui::Grid::new("strum_pattern_settings")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Grid");
                ComboBox::from_id_salt("strum_resolution")
                    .selected_text(s.resolution.label())
                    .show_ui(ui, |ui| {
                        for resolution in Resolution::ALL {
                            ui.selectable_value(&mut s.resolution, resolution, resolution.label());
                        }
                    });
                ui.end_row();

                ui.label("Swing");
                ui.add_enabled_ui(s.resolution.swings(), |ui| {
                    percent(ui, &mut s.swing, 0.0, MAX_SWING)
                        .on_hover_text("How late the off-beats come: 33% is a triplet shuffle, 50% a dotted one")
                        .on_disabled_hover_text("Triplets already swing");
                });
                ui.end_row();

                ui.label("Gate");
                percent(ui, &mut s.gate, MIN_GATE, 1.0).on_hover_text(
                    "How much of its step a stroke rings: 100% up to the next, 30% a staccato chop",
                );
                ui.end_row();

                ui.label("Velocity");
                ui.horizontal(|ui| {
                    percent(ui, &mut s.accent, 0.05, 1.5)
                        .on_hover_text("Accented strokes, as a share of each chord's velocity");
                    chrome::caption(ui, "accents");
                    ui.add_space(space::S);
                    percent(ui, &mut s.normal, 0.05, 1.5)
                        .on_hover_text("The other strokes, as a share of each chord's velocity");
                    chrome::caption(ui, "others");
                });
                ui.end_row();

                ui.label("Upstrokes");
                ui.horizontal(|ui| {
                    for (skip, label) in [(0, "All strings"), (1, "Skip bass"), (2, "Skip two")] {
                        ui.selectable_value(&mut s.upstroke_skip, skip, label)
                            .on_hover_text("An upstroke mostly catches the top strings: leave out the lowest notes");
                    }
                });
                ui.end_row();

                ui.label("Humanize");
                ui.horizontal(|ui| {
                    percent(ui, &mut s.humanize, 0.0, 1.0).on_hover_text(
                        "A little timing and velocity variation (the Guitar varies its strums too)",
                    );
                    if s.humanize > 0.0 && ui.button("Reroll").clicked() {
                        s.seed = s.seed.wrapping_add(1);
                    }
                });
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut s.restart, "Start again on each chord")
                    .on_hover_text("Off, the pattern keeps the bar's time across chord changes, as a strumming hand does");
                ui.end_row();
            });

        if !context.selected {
            ui.add_space(space::XS);
            ui.colored_label(ui.visuals().warn_fg_color, "Select held chords to strum");
        }
        let articulated = s.pattern.steps().iter().any(|step| {
            matches!(
                step,
                Step::Hit {
                    hit: Hit::Dead | Hit::PalmMute,
                    ..
                }
            )
        });
        if articulated {
            chrome::caption(
                ui,
                match context.articulation {
                    Articulation::KeySwitches { .. } => {
                        "Dead notes and palm mutes switch the Guitar's articulation"
                    }
                    Articulation::Shortened => {
                        "Dead notes and palm mutes play as short, quiet strums"
                    }
                },
            );
        }
    }
}

/// `value` (a share) as a percentage from `min` to `max`. The response,
/// for a tooltip.
fn percent(ui: &mut egui::Ui, value: &mut f32, min: f32, max: f32) -> egui::Response {
    let mut p = *value * 100.0;
    let response = ui.add(
        DragValue::new(&mut p)
            .range(min * 100.0..=max * 100.0)
            .speed(0.5)
            .max_decimals(0)
            .suffix("%"),
    );
    *value = p / 100.0;
    response
}

/// "8 steps · 1 bar of 4/4", and the meter a preset was written for if
/// it isn't the song's.
fn length_text(s: &StrumPattern, meter: TimeSignature) -> String {
    let n = s.pattern.len();
    let ticks = n as i64 * s.resolution.step().get();
    let bar = meter.bar_len().get().max(1);
    let bars = ticks as f64 / bar as f64;
    let size = if ticks % bar == 0 {
        match ticks / bar {
            1 => "1 bar".to_owned(),
            whole => format!("{whole} bars"),
        }
    } else {
        format!("{} bar", format!("{bars:.2}").trim_end_matches('0'))
    };
    let mut line = format!(
        "{n} steps · {size} of {}/{}",
        meter.numerator, meter.denominator
    );
    if let Some(preset) = s.preset()
        && preset.meter != (meter.numerator, meter.denominator)
    {
        line += &format!(" · written for {}/{}", preset.meter.0, preset.meter.1);
    }
    line
}

/// One cycle of the pattern as it plays (see the module documentation).
/// `stale` greys it out: the text has changed into something unplayable.
fn preview(ui: &mut egui::Ui, s: &StrumPattern, stale: bool, meter: TimeSignature) {
    let steps = s.pattern.steps();
    let n = steps.len().max(1);
    let cell = (PREVIEW_WIDTH / n as f32).clamp(MIN_CELL, MAX_CELL);
    let (rect, response) =
        ui.allocate_exact_size(vec2(cell * n as f32, PREVIEW_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect.expand(stroke::HAIRLINE));
    let visuals = ui.visuals();
    painter.rect_filled(rect, radius::PANEL, visuals.extreme_bg_color);

    // Lines between steps, stronger between beats and bars.
    let per_beat = s.resolution.per_beat();
    let step_ticks = s.resolution.step().get();
    let per_bar = Some(meter.bar_len().get())
        .filter(|bar| bar % step_ticks == 0)
        .map(|bar| bar / step_ticks);
    for i in 1..n as i64 {
        let weight = if per_bar.is_some_and(|b| i % b == 0) {
            0.45
        } else if i % per_beat == 0 {
            0.22
        } else {
            0.07
        };
        let x = rect.left() + i as f32 * cell;
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(
                stroke::HAIRLINE,
                visuals.text_color().gamma_multiply(weight),
            ),
        );
    }

    let fade = if stale { 0.35 } else { 1.0 };
    let accent = theme::accent(visuals).gamma_multiply(fade);
    let ink = visuals.text_color().gamma_multiply(fade);
    let weak = visuals.weak_text_color().gamma_multiply(fade);
    let body = Rect::from_min_max(
        pos2(rect.left(), rect.top() + ARROW_BAND + space::XS),
        pos2(rect.right(), rect.bottom() - space::S),
    );
    let swing = if s.resolution.swings() {
        s.swing.clamp(0.0, MAX_SWING)
    } else {
        0.0
    };
    // Where step `i` starts, in steps, swung.
    let at = |i: usize| i as f32 + if i % 2 == 1 { swing } else { 0.0 };
    let gate = s.gate.clamp(MIN_GATE, 1.0);
    let directions = s.pattern.directions(s.resolution);
    for (i, step) in steps.iter().enumerate() {
        let Step::Hit {
            hit,
            accent: strong,
        } = *step
        else {
            continue;
        };
        let ties = steps[i + 1..]
            .iter()
            .take_while(|s| **s == Step::Tie)
            .count();
        let last = i + ties;
        let start = rect.left() + at(i) * cell;
        let end = rect.left() + (at(last) + gate * (at(last + 1) - at(last))) * cell;
        let end = end.min(rect.right()).max(start + 2.0);
        let up = directions[i] == Some(Direction::Up);
        // The bass sits low in a step, the top strings high.
        let (top, bottom) = match hit {
            Hit::Bass => (0.62, 1.0),
            Hit::Chord => (0.0, 0.62),
            Hit::PalmMute => (0.38, 1.0),
            _ if up => (0.0, 1.0 - 0.2 * f32::from(s.upstroke_skip.min(2))),
            _ => (0.0, 1.0),
        };
        let block = Rect::from_min_max(
            pos2(start + 1.0, body.top() + top * body.height()),
            pos2(end - 1.0, body.top() + bottom * body.height()),
        );
        // The middle of the stroke's first step, where its arrow goes.
        let mid = start + cell.min(end - start) / 2.0;
        match hit {
            Hit::Dead => cross(&painter, mid, block, cell, if strong { ink } else { weak }),
            Hit::PalmMute => {
                let amber = color::MUTE.gamma_multiply(if strong { fade } else { 0.5 * fade });
                painter.rect_filled(block, radius::SMALL, amber);
            }
            _ => {
                let fill = if strong {
                    accent
                } else {
                    accent.gamma_multiply(0.5)
                };
                painter.rect_filled(block, radius::SMALL, fill);
            }
        }
        let center = pos2(mid, rect.top() + ARROW_BAND / 2.0 + space::XS / 2.0);
        arrow(
            &painter,
            center,
            ARROW_BAND - space::S,
            cell,
            up,
            if strong { ink } else { weak },
        );
    }
    response.on_hover_text(
        "One time through the pattern, as it plays: accents bright, palm mutes amber, \
         dead notes crossed; the arrows are the strumming hand's direction",
    );
}

/// A dead note's cross, at `x` in `block`, no wider than a step.
fn cross(painter: &Painter, x: f32, block: Rect, cell: f32, ink: Color32) {
    let size = (block.height().min(cell * 0.7)).min(12.0) / 2.0;
    let c = pos2(x, block.center().y);
    let line = Stroke::new(stroke::EMPHASIS, ink);
    painter.line_segment([c + vec2(-size, -size), c + vec2(size, size)], line);
    painter.line_segment([c + vec2(-size, size), c + vec2(size, -size)], line);
}

/// An arrow `height` tall centred on `at`, pointing up or down, no wider
/// than a step.
fn arrow(painter: &Painter, at: Pos2, height: f32, cell: f32, up: bool, ink: Color32) {
    let half = height / 2.0;
    let (tip, tail) = if up {
        (at.y - half, at.y + half)
    } else {
        (at.y + half, at.y - half)
    };
    let line = Stroke::new(stroke::EMPHASIS * 0.8, ink);
    painter.line_segment([pos2(at.x, tail), pos2(at.x, tip)], line);
    let wing = (height * 0.35).min(cell * 0.35);
    let back = if up { tip + wing } else { tip - wing };
    painter.line_segment([pos2(at.x - wing, back), pos2(at.x, tip)], line);
    painter.line_segment([pos2(at.x + wing, back), pos2(at.x, tip)], line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_length_says_how_much_of_a_bar_it_is() {
        let mut s = StrumPattern::default();
        let four = TimeSignature::default();
        assert_eq!(length_text(&s, four), "8 steps · 1 bar of 4/4");
        PRESETS
            .iter()
            .find(|p| p.meter == (3, 4))
            .unwrap()
            .apply(&mut s);
        assert_eq!(
            length_text(&s, four),
            "6 steps · 0.75 bar of 4/4 · written for 3/4"
        );
        let bossa = PRESETS.iter().find(|p| p.name == "Bossa nova").unwrap();
        bossa.apply(&mut s);
        assert_eq!(length_text(&s, four), "16 steps · 2 bars of 4/4");
    }
}
