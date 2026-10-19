//! The Drum Synth's panel: sixteen pads that light as they play (press one
//! to hear it and edit it); the selected pad's hit drawn, with its selected
//! layer picked out of it; the pad's own knobs; its six layers as tabs and
//! the selected layer's controls; and the kit's knobs. Pads and layers copy,
//! paste, reset and clear from their right-click menus.

use std::hash::{Hash, Hasher};
use std::ops::Range;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use instruments::builtin::drums::{
    self, CHOKES, FilterKind, KIT_DRIVE, KIT_LEVEL, KIT_TUNE, LAYERS, Layer, Mode, Source, Voice,
};

use super::Panel;
use super::panel::{KNOB_TEXT, display, knob_slot};
use crate::theme::{self, radius, size, space, stroke, text};
use crate::widgets::keys::KeyEvent;
use crate::widgets::knob;

/// How fast a pad's light fades after a hit, per second.
const FADE_PER_SECOND: f32 = 4.0;

const PAD_COLUMNS: usize = 4;

/// The longest hit the display draws.
const PREVIEW_SECONDS: f32 = 3.0;
/// How often the display may redraw the hit while a knob is turning.
const PREVIEW_INTERVAL: f64 = 0.08;
const PREVIEW_HEIGHT: f32 = 48.0;

