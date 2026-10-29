//! The Drum Synth's panel: sixteen pads that light as they play (press one
//! to hear it and edit it); the selected pad's card, with its hit drawn
//! (its selected layer picked out of it) and its own knobs; its six layers
//! as chips sketching their sources, and the selected layer's controls in
//! cards (source, pitch envelope, filter; amp envelope, drawn, and output);
//! and the kit's knobs. Pads and layers copy, paste, reset and clear from
//! their right-click menus.

use std::hash::{Hash, Hasher};
use std::ops::Range;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use instruments::builtin::drums::{
    self, CHOKES, FilterKind, KIT_DRIVE, KIT_LEVEL, KIT_TUNE, LAYERS, Layer, Mode, Source, Voice,
};

use super::Panel;
use super::panel::{DISPLAY_INSET, display};
use crate::theme::{self, radius, size, space, stroke, text};
use crate::widgets::keys::KeyEvent;
use crate::widgets::{chrome, knob};

/// How fast a pad's light fades after a hit, per second.
const FADE_PER_SECOND: f32 = 4.0;

const PAD_COLUMNS: usize = 4;

/// The longest hit the display draws.
const PREVIEW_SECONDS: f32 = 3.0;
/// How often the display may redraw the hit while a knob is turning.
const PREVIEW_INTERVAL: f64 = 0.08;

/// The gap between pads.
const PAD_GAP: f32 = space::S;
/// A pad's width: room for the longest pad name ("Closed Hat").
const PAD_WIDTH: f32 = 60.0;
/// A compact knob's width, which the cards' widths are counted in.
const KNOB: f32 = knob::compact_width(knob::COMPACT);
/// The amp envelope's drawing, beside its knobs.
const ENVELOPE_WIDTH: f32 = 1.5 * KNOB;
/// The dot before each layer's chip, which switches it on and off.
const DOT_WIDTH: f32 = 12.0;
/// How many points a layer chip's sketch has.
const SKETCH_POINTS: usize = 24;

/// What the panel keeps between frames, per drum track.
#[derive(Debug, Clone, Default)]
struct State {
    pad: usize,
    /// Each pad's selected layer.
    layers: Vec<usize>,
    /// Each pad's light, falling back smoothly however short its hit.
    flash: Vec<f32>,
    /// The note a pad being pressed plays.
    held: Option<u8>,
}

impl State {
    fn new(pads: usize) -> Self {
        Self {
            pad: 0,
            layers: vec![0; pads],
            flash: vec![0.0; pads],
            held: None,
        }
    }
}

/// Settings copied from a pad or a layer, for pasting onto another, on
/// any drum track.
#[derive(Debug, Clone)]
enum Clipboard {
    Pad(Vec<f32>),
    Layer(Vec<f32>),
}

fn clipboard_id() -> egui::Id {
    egui::Id::new("drum synth clipboard")
}

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let voices = drums::voices();
    let state_id = panel.id("drums");
    let mut state: State = ui
        .data_mut(|d| d.remove_temp(state_id))
        .filter(|s: &State| s.layers.len() == voices.len())
        .unwrap_or_else(|| State::new(voices.len()));
    state.pad = state.pad.min(voices.len() - 1);

    let dt = ui.input(|i| i.stable_dt).min(0.1);
    for (i, light) in state.flash.iter_mut().enumerate() {
        let level = panel.meters.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        *light = level.max(*light - FADE_PER_SECOND * dt);
    }
    if state.flash.iter().any(|l| *l > 0.0) {
        ui.ctx().request_repaint();
    }

    let height = ui.available_height();
    let rows = (voices.len() / PAD_COLUMNS) as f32;
    let pad_height = ((height - (rows - 1.0) * PAD_GAP) / rows).clamp(36.0, 54.0);
    let compact = std::mem::replace(&mut panel.compact, true);
    // When the view is too short for the layer's two rows of sections, the
    // panel scrolls up and down rather than spilling out of its card.
    egui::ScrollArea::vertical()
        .id_salt(panel.id("drums scroll"))
        .auto_shrink([true, true])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::M;
                pads(ui, panel, voices, &mut state, pad_height);
                let voice = &voices[state.pad];
                let layer = state.layers[state.pad].min(LAYERS - 1);
                pad_section(ui, panel, state.pad, voice, layer, height);
                layers(ui, panel, voice, &mut state.layers[state.pad], height);
                kit(ui, panel, height);
            });
        });
    panel.compact = compact;
    ui.data_mut(|d| d.insert_temp(state_id, state));
}

/// The kit's own knobs, one over another in a narrow card as tall as the
/// pad's.
fn kit(ui: &mut Ui, panel: &mut Panel, height: f32) {
    let color = theme::section::NEUTRAL;
    chrome::section(ui, color, |ui| {
        ui.set_width(KNOB);
        ui.set_min_height(card_height(height) - 2.0 * space::S);
        chrome::section_header(ui, "Kit", color, |_| {});
        for index in [KIT_LEVEL, KIT_TUNE, KIT_DRIVE] {
            let label = panel.params[index].name.trim_start_matches("Kit ");
            panel.knob_at(ui, index, Some(label), None);
        }
    });
}

