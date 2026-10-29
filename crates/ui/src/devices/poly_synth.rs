//! The Poly Synth's panel, in three cards: the oscillator, its waveform
//! drawn with the second, detuned oscillator beside it; the filter, its
//! response drawn at the cutoff and resonance (and, faintly, where the
//! envelope opens it to), dragged sideways for the cutoff and up or down
//! for the resonance; and the envelope, as an editable curve.

use eframe::egui::{Pos2, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};

use super::Panel;
use super::panel::display;
use crate::theme::{self, space, stroke};
use crate::widgets::spectrum::Axes;
use crate::widgets::{chrome, knob};

/// A compact knob's width, which the cards' widths are counted in.
const KNOB: f32 = knob::compact_width(knob::COMPACT);
/// The oscillator's and the filter's cards: four knobs wide.
const CARD_WIDTH: f32 = 4.0 * KNOB;
/// How much wider than tall the envelope's curve is, and its widest.
const ENVELOPE_ASPECT: f32 = 2.0;
const MAX_ENVELOPE_WIDTH: f32 = 420.0;
/// Points drawn along the oscillator's two cycles.
const WAVE_POINTS: usize = 120;

/// The synth's filter's damping (`1 / Q`) at resonance 0 and 1: what
/// `dsp::Svf::set_resonance` moves between, linearly.
const MAX_DAMPING: f32 = 2.0;
const MIN_DAMPING: f32 = 0.05;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let height = ui.available_height();
    let compact = std::mem::replace(&mut panel.compact, true);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::M;
        let display = super::wavetable::display_height(height, 0, 1);
        oscillator(ui, panel, display);
        filter(ui, panel, display);
        envelope(ui, panel, height);
    });
    panel.compact = compact;
}

/// The waveform as buttons, its two cycles drawn, and the detune.
fn oscillator(ui: &mut Ui, panel: &mut Panel, display: f32) {
    let color = theme::section::OSC_A;
    chrome::section(ui, color, |ui| {
        ui.set_width(CARD_WIDTH);
        chrome::section_header(ui, "Oscillator", color, |ui| {
            panel.choice(ui, "waveform");
        });
        wave_display(ui, panel, vec2(CARD_WIDTH, display));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            panel.knob(ui, "detune");
        });
    });
}

/// Two cycles of the chosen wave, and behind it the second oscillator,
/// shifted as far as Detune pulls the two apart (half a cycle at the most):
/// a picture of the drift heard as the sound's beating and width.
fn wave_display(ui: &mut Ui, panel: &Panel, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(space::S, space::M));
    let color = theme::section::OSC_A;
    let waveform = panel.value("waveform") as usize;
    let detune = panel.index("detune").map_or(0.0, |index| {
        panel.params[index].to_normalized(panel.value("detune"))
    });
    let wave = |shift: f32| -> Vec<Pos2> {
        (0..=WAVE_POINTS)
            .map(|i| {
                let t = i as f32 / WAVE_POINTS as f32;
                let phase = (2.0 * t + shift).fract();
                let y = match waveform {
                    0 => (std::f32::consts::TAU * phase).sin(),
                    1 => 2.0 * phase - 1.0,
                    _ => {
                        if phase < 0.5 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                };
                pos2(
                    rect.left() + t * rect.width(),
                    rect.center().y - 0.9 * y * rect.height() / 2.0,
                )
            })
            .collect()
    };
    let visuals = ui.visuals();
    painter.hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(
            stroke::HAIRLINE,
            visuals.weak_text_color().gamma_multiply(0.3),
        ),
    );
    if detune > 0.0 {
        painter.add(Shape::line(
            wave(0.5 * detune),
            Stroke::new(stroke::HAIRLINE, color.gamma_multiply(0.45)),
        ));
    }
    painter.add(Shape::line(wave(0.0), Stroke::new(stroke::EMPHASIS, color)));
    response.on_hover_text("The two oscillators, the second detuned from the first");
}

/// The filter's response, and its knobs with the output's gain.
fn filter(ui: &mut Ui, panel: &mut Panel, display: f32) {
    let color = theme::section::FILTER;
    chrome::section(ui, color, |ui| {
        ui.set_width(CARD_WIDTH);
        chrome::section_header(ui, "Filter", color, |ui| {
            chrome::caption(ui, "Low pass 12 dB");
        });
        filter_display(ui, panel, vec2(CARD_WIDTH, display));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            panel.knob(ui, "cutoff");
            panel.knob_as(ui, "resonance", Some("Res"));
            panel.knob_as(ui, "env_amount", Some("Env"));
            panel.knob(ui, "gain");
        });
    });
}