/// Small labels over groups of knobs: as tall as a knob's name.
const SECTION_TEXT: f32 = knob::LABEL_HEIGHT;
/// The gap between pads.
const PAD_GAP: f32 = space::S;
/// The layers' tabs.
const TAB_HEIGHT: f32 = size::BAR;

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

    // Two rows of knobs under a strip of tabs and a header, beside the pads.
    let height = ui.available_height();
    let above = TAB_HEIGHT + size::CONTROL + space::L;
    panel.knob_size = ((height - 2.0 * (KNOB_TEXT + SECTION_TEXT) - above) / 2.0).clamp(24.0, 32.0);
    let rows = (drums::voices().len() / PAD_COLUMNS) as f32;
    let pad_size = ((height - (rows - 1.0) * PAD_GAP) / rows).clamp(36.0, 54.0);

    ui.horizontal_top(|ui| {
        pads(ui, panel, voices, &mut state, pad_size);
        ui.separator();
        let voice = &voices[state.pad];
        let layer = state.layers[state.pad].min(LAYERS - 1);
        pad_controls(ui, panel, state.pad, voice, layer);
        ui.separator();
        layers(ui, panel, voice, &mut state.layers[state.pad]);
        ui.separator();
        ui.vertical(|ui| {
            // Never wrapped: in a scrolled chain the last column's room
            // can run out, which would stack the letters.
            ui.add(egui::Label::new(RichText::new("Kit").strong()).extend());
            for index in [KIT_LEVEL, KIT_TUNE, KIT_DRIVE] {
                let label = panel.params[index].name.trim_start_matches("Kit ");
                panel.knob_at(ui, index, Some(label), None);
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(state_id, state));
}

/// The pads, four by four; pressing one plays it and selects it.
fn pads(ui: &mut Ui, panel: &mut Panel, voices: &[Voice], state: &mut State, size: f32) {
    let values = panel.values().to_vec();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::splat(PAD_GAP);
        for (row, row_voices) in voices.chunks(PAD_COLUMNS).enumerate() {
            ui.horizontal(|ui| {
                for (column, voice) in row_voices.iter().enumerate() {
                    let index = row * PAD_COLUMNS + column;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(size * 1.2, size), Sense::click_and_drag());
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

/// The selected pad's name, its hit drawn, and its own knobs.
fn pad_controls(ui: &mut Ui, panel: &mut Panel, pad: usize, voice: &Voice, layer: usize) {
    let width = 5.0 * knob::width(panel.knob_size);
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            ui.label(RichText::new(voice.name).strong());
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
        preview(ui, panel, pad, voice, layer, vec2(width, PREVIEW_HEIGHT));
        let row = |ui: &mut Ui, panel: &mut Panel, knobs: &[(usize, &str)]| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for &(offset, label) in knobs {
                    panel.knob_at(ui, voice.param(offset), Some(label), None);
                }
            });
        };
        row(
            ui,
            panel,
            &[
                (Voice::TUNE, "Tune"),
                (Voice::LENGTH, "Length"),
                (Voice::LEVEL, "Level"),
                (Voice::PAN, "Pan"),
                (Voice::DRIVE, "Drive"),
            ],
        );
        let filter = FilterKind::from_value(panel.values()[voice.param(Voice::FILTER)]);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            panel.knob_at(ui, voice.param(Voice::VELOCITY), Some("Velocity"), None);
            menu_slot(ui, panel, voice.param(Voice::CHOKE), "Choke", true);
            menu_slot(ui, panel, voice.param(Voice::FILTER), "Filter", true);
            enabled_knob(
                ui,
                panel,
                voice.param(Voice::CUTOFF),
                "Cutoff",
                filter != FilterKind::Off,
            );
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

/// A knob that's greyed out (and left alone) when it does nothing.
fn enabled_knob(ui: &mut Ui, panel: &mut Panel, index: usize, label: &str, enabled: bool) {
    ui.add_enabled_ui(enabled, |ui| {
        panel.knob_at(ui, index, Some(label), None);
    });
}

/// A choice as a menu button in a row of knobs, as wide as a knob (see
/// [`knob_slot`]).
fn menu_slot(ui: &mut Ui, panel: &mut Panel, index: usize, label: &str, enabled: bool) {
    let size = vec2(knob::width(panel.knob_size), panel.knob_size + KNOB_TEXT);
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
        ui.set_min_size(size);
        ui.add_enabled_ui(enabled, |ui| {
            knob_slot(ui, panel.knob_size, label, |ui| {
                panel.choice_menu_at(ui, index);
            });
        });
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

/// The selected pad's layers as tabs, and the selected one's controls.
fn layers(ui: &mut Ui, panel: &mut Panel, voice: &Voice, selected: &mut usize) {
    ui.vertical(|ui| {
        tabs(ui, panel, voice, selected);
        let layer = (*selected).min(LAYERS - 1);
        let p = |offset: usize| voice.layer_param(layer, offset);
        let values = panel.values();
        let mode = Mode::from_value(values[p(Layer::MODE)]);
        let source = Source::from_value(values[p(Layer::SOURCE)]);
        let filter = FilterKind::from_value(values[p(Layer::FILTER)]);
        let bursts = values[p(Layer::BURSTS)] as usize + 1;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            panel.choice_at(ui, p(Layer::MODE)).on_hover_text(
                "Off; On; or FM Only: silent, but bending the next layer's pitch \
                 with its FM",
            );
            ui.separator();
            ui.weak("Source");
            panel.choice_menu_at(ui, p(Layer::SOURCE));
        });
        if mode == Mode::Off {
            ui.multiply_opacity(0.55);
        }

        let pitched = source.pitch().is_some();
        ui.horizontal_top(|ui| {
            section(ui, "Source", |ui| {
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
                enabled_knob(ui, panel, p(Layer::PHASE), "Phase", source.has_phase());
                let fm = enabled_response(ui, panel, p(Layer::FM), "FM", layer > 0 && pitched);
                fm.on_hover_text(if layer == 0 {
                    "FM from the layer above: the first layer has none".to_owned()
                } else {
                    format!("How far Layer {layer} bends this one's pitch")
                });
            });
            divider(ui, panel);
            section(ui, "Pitch envelope", |ui| {
                enabled_knob(ui, panel, p(Layer::PITCH_ENV), "Amount", pitched);
                enabled_knob(ui, panel, p(Layer::PITCH_TIME), "Time", pitched);
            });
            divider(ui, panel);
            section(ui, "Output", |ui| {
                panel.knob_at(ui, p(Layer::DRIVE), Some("Drive"), None);
                panel.knob_at(ui, p(Layer::LEVEL), Some("Level"), None);
                panel.knob_at(ui, p(Layer::PAN), Some("Pan"), None);
                panel.knob_at(ui, p(Layer::VELOCITY), Some("Velocity"), None);
            });
        });
        ui.horizontal_top(|ui| {
            section(ui, "Envelope", |ui| {
                panel.knob_at(ui, p(Layer::DELAY), Some("Delay"), None);
                panel.knob_at(ui, p(Layer::ATTACK), Some("Attack"), None);
                panel.knob_at(ui, p(Layer::HOLD), Some("Hold"), None);
                panel.knob_at(ui, p(Layer::DECAY), Some("Decay"), None);
                panel.knob_at(ui, p(Layer::CURVE), Some("Curve"), None);
                menu_slot(ui, panel, p(Layer::BURSTS), "Bursts", true);
                enabled_knob(ui, panel, p(Layer::GAP), "Gap", bursts > 1);
            });
            divider(ui, panel);
            section(ui, "Filter", |ui| {
                menu_slot(ui, panel, p(Layer::FILTER), "Type", true);
                let on = filter != FilterKind::Off;
                enabled_knob(ui, panel, p(Layer::CUTOFF), "Cutoff", on);
                enabled_knob(ui, panel, p(Layer::RESONANCE), "Res", filter.resonates());
                enabled_knob(ui, panel, p(Layer::FILTER_ENV), "Env", on);
                enabled_knob(ui, panel, p(Layer::FILTER_TIME), "Time", on);
                enabled_knob(ui, panel, p(Layer::TRACK), "Track", on);
            });
        });
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

/// A line between groups in a row of knobs, as tall as the row. (A plain
/// separator in a row takes all the height left, which grows the panel.)
fn divider(ui: &mut Ui, panel: &Panel) {
    let height = SECTION_TEXT + panel.knob_size + KNOB_TEXT;
    ui.add_sized(
        vec2(space::M, height),
        egui::Separator::default().vertical(),
    );
}

/// Knobs in a row under a small heading.
fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_sized(
            vec2(knob::width(24.0), SECTION_TEXT),
            egui::Label::new(RichText::new(title).size(text::SMALL).weak()).extend(),
        );
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            add(ui);
        });
    });
}

/// The six layers as tabs: the dot shows (and, clicked, flips) whether the
/// layer plays; the rest selects it. Each has a right-click menu.
fn tabs(ui: &mut Ui, panel: &mut Panel, voice: &Voice, selected: &mut usize) {
    let values = panel.values().to_vec();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        for layer in 0..LAYERS {
            let mode_index = voice.layer_param(layer, Layer::MODE);
            let mode = Mode::from_value(values[mode_index]);
            let source = Source::from_value(values[voice.layer_param(layer, Layer::SOURCE)]);
            let name = short_source(source);
            let width = 22.0 + 7.0 * name.len() as f32;
            let (rect, response) =
                ui.allocate_exact_size(vec2(width.max(64.0), TAB_HEIGHT), Sense::click());
            let dot_rect =
                Rect::from_center_size(rect.left_center() + vec2(10.0, 0.0), vec2(14.0, 14.0));
            let dot = ui.interact(dot_rect, response.id.with("dot"), Sense::click());
            let tab_name = format!("Layer {}", layer + 1);
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    true,
                    layer == *selected,
                    &tab_name,
                )
            });
            let dot_name = format!("Layer {} on", layer + 1);
            dot.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    true,
                    mode != Mode::Off,
                    &dot_name,
                )
            });
            let hover = theme::hover(ui, &response);
            paint_tab(
                ui,
                rect,
                dot_rect.center(),
                layer,
                name,
                mode,
                (layer == *selected, hover),
                dot.hovered(),
            );
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
            if response.clicked() || response.secondary_clicked() {
                *selected = layer;
            }
            response
                .on_hover_text(format!("Layer {}: {}", layer + 1, source.label()))
                .context_menu(|ui| layer_menu(ui, panel, voice, layer, selected));
        }
    });
}