/// The pads, four by four; pressing one plays it and selects it.
fn pads(ui: &mut Ui, panel: &mut Panel, voices: &[Voice], state: &mut State, height: f32) {
    let values = panel.values().to_vec();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::splat(PAD_GAP);
        for (row, row_voices) in voices.chunks(PAD_COLUMNS).enumerate() {
            ui.horizontal(|ui| {
                for (column, voice) in row_voices.iter().enumerate() {
                    let index = row * PAD_COLUMNS + column;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(PAD_WIDTH, height), Sense::click_and_drag());
                    let choke = values[voice.param(Voice::CHOKE)] as usize;
                    let silent = (0..LAYERS).all(|l| {
                        Mode::from_value(values[voice.layer_param(l, Layer::MODE)]) != Mode::On
                    });
                    let hover = theme::hover(ui, &response);
                    paint_pad(
                        ui,
                        rect,
                        voice,
                        (state.flash[index], hover),
                        index == state.pad,
                        choke,
                        silent,
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, voice.name)
                    });
                    let response = response.on_hover_text(format!(
                        "{}: notes {}\nRight-click to copy, paste or reset",
                        voice.name,
                        voice
                            .notes
                            .iter()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    let primary = ui.input(|i| i.pointer.primary_down());
                    if response.is_pointer_button_down_on()
                        && primary
                        && state.held.is_none()
                        && let Some(note) = voice.notes.first()
                    {
                        state.held = Some(*note);
                        state.pad = index;
                        panel.play(KeyEvent::Down(*note));
                    }
                    if response.secondary_clicked() {
                        state.pad = index;
                    }
                    response.context_menu(|ui| pad_menu(ui, panel, voice));
                }
            });
        }
    });
    if let Some(note) = state.held
        && !ui.input(|i| i.pointer.any_down())
    {
        panel.play(KeyEvent::Up(note));
        state.held = None;
    }
}

/// A pad: lit by its last hit (`light`), brighter while hovered (`hover`),
/// both 0 to 1.
fn paint_pad(
    ui: &Ui,
    rect: Rect,
    voice: &Voice,
    (light, hover): (f32, f32),
    selected: bool,
    choke: usize,
    silent: bool,
) {
    let visuals = ui.visuals();
    let lit = visuals.selection.bg_fill;
    let base = visuals
        .widgets
        .inactive
        .weak_bg_fill
        .lerp_to_gamma(visuals.widgets.hovered.weak_bg_fill, hover);
    let fill = mix(base, lit, (light * 1.2).min(1.0));
    let painter = ui.painter_at(rect);
    let corner = egui::CornerRadius::same(radius::PANEL);
    painter.rect_filled(rect, corner, fill);
    let stroke = if selected {
        Stroke::new(stroke::EMPHASIS, theme::accent(visuals))
    } else {
        visuals.widgets.noninteractive.bg_stroke
    };
    painter.rect_stroke(rect, corner, stroke, egui::StrokeKind::Inside);
    let name_color = if silent {
        visuals.weak_text_color()
    } else {
        visuals.strong_text_color()
    };
    let inner = rect.shrink(space::S);
    painter.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        voice.name,
        FontId::proportional(text::SMALL),
        name_color,
    );
    if let Some(note) = voice.notes.first() {
        painter.text(
            inner.right_bottom(),
            Align2::RIGHT_BOTTOM,
            format!("{note}"),
            FontId::monospace(text::TINY),
            visuals.weak_text_color(),
        );
    }
    if choke > 0 {
        painter.text(
            inner.left_bottom(),
            Align2::LEFT_BOTTOM,
            format!("choke {}", CHOKES[choke.min(CHOKES.len() - 1)]),
            FontId::proportional(text::TINY),
            visuals.weak_text_color(),
        );
    }
}