/// The filter's response in dB at `hz`: the synth's state-variable lowpass
/// at `cutoff` and `resonance` (0 to 1).
fn response_db(cutoff: f32, resonance: f32, hz: f32, sample_rate: f32) -> f32 {
    let damping = MAX_DAMPING - (MAX_DAMPING - MIN_DAMPING) * resonance.clamp(0.0, 1.0);
    let shape = dsp::svf::Shape::lowpass(cutoff, 1.0 / damping, sample_rate);
    let power = shape.power_at(dsp::svf::response_tan(hz, sample_rate));
    (10.0 * power.max(1e-12).log10()) as f32
}

/// The response at the cutoff and resonance, filled; faintly, where the
/// envelope at its peak opens it to. Drag sideways for the cutoff, up or
/// down for the resonance.
fn filter_display(ui: &mut Ui, panel: &mut Panel, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let axes = Axes {
        rect: response.rect.shrink2(vec2(space::S, space::S + space::XS)),
        low_hz: 20.0,
        high_hz: 20_000.0,
        floor_db: -36.0,
        ceiling_db: 24.0,
    };
    let visuals = ui.visuals().clone();
    axes.paint_grid(&painter, &visuals, 0.0);
    painter.hline(
        axes.rect.x_range(),
        axes.y(0.0),
        Stroke::new(
            stroke::HAIRLINE,
            visuals.weak_text_color().gamma_multiply(0.3),
        ),
    );
    let rate = panel.sample_rate;
    let (cutoff, resonance) = (panel.value("cutoff"), panel.value("resonance"));
    let curve = |cutoff: f32| -> Vec<Pos2> {
        let mut points = Vec::new();
        let mut x = axes.rect.left();
        while x <= axes.rect.right() {
            let db = response_db(cutoff, resonance, axes.hz(x), rate);
            points.push(pos2(x, axes.y(db)));
            x += 2.0;
        }
        points
    };
    let color = theme::section::FILTER;
    let opened = (cutoff * panel.value("env_amount").exp2()).min(20_000.0);
    if opened > cutoff * 1.01 {
        painter.add(Shape::line(
            curve(opened),
            Stroke::new(stroke::HAIRLINE, color.gamma_multiply(0.4)),
        ));
    }
    let line = curve(cutoff);
    for pair in line.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        painter.add(Shape::convex_polygon(
            vec![
                a,
                b,
                pos2(b.x, axes.rect.bottom()),
                pos2(a.x, axes.rect.bottom()),
            ],
            color.gamma_multiply(0.1),
            Stroke::NONE,
        ));
    }
    painter.add(Shape::line(line, Stroke::new(stroke::EMPHASIS, color)));
    let peak = response_db(cutoff, resonance, cutoff, rate);
    let dot = pos2(
        axes.x(cutoff),
        axes.y(peak)
            .clamp(axes.rect.top() + 4.0, axes.rect.bottom() - 2.0),
    );
    painter.circle_filled(dot, 4.0, color);
    if response.dragged() {
        let delta = response.drag_delta();
        panel.set_id("cutoff", cutoff * (delta.x / 60.0).exp2());
        panel.set_id("resonance", resonance - delta.y / 150.0);
    }
    if response.drag_stopped() {
        panel.finish();
    }
    response.on_hover_text(
        "Drag sideways for the cutoff, up or down for resonance; \
         the faint curve is where the envelope opens it to",
    );
}

/// The envelope as a curve to drag, as wide as the height suits.
fn envelope(ui: &mut Ui, panel: &mut Panel, height: f32) {
    let color = theme::section::ENVELOPE;
    chrome::section(ui, color, |ui| {
        chrome::section_header(ui, "Envelope", color, |_| {});
        let tall = super::wavetable::display_height(height, 0, 0);
        let size = vec2((tall * ENVELOPE_ASPECT).min(MAX_ENVELOPE_WIDTH), tall);
        // Drawn in the section's color rather than the track's, as the
        // Wavetable's envelopes are.
        let track = std::mem::replace(&mut panel.color, color);
        panel.envelope(ui, 0, size);
        panel.color = track;
    });
}