/// A source's name, short enough for a tab.
fn short_source(source: Source) -> &'static str {
    source.label().trim_end_matches(" Noise")
}

/// A layer's tab, `selected` or not and as far hovered as the fade says
/// (0 to 1).
#[allow(clippy::too_many_arguments)]
fn paint_tab(
    ui: &Ui,
    rect: Rect,
    dot: Pos2,
    layer: usize,
    name: &str,
    mode: Mode,
    (selected, hover): (bool, f32),
    dot_hovered: bool,
) {
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    let fill = if selected {
        visuals.widgets.active.weak_bg_fill
    } else {
        visuals
            .widgets
            .inactive
            .weak_bg_fill
            .lerp_to_gamma(visuals.widgets.hovered.weak_bg_fill, hover)
    };
    let accent = theme::accent(visuals);
    let corner = egui::CornerRadius::same(radius::CONTROL);
    painter.rect_filled(rect, corner, fill);
    if selected {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(stroke::HAIRLINE, accent),
            egui::StrokeKind::Inside,
        );
    }
    let radius = if dot_hovered { 5.0 } else { 4.0 };
    match mode {
        Mode::On => {
            painter.circle_filled(dot, radius, accent);
        }
        Mode::FmOnly => {
            painter.circle_stroke(dot, radius, Stroke::new(stroke::EMPHASIS, accent));
        }
        Mode::Off => {
            let stroke = Stroke::new(stroke::HAIRLINE, visuals.weak_text_color());
            painter.circle_stroke(dot, radius, stroke);
        }
    }
    let text_color = if mode == Mode::Off {
        visuals.weak_text_color()
    } else {
        visuals.strong_text_color()
    };
    let label = match mode {
        Mode::FmOnly => format!("{} {name} → FM", layer + 1),
        _ => format!("{} {name}", layer + 1),
    };
    painter.text(
        pos2(dot.x + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(text::SMALL),
        text_color,
    );
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