/// A pad's right-click menu.
fn pad_menu(ui: &mut Ui, panel: &mut Panel, voice: &Voice) {
    ui.label(RichText::new(voice.name).strong());
    let range = voice.params.clone();
    if ui.button("Copy pad").clicked() {
        let values = panel.values()[range.clone()].to_vec();
        ui.data_mut(|d| d.insert_temp(clipboard_id(), Clipboard::Pad(values)));
        ui.close();
    }
    let copied = ui.data(|d| d.get_temp::<Clipboard>(clipboard_id()));
    let paste = match copied {
        Some(Clipboard::Pad(values)) => Some(values),
        _ => None,
    };
    if ui
        .add_enabled(paste.is_some(), egui::Button::new("Paste pad"))
        .clicked()
        && let Some(values) = paste
    {
        // A pad's own settings and its layers': its choke group too, but
        // not its place in the kit, which is the pad itself.
        set_all(panel, range.clone().zip(values), "Paste Pad");
        ui.close();
    }
    ui.separator();
    if ui
        .button("Reset pad")
        .on_hover_text("Back to the factory sound")
        .clicked()
    {
        let defaults: Vec<_> = range.clone().map(|i| (i, default(panel, i))).collect();
        set_all(panel, defaults, "Reset Pad");
        ui.close();
    }
    if ui
        .button("Clear pad")
        .on_hover_text("Every layer off, to build a sound from nothing")
        .clicked()
    {
        let off: Vec<_> = (0..LAYERS)
            .map(|l| (voice.layer_param(l, Layer::MODE), Mode::Off as usize as f32))
            .collect();
        set_all(panel, off, "Clear Pad");
        ui.close();
    }
}

/// Sets several parameters as one undo step.
fn set_all(panel: &mut Panel, values: impl IntoIterator<Item = (usize, f32)>, label: &str) {
    for (index, value) in values {
        panel.set(index, value);
    }
    panel.session.end_live(label);
}

fn default(panel: &Panel, index: usize) -> f32 {
    panel.params[index].default_value()
}

/// The selected pad's card: its name and note, whether its hits vary, its
/// hit drawn as tall as the height allows, and its own knobs.
fn pad_section(
    ui: &mut Ui,
    panel: &mut Panel,
    pad: usize,
    voice: &Voice,
    layer: usize,
    height: f32,
) {
    let width = 4.0 * KNOB;
    let color = theme::section::NOTE;
    chrome::section(ui, color, |ui| {
        ui.set_width(width);
        chrome::section_header(ui, voice.name, color, |ui| {
            ui.weak(format!("note {}", voice.notes[0]));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                panel
                    .toggle_at(ui, voice.param(Voice::VARY), "Vary hits")
                    .on_hover_text(
                        "Off: every hit is the same. On: each hit's noise and \
                         metal come out a little different, as on an analog machine.",
                    );
            });
        });
        let display = super::wavetable::display_height(height, 0, 3);
        preview(ui, panel, pad, voice, layer, vec2(width, display));
        let filter = FilterKind::from_value(panel.values()[voice.param(Voice::FILTER)]);
        knob_row(ui, |ui| {
            panel.knob_at(ui, voice.param(Voice::TUNE), Some("Tune"), None);
            panel.knob_at(ui, voice.param(Voice::LENGTH), Some("Length"), None);
            panel.knob_at(ui, voice.param(Voice::LEVEL), Some("Level"), None);
            panel.knob_at(ui, voice.param(Voice::PAN), Some("Pan"), None);
        });
        knob_row(ui, |ui| {
            panel.knob_at(ui, voice.param(Voice::VELOCITY), Some("Velocity"), None);
            panel.knob_at(ui, voice.param(Voice::DRIVE), Some("Drive"), None);
            panel.menu_slot(ui, voice.param(Voice::CHOKE), "Choke");
        });
        knob_row(ui, |ui| {
            panel.menu_slot(ui, voice.param(Voice::FILTER), "Filter");
            let on = filter != FilterKind::Off;
            enabled_knob(ui, panel, voice.param(Voice::CUTOFF), "Cutoff", on);
            enabled_knob(
                ui,
                panel,
                voice.param(Voice::RESONANCE),
                "Res",
                filter.resonates(),
            );
        });
    });
}

/// How tall the pad's card is in a panel `height` tall: what the height
/// leaves under the gap the view keeps below it, as
/// [`super::wavetable::display_height`] sizes its display.
fn card_height(height: f32) -> f32 {
    height - space::S
}

/// A card of the selected layer's, in one of its two rows: at least
/// `height` tall, so the cards in a row end together.
fn layer_card<R>(ui: &mut Ui, color: Color32, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    chrome::section(ui, color, |ui| {
        ui.set_min_height(height - 2.0 * space::S);
        add(ui)
    })
    .inner
}

/// Called last in a row's last card: widens the card so the row is
/// `width` wide from `left`, and returns how wide the row is without that.
fn stretch(ui: &mut Ui, left: f32, width: f32) -> f32 {
    let natural = ui.min_rect().right() + space::S - left;
    ui.set_min_width((left + width - space::S - ui.min_rect().left()).max(0.0));
    natural
}

/// A row of compact knobs, side by side with no gaps (each knob's width
/// has room around its circle).
fn knob_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        add(ui);
    });
}

/// A knob that's greyed out (and left alone) when it does nothing.
fn enabled_knob(ui: &mut Ui, panel: &mut Panel, index: usize, label: &str, enabled: bool) {
    ui.add_enabled_ui(enabled, |ui| {
        panel.knob_at(ui, index, Some(label), None);
    });
}

/// The selected pad's hit, as an envelope of its waveform; the selected
/// layer heard alone is drawn over it.
fn preview(ui: &mut Ui, panel: &Panel, pad: usize, voice: &Voice, layer: usize, size: egui::Vec2) {
    let values = panel.values();
    let mut hasher = std::hash::DefaultHasher::new();
    for index in voice.params.clone().chain([KIT_TUNE]) {
        values[index].to_bits().hash(&mut hasher);
    }
    (pad, layer, size.x as u32, panel.sample_rate.to_bits()).hash(&mut hasher);
    let key = hasher.finish();

    let id = panel.id(("preview", pad));
    let now = ui.input(|i| i.time);
    let cached: Option<Preview> = ui.data(|d| d.get_temp(id));
    let stale = cached.as_ref().is_none_or(|p| p.key != key);
    let preview = match cached {
        Some(old) if stale && now - old.made < PREVIEW_INTERVAL => {
            // Turning a knob: catch up shortly.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(PREVIEW_INTERVAL));
            old
        }
        Some(old) if !stale => old,
        _ => {
            let fresh = Preview::render(values, pad, layer, panel.sample_rate, size.x, key, now);
            ui.data_mut(|d| d.insert_temp(id, fresh.clone()));
            fresh
        }
    };

    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(space::S, space::XS));
    let visuals = ui.visuals();
    let middle = rect.center().y;
    let half = rect.height() / 2.0;
    let column = |painter: &egui::Painter, i: usize, [low, high]: [f32; 2], color: Color32| {
        // Below half a pixel either way is silence: nothing to draw.
        if (high - low) * half < 0.5 && high.abs().max(low.abs()) * half < 0.5 {
            return;
        }
        let x = rect.left() + i as f32 + 0.5;
        let top = middle - high.clamp(-1.0, 1.0) * half;
        let bottom = middle - low.clamp(-1.0, 1.0) * half;
        painter.line_segment(
            [pos2(x, top), pos2(x, bottom.max(top + 1.0))],
            Stroke::new(stroke::HAIRLINE, color),
        );
    };
    let weak = visuals.weak_text_color().gamma_multiply(0.6);
    for (i, peaks) in preview.pad.iter().enumerate() {
        column(&painter, i, *peaks, weak);
    }
    let accent = theme::accent(visuals);
    for (i, peaks) in preview.layer.iter().enumerate() {
        column(&painter, i, *peaks, accent);
    }
    let length = if preview.seconds >= 1.0 {
        format!("{:.2} s", preview.seconds)
    } else {
        format!("{:.0} ms", preview.seconds * 1000.0)
    };
    painter.text(
        rect.right_bottom(),
        Align2::RIGHT_BOTTOM,
        length,
        FontId::monospace(text::TINY),
        visuals.weak_text_color(),
    );
    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        format!("Layer {}", layer + 1),
        FontId::proportional(text::TINY),
        accent,
    );
    response.on_hover_text("The pad's hit, and its selected layer on its own");
}

/// A hit drawn as columns of lowest and highest sample.
#[derive(Debug, Clone)]
struct Preview {
    key: u64,
    /// When it was rendered, in egui's time.
    made: f64,
    seconds: f32,
    pad: Vec<[f32; 2]>,
    layer: Vec<[f32; 2]>,
}

impl Preview {
    fn render(
        values: &[f32],
        pad: usize,
        layer: usize,
        sample_rate: f32,
        width: f32,
        key: u64,
        made: f64,
    ) -> Self {
        let whole = drums::render_hit(values, pad, None, sample_rate, PREVIEW_SECONDS);
        let alone = drums::render_hit(values, pad, Some(layer), sample_rate, PREVIEW_SECONDS);
        let samples = whole
            .len()
            .max(alone.len())
            .max((0.03 * sample_rate) as usize);
        let loudest = whole
            .iter()
            .chain(&alone)
            .fold(0.0f32, |m, s| m.max(s.abs()))
            .max(1e-6);
        let columns = width.max(1.0) as usize;
        let peaks = |signal: &[f32]| -> Vec<[f32; 2]> {
            (0..columns)
                .map(|c| {
                    let range = c * samples / columns
                        ..((c + 1) * samples / columns).max(c * samples / columns + 1);
                    let chunk = signal.get(range).unwrap_or(&[]);
                    let low = chunk.iter().copied().fold(0.0f32, f32::min);
                    let high = chunk.iter().copied().fold(0.0f32, f32::max);
                    [low / loudest, high / loudest]
                })
                .collect()
        };
        Self {
            key,
            made,
            seconds: samples as f32 / sample_rate,
            pad: peaks(&whole),
            layer: peaks(&alone),
        }
    }
}

/// The selected pad's layers as chips, and the selected one's controls in
/// two rows of cards: what it plays and how its pitch moves, and how its
/// level moves and where it goes.
fn layers(ui: &mut Ui, panel: &mut Panel, voice: &Voice, selected: &mut usize, height: f32) {
    // Each row's last card stretches so both rows end where the wider one
    // did last frame; a row's own width is measured without the stretch.
    let width_id = panel.id("layer rows");
    let width: f32 = ui.data(|d| d.get_temp(width_id)).unwrap_or(0.0);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::S;
        let left = ui.cursor().left();
        let chips_width = chips(ui, panel, voice, selected);
        // The two rows share what the chips leave of the pad card's height.
        let row = ((card_height(height) - size::CONTROL - 2.0 * space::S) / 2.0)
            .max(size::CONTROL + 2.0 * knob::compact_height(knob::COMPACT) + 4.0 * space::S);
        let layer = (*selected).min(LAYERS - 1);
        let p = |offset: usize| voice.layer_param(layer, offset);
        let values = panel.values();
        let mode = Mode::from_value(values[p(Layer::MODE)]);
        let source = Source::from_value(values[p(Layer::SOURCE)]);
        let filter = FilterKind::from_value(values[p(Layer::FILTER)]);
        let bursts = values[p(Layer::BURSTS)] as usize + 1;
        if mode == Mode::Off {
            ui.multiply_opacity(0.55);
        }
        let pitched = source.pitch().is_some();

        let first = ui
            .horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::M;
                layer_card(ui, theme::section::OSC_A, row, |ui| {
                    chrome::section_header(ui, "Source", theme::section::OSC_A, |ui| {
                        panel.choice_menu_at(ui, p(Layer::SOURCE));
                    });
                    knob_row(ui, |ui| {
                        let pitch = source.pitch().unwrap_or("Pitch");
                        enabled_knob(ui, panel, p(Layer::PITCH), pitch, pitched);
                        let shape = source.shape();
                        enabled_knob(
                            ui,
                            panel,
                            p(Layer::SHAPE),
                            shape.unwrap_or("Shape"),
                            shape.is_some(),
                        );
                    });
                    knob_row(ui, |ui| {
                        enabled_knob(ui, panel, p(Layer::PHASE), "Phase", source.has_phase());
                        let fm =
                            enabled_response(ui, panel, p(Layer::FM), "FM", layer > 0 && pitched);
                        fm.on_hover_text(if layer == 0 {
                            "FM from the layer above: the first layer has none".to_owned()
                        } else {
                            format!("How far Layer {layer} bends this one's pitch")
                        });
                    });
                });
                layer_card(ui, theme::section::ENVELOPE, row, |ui| {
                    chrome::section_header(ui, "Pitch envelope", theme::section::ENVELOPE, |_| {});
                    enabled_knob(ui, panel, p(Layer::PITCH_ENV), "Amount", pitched);
                    enabled_knob(ui, panel, p(Layer::PITCH_TIME), "Time", pitched);
                });
                layer_card(ui, theme::section::FILTER, row, |ui| {
                    chrome::section_header(ui, "Filter", theme::section::FILTER, |ui| {
                        panel.choice_menu_at(ui, p(Layer::FILTER));
                    });
                    let on = filter != FilterKind::Off;
                    knob_row(ui, |ui| {
                        enabled_knob(ui, panel, p(Layer::CUTOFF), "Cutoff", on);
                        enabled_knob(ui, panel, p(Layer::RESONANCE), "Res", filter.resonates());
                        enabled_knob(ui, panel, p(Layer::FILTER_ENV), "Env", on);
                    });
                    knob_row(ui, |ui| {
                        enabled_knob(ui, panel, p(Layer::FILTER_TIME), "Time", on);
                        enabled_knob(ui, panel, p(Layer::TRACK), "Track", on);
                    });
                    stretch(ui, left, width)
                })
            })
            .inner;
        let second = ui
            .horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::M;
                layer_card(ui, theme::section::ENVELOPE, row, |ui| {
                    chrome::section_header(ui, "Amp envelope", theme::section::ENVELOPE, |_| {});
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = space::S;
                        let rows = 2.0 * knob::compact_height(knob::COMPACT) + space::XS;
                        envelope_display(ui, panel, voice, layer, vec2(ENVELOPE_WIDTH, rows));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = space::XS;
                            knob_row(ui, |ui| {
                                panel.knob_at(ui, p(Layer::DELAY), Some("Delay"), None);
                                panel.knob_at(ui, p(Layer::ATTACK), Some("Attack"), None);
                                panel.knob_at(ui, p(Layer::HOLD), Some("Hold"), None);
                                panel.knob_at(ui, p(Layer::DECAY), Some("Decay"), None);
                            });
                            knob_row(ui, |ui| {
                                panel.knob_at(ui, p(Layer::CURVE), Some("Curve"), None);
                                panel.menu_slot(ui, p(Layer::BURSTS), "Bursts");
                                enabled_knob(ui, panel, p(Layer::GAP), "Gap", bursts > 1);
                            });
                        });
                    });
                });
                // Whether the layer is heard, or only bends the next one's
                // pitch, names its output's card.
                layer_card(ui, theme::section::NEUTRAL, row, |ui| {
                    chrome::section_header(ui, "", theme::section::NEUTRAL, |ui| {
                        panel.choice_at(ui, p(Layer::MODE)).on_hover_text(
                            "Off; On; or FM Only: silent, but bending the next layer's pitch \
                             with its FM",
                        );
                    });
                    knob_row(ui, |ui| {
                        panel.knob_at(ui, p(Layer::LEVEL), Some("Level"), None);
                        panel.knob_at(ui, p(Layer::PAN), Some("Pan"), None);
                    });
                    knob_row(ui, |ui| {
                        panel.knob_at(ui, p(Layer::DRIVE), Some("Drive"), None);
                        panel.knob_at(ui, p(Layer::VELOCITY), Some("Velocity"), None);
                    });
                    stretch(ui, left, width)
                })
            })
            .inner;
        let widest = chips_width.max(first).max(second);
        if widest != width {
            ui.data_mut(|d| d.insert_temp(width_id, widest));
            ui.ctx().request_repaint();
        }
    });
}

fn enabled_response(
    ui: &mut Ui,
    panel: &mut Panel,
    index: usize,
    label: &str,
    enabled: bool,
) -> Response {
    ui.add_enabled_ui(enabled, |ui| panel.knob_at(ui, index, Some(label), None))
        .inner
}

/// The six layers as chips, each sketching its source's waveform: the dot
/// before each shows (and, clicked, flips) whether the layer plays; the
/// chip selects it. Each has a right-click menu. Returns the row's width.
fn chips(ui: &mut Ui, panel: &mut Panel, voice: &Voice, selected: &mut usize) -> f32 {
    let values = panel.values().to_vec();
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        for layer in 0..LAYERS {
            if layer > 0 {
                ui.add_space(space::S);
            }
            let mode_index = voice.layer_param(layer, Layer::MODE);
            let mode = Mode::from_value(values[mode_index]);
            let source = Source::from_value(values[voice.layer_param(layer, Layer::SOURCE)]);
            let (dot_rect, dot) =
                ui.allocate_exact_size(vec2(DOT_WIDTH, size::CONTROL), Sense::click());
            let dot_name = format!("Layer {} on", layer + 1);
            dot.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    true,
                    mode != Mode::Off,
                    &dot_name,
                )
            });
            paint_dot(ui, dot_rect.center(), mode, dot.hovered());
            if dot.clicked() {
                let flipped = if mode == Mode::Off {
                    Mode::On
                } else {
                    Mode::Off
                };
                panel.set(mode_index, flipped as usize as f32);
                panel.session.end_live(if flipped == Mode::On {
                    "Layer On"
                } else {
                    "Layer Off"
                });
                *selected = layer;
            }
            dot.on_hover_text(match mode {
                Mode::Off => "Off: click to turn on",
                Mode::On => "On: click to turn off",
                Mode::FmOnly => "FM only: click to turn off",
            });
            let color = match mode {
                Mode::On => theme::section::OSC_A,
                Mode::FmOnly => theme::section::OSC_C,
                Mode::Off => theme::section::NEUTRAL.gamma_multiply(0.7),
            };
            let name = format!("Layer {}", layer + 1);
            let text = format!("{}", layer + 1);
            let sketch = source_sketch(source);
            let chip = chrome::chip(ui, &name, &text, layer == *selected, color, &sketch);
            if chip.clicked() || chip.secondary_clicked() {
                *selected = layer;
            }
            let fm = if mode == Mode::FmOnly { " → FM" } else { "" };
            chip.on_hover_text(format!("Layer {}: {}{fm}", layer + 1, source.label()))
                .context_menu(|ui| layer_menu(ui, panel, voice, layer, selected));
        }
    });
    row.response.rect.width()
}

/// A layer's on dot: filled while it plays, a ring while it only bends the
/// next layer (FM only), faint while it's off.
fn paint_dot(ui: &Ui, center: Pos2, mode: Mode, hovered: bool) {
    let visuals = ui.visuals();
    let painter = ui.painter();
    let radius = if hovered { 5.0 } else { 4.0 };
    match mode {
        Mode::On => {
            painter.circle_filled(center, radius, theme::section::OSC_A);
        }
        Mode::FmOnly => {
            let stroke = Stroke::new(stroke::EMPHASIS, theme::section::OSC_C);
            painter.circle_stroke(center, radius, stroke);
        }
        Mode::Off => {
            let stroke = Stroke::new(stroke::HAIRLINE, visuals.weak_text_color());
            painter.circle_stroke(center, radius, stroke);
        }
    }
}

/// A cycle or two of what `source` sounds like, for its layer's chip
/// (heights from 0 to 1): the waves' shapes, the noises as scribbles from
/// smooth (brown) to spiky (violet), crackle's clicks, digital noise's
/// steps, an impulse's single cycle.
fn source_sketch(source: Source) -> Vec<f32> {
    use std::f32::consts::TAU;
    let wave = |cycles: f32, at: &dyn Fn(f32) -> f32| -> Vec<f32> {
        (0..SKETCH_POINTS)
            .map(|i| 0.5 + 0.45 * at(i as f32 / (SKETCH_POINTS - 1) as f32 * cycles))
            .collect()
    };
    // The same scribble every frame, from a fixed seed.
    let mut seed = 0x2545_f491_u32;
    let raw: Vec<f32> = (0..SKETCH_POINTS)
        .map(|_| dsp::noise::bipolar(dsp::xorshift32(&mut seed)))
        .collect();
    let scribble =
        |values: Vec<f32>| -> Vec<f32> { values.iter().map(|v| 0.5 + 0.45 * v).collect() };
    let smoothed = |keep: f32| -> Vec<f32> {
        let mut level = 0.0;
        raw.iter()
            .map(|x| {
                level = level * keep + x * (1.0 - keep);
                level / (1.0 - keep).sqrt()
            })
            .collect()
    };
    let square = |t: f32| if t.fract() < 0.5 { 1.0 } else { -1.0 };
    match source {
        Source::Sine => wave(1.0, &|t| (TAU * t).sin()),
        Source::Triangle => wave(1.0, &|t| 1.0 - 4.0 * (t.fract() - 0.5).abs()),
        Source::Saw => wave(1.5, &|t| 1.0 - 2.0 * t.fract()),
        Source::Square => wave(1.5, &|t| 0.8 * square(t)),
        Source::Metal => wave(1.5, &|t| {
            (square(t) + square(1.48 * t) + square(2.12 * t)) / 3.0
        }),
        Source::Membrane => wave(2.5, &|t| (TAU * t).sin() * (-0.9 * t).exp()),
        Source::Bell => wave(1.0, &|t| {
            ((TAU * t).sin() + 0.6 * (TAU * 2.76 * t).sin() + 0.4 * (TAU * 5.4 * t).sin()) / 2.0
        }),
        Source::Impulse => wave(1.0, &|t| if t < 0.5 { (TAU * 2.0 * t).sin() } else { 0.0 }),
        Source::White | Source::Blue => scribble(raw.iter().map(|x| 0.9 * x).collect()),
        Source::Violet => scribble(
            std::iter::once(0.0)
                .chain(raw.windows(2).map(|w| (w[1] - w[0]) / 2.0))
                .collect(),
        ),
        Source::Pink => scribble(smoothed(0.5)),
        Source::Brown => scribble(smoothed(0.8)),
        Source::Crackle => (0..SKETCH_POINTS)
            .map(|i| if matches!(i, 4 | 13 | 19) { 0.95 } else { 0.5 })
            .collect(),
        Source::Digital => scribble(
            raw.chunks(4)
                .flat_map(|step| std::iter::repeat_n((step[0] * 2.0).round() / 2.0, step.len()))
                .collect(),
        ),
    }
}

/// The selected layer's level through its hit, as its amp envelope's
/// knobs set it: the delay, each burst's attack and hold, and the decay
/// from exponential to straight. Its length is written in the corner.
fn envelope_display(ui: &mut Ui, panel: &Panel, voice: &Voice, layer: usize, size: egui::Vec2) {
    let values = panel.values();
    let p = |offset: usize| values[voice.layer_param(layer, offset)];
    let shape = AmpShape {
        delay: p(Layer::DELAY),
        attack: p(Layer::ATTACK),
        hold: p(Layer::HOLD),
        decay: p(Layer::DECAY),
        curve: p(Layer::CURVE),
        bursts: p(Layer::BURSTS) as usize + 1,
        gap: p(Layer::GAP),
    };
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(space::S, space::S));
    let color = theme::section::ENVELOPE;
    let levels = shape.levels(rect.width().max(2.0) as usize);
    let last = (levels.len() - 1).max(1) as f32;
    let points: Vec<Pos2> = levels
        .iter()
        .enumerate()
        .map(|(i, level)| {
            pos2(
                rect.left() + i as f32 / last * rect.width(),
                rect.bottom() - level * rect.height(),
            )
        })
        .collect();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        painter.add(egui::Shape::convex_polygon(
            vec![a, b, pos2(b.x, rect.bottom()), pos2(a.x, rect.bottom())],
            color.gamma_multiply(0.15),
            Stroke::NONE,
        ));
    }
    painter.add(egui::Shape::line(
        points,
        Stroke::new(stroke::EMPHASIS, color),
    ));
    let seconds = shape.length() / 1000.0;
    let length = if seconds >= 1.0 {
        format!("{seconds:.2} s")
    } else {
        format!("{:.0} ms", seconds * 1000.0)
    };
    painter.text(
        response.rect.right_top() + vec2(-DISPLAY_INSET.x, DISPLAY_INSET.y),
        Align2::RIGHT_TOP,
        length,
        FontId::monospace(text::TINY),
        ui.visuals().weak_text_color(),
    );
    response.on_hover_text("The layer's level through its hit");
}

/// A layer's amp envelope, in milliseconds, for [`envelope_display`].
#[derive(Debug, Clone, Copy)]
struct AmpShape {
    delay: f32,
    attack: f32,
    hold: f32,
    decay: f32,
    /// From exponential (0) to straight (1).
    curve: f32,
    bursts: usize,
    gap: f32,
}

impl AmpShape {
    /// From the hit to where the last burst has died away.
    fn length(&self) -> f32 {
        self.delay + (self.bursts - 1) as f32 * self.gap + self.attack + self.hold + self.decay
    }

    /// The level at `count` evenly spaced moments of the hit, 0 to 1. The
    /// synth's envelope stepped coarsely: each burst strikes again from
    /// wherever the level is, the decay falls 60 dB over its time, and
    /// the curve mixes that with a straight line down.
    fn levels(&self, count: usize) -> Vec<f32> {
        let count = count.max(2);
        let step = self.length().max(1.0) / (count - 1) as f32;
        let exp_mult = (-(1000.0f32.ln()) * step / self.decay.max(step)).exp();
        let lin_step = step / self.decay.max(step);
        let curve = self.curve.clamp(0.0, 1.0);
        let strikes: Vec<f32> = (0..self.bursts)
            .map(|b| self.delay + b as f32 * self.gap)
            .collect();
        let (mut level, mut exp, mut lin) = (0.0f32, 0.0f32, 0.0f32);
        let mut held = 0.0;
        let mut stage = 0; // 0 waiting, 1 attack, 2 hold, 3 decay
        let mut struck = 0;
        (0..count)
            .map(|i| {
                let t = i as f32 * step;
                while struck < strikes.len() && t >= strikes[struck] {
                    struck += 1;
                    stage = 1;
                }
                match stage {
                    1 => {
                        level = (level + step / self.attack.max(step)).min(1.0);
                        if level >= 1.0 {
                            stage = 2;
                            held = 0.0;
                        }
                    }
                    2 => {
                        held += step;
                        if held >= self.hold {
                            stage = 3;
                            exp = 1.0;
                            lin = 1.0;
                        }
                    }
                    3 => {
                        exp *= exp_mult;
                        lin = (lin - lin_step).max(0.0);
                        level = exp + curve * (lin - exp);
                    }
                    _ => {}
                }
                level
            })
            .collect()
    }
}

/// A layer tab's right-click menu.
fn layer_menu(ui: &mut Ui, panel: &mut Panel, voice: &Voice, layer: usize, selected: &mut usize) {
    ui.label(RichText::new(format!("{} Layer {}", voice.name, layer + 1)).strong());
    let range = voice.layer(layer);
    if ui.button("Copy layer").clicked() {
        let values = panel.values()[range.clone()].to_vec();
        ui.data_mut(|d| d.insert_temp(clipboard_id(), Clipboard::Layer(values)));
        ui.close();
    }
    let copied = ui.data(|d| d.get_temp::<Clipboard>(clipboard_id()));
    let paste = match copied {
        Some(Clipboard::Layer(values)) => Some(values),
        _ => None,
    };
    if ui
        .add_enabled(paste.is_some(), egui::Button::new("Paste layer"))
        .clicked()
        && let Some(values) = paste
    {
        set_all(panel, range.clone().zip(values), "Paste Layer");
        ui.close();
    }
    ui.separator();
    for (label, other) in [
        ("Move left", layer.checked_sub(1)),
        ("Move right", Some(layer + 1)),
    ] {
        let other = other.filter(|&o| o < LAYERS);
        if ui
            .add_enabled(other.is_some(), egui::Button::new(label))
            .clicked()
            && let Some(other) = other
        {
            swap_layers(panel, voice.layer(layer), voice.layer(other));
            *selected = other;
            ui.close();
        }
    }
    ui.separator();
    if ui
        .button("Reset layer")
        .on_hover_text("Back to the factory settings")
        .clicked()
    {
        let defaults: Vec<_> = range.clone().map(|i| (i, default(panel, i))).collect();
        set_all(panel, defaults, "Reset Layer");
        ui.close();
    }
}

/// Swaps two layers' settings: the order matters, since each modulates
/// the next.
fn swap_layers(panel: &mut Panel, a: Range<usize>, b: Range<usize>) {
    let values = panel.values().to_vec();
    let swapped: Vec<_> = a
        .clone()
        .zip(b.clone().map(|i| values[i]))
        .chain(b.zip(a.map(|i| values[i])))
        .collect();
    set_all(panel, swapped, "Move Layer");
}

/// `a` moved toward `b` by `t`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}
