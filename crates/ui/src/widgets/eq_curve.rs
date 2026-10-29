//! A parametric EQ drawn as its frequency response over the sound's
//! spectrum (before it, faint, and after it), edited with a handle per
//! band, as in FabFilter Pro-Q and FL Studio's Parametric EQ 2:
//!
//! - Double-click an empty spot to add a band there.
//! - Drag a handle sideways for frequency and up or down for gain.
//! - The mouse wheel over a handle changes its Q.
//! - Double-click a handle to flatten its gain.
//! - Right-click a handle for its type, dynamic mode, or to remove it.
//!
//! A band exists while its `on` parameter is on. Dynamic bands show the
//! range their gain can move through, and where it is right now.

use std::sync::Arc;

use effects::eq::{self, BandType};
use eframe::egui::{self, Align2, Color32, FontId, Id, Rect, Sense, Shape, Stroke, pos2, vec2};
use instruments::ParamDescriptor;

use crate::widgets::spectrum::{Axes, Spectrum, SpectrumSettings};

const PAD: f32 = 8.0;
const LOW_HZ: f32 = 20.0;
const HIGH_HZ: f32 = 20_000.0;
/// The gain range shown, either way.
const RANGE_DB: f32 = 24.0;
const HANDLE_RADIUS: f32 = 7.0;

/// The curves as last drawn, kept in egui's memory between frames: the
/// response at every pixel column is a sum over all the bands, which only
/// changes when a setting, a dynamic band's gain or the display's size
/// does, not every frame.
#[derive(Default)]
struct Curves {
    /// What they were drawn from: every value, offset and dimension, as bits.
    key: Vec<u32>,
    /// The selected band's shape, filled.
    fill: Option<egui::Mesh>,
    /// The response as set, and with the dynamic bands where they are now.
    set: Vec<egui::Pos2>,
    live: Vec<egui::Pos2>,
}

/// A color per band, repeating, so a handle and its controls match.
pub const BAND_COLORS: [Color32; 8] = [
    Color32::from_rgb(235, 110, 110),
    Color32::from_rgb(235, 175, 90),
    Color32::from_rgb(220, 220, 100),
    Color32::from_rgb(120, 210, 130),
    Color32::from_rgb(90, 200, 210),
    Color32::from_rgb(100, 160, 240),
    Color32::from_rgb(180, 140, 240),
    Color32::from_rgb(230, 130, 200),
];

pub fn band_color(band: usize) -> Color32 {
    BAND_COLORS[band % BAND_COLORS.len()]
}

/// Where each of a band's parameters is, by id (`b3_freq`), so the editor
/// works with whatever parameters this EQ has.
#[derive(Debug, Clone, Copy, Default)]
pub struct BandParams {
    pub on: Option<usize>,
    pub kind: Option<usize>,
    pub freq: Option<usize>,
    pub gain: Option<usize>,
    pub q: Option<usize>,
    pub dynamic: Option<usize>,
    pub range: Option<usize>,
}

impl BandParams {
    pub fn find(params: &[ParamDescriptor], band: usize) -> BandParams {
        let find = |what: &str| {
            let id = format!("b{}_{what}", band + 1);
            params.iter().position(|p| p.id == id)
        };
        BandParams {
            on: find("on"),
            kind: find("type"),
            freq: find("freq"),
            gain: find("gain"),
            q: find("q"),
            dynamic: find("dyn"),
            range: find("range"),
        }
    }
}

/// Every band's parameter positions.
pub fn bands(params: &[ParamDescriptor]) -> Vec<BandParams> {
    (0..eq::BANDS)
        .map(|b| BandParams::find(params, b))
        .collect()
}

/// What changed this frame, for the caller to apply.
#[derive(Debug, Default)]
pub struct EqResponse {
    /// `(parameter index, new value)` pairs.
    pub changes: Vec<(usize, f32)>,
    /// A drag ended, or a one-off change (wheel, reset, menu) happened.
    pub finished: bool,
    /// The band last clicked, dragged or added, whose controls the caller
    /// shows.
    pub selected: usize,
}

/// The spectra to draw behind the curve.
pub struct Spectra<'a> {
    pub before: &'a Spectrum,
    pub after: &'a Spectrum,
    pub settings: &'a SpectrumSettings,
}

pub struct EqCurve<'a> {
    id: Id,
    params: &'a [ParamDescriptor],
    values: &'a [f32],
    sample_rate: f32,
    size: egui::Vec2,
    /// Each band's live dynamic gain offset, in dB.
    offsets: &'a [f32],
    spectra: Option<Spectra<'a>>,
    selected: usize,
}

impl<'a> EqCurve<'a> {
    /// `values` are every EQ parameter, in its descriptor's order.
    pub fn new(id: Id, params: &'a [ParamDescriptor], values: &'a [f32], sample_rate: f32) -> Self {
        EqCurve {
            id,
            params,
            values,
            sample_rate,
            size: vec2(360.0, 150.0),
            offsets: &[],
            spectra: None,
            selected: 0,
        }
    }

    pub fn size(mut self, size: egui::Vec2) -> Self {
        self.size = size;
        self
    }

    pub fn offsets(mut self, offsets: &'a [f32]) -> Self {
        self.offsets = offsets;
        self
    }

    pub fn spectra(mut self, spectra: Spectra<'a>) -> Self {
        self.spectra = Some(spectra);
        self
    }

    pub fn selected(mut self, band: usize) -> Self {
        self.selected = band;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> EqResponse {
        let mut out = EqResponse {
            selected: self.selected,
            ..EqResponse::default()
        };
        let (rect, background) = ui.allocate_exact_size(self.size, Sense::click());
        #[cfg(test)]
        ui.ctx()
            .data_mut(|d| d.insert_temp(self.id.with("rect"), rect));
        let area = rect.shrink(PAD);
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        painter.rect_stroke(
            rect,
            4.0,
            visuals.widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let axes = Axes {
            rect: area,
            low_hz: LOW_HZ,
            high_hz: HIGH_HZ,
            floor_db: -RANGE_DB,
            ceiling_db: RANGE_DB,
        };
        // Fewer level lines when there's little room for their labels.
        axes.paint_grid(
            &painter,
            &visuals,
            if area.height() < 160.0 { 12.0 } else { 6.0 },
        );
        painter.hline(
            area.x_range(),
            axes.y(0.0),
            Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.35)),
        );

        if let Some(spectra) = &self.spectra {
            let behind = Axes {
                floor_db: spectra.settings.floor_db,
                ceiling_db: spectra.settings.ceiling_db,
                ..axes
            };
            let weak = visuals.weak_text_color();
            spectra.before.paint(
                &painter,
                &behind,
                spectra.settings,
                self.sample_rate,
                Stroke::new(1.0, weak.gamma_multiply(0.35)),
                Color32::TRANSPARENT,
            );
            let tint = crate::theme::accent(&visuals);
            spectra.after.paint(
                &painter,
                &behind,
                spectra.settings,
                self.sample_rate,
                Stroke::new(1.0, tint.gamma_multiply(0.45)),
                tint.gamma_multiply(0.10),
            );
        }

        let bands = bands(self.params);
        let value = |index: Option<usize>| index.and_then(|i| self.values.get(i)).copied();
        let on = |band: &BandParams| value(band.on).unwrap_or(0.0) >= 0.5;
        let dynamic = |band: &BandParams| value(band.dynamic).unwrap_or(0.0) >= 0.5;

        // Dynamic bands move: the live curve has their offsets added.
        let mut live = self.values.to_vec();
        let mut moving = false;
        for (i, band) in bands.iter().enumerate() {
            let offset = self.offsets.get(i).copied().unwrap_or(0.0);
            if on(band)
                && dynamic(band)
                && offset != 0.0
                && let Some(gain) = band.gain
            {
                live[gain] += offset;
                moving = true;
            }
        }
        let accent = crate::theme::accent(&visuals);
        let key: Vec<u32> = self
            .values
            .iter()
            .chain(&live)
            .chain(&[area.left(), area.top(), area.right(), area.bottom()])
            .chain(&[self.sample_rate, out.selected as f32])
            .map(|v| v.to_bits())
            .chain([u32::from(bands.get(out.selected).is_some_and(on))])
            .collect();
        let curves_id = self.id.with("curves");
        let cached: Option<Arc<Curves>> = ui.data(|d| d.get_temp(curves_id));
        let curves = match cached {
            Some(curves) if curves.key == key => curves,
            _ => {
                let curve = |values: &[f32]| -> Vec<egui::Pos2> {
                    let mut points = Vec::with_capacity(area.width() as usize + 1);
                    let mut x = area.left();
                    while x <= area.right() {
                        points.push(pos2(
                            x,
                            axes.y(eq::response_db(values, axes.hz(x), self.sample_rate)),
                        ));
                        x += 1.0;
                    }
                    points
                };
                // The selected band's own shape, filled in its color.
                let fill = bands.get(out.selected).filter(|b| on(b)).map(|_| {
                    let mut alone = self.values.to_vec();
                    for (i, other) in bands.iter().enumerate() {
                        if i != out.selected
                            && let Some(index) = other.on
                        {
                            alone[index] = 0.0;
                        }
                    }
                    let color = band_color(out.selected);
                    let mut fill = egui::Mesh::default();
                    let zero = axes.y(0.0);
                    let mut x = area.left();
                    while x <= area.right() {
                        let y = axes.y(eq::response_db(&alone, axes.hz(x), self.sample_rate));
                        fill.add_colored_rect(
                            Rect::from_min_max(pos2(x, y.min(zero)), pos2(x + 2.0, y.max(zero))),
                            color.gamma_multiply(0.16),
                        );
                        x += 2.0;
                    }
                    fill
                });
                let curves = Arc::new(Curves {
                    key,
                    fill,
                    set: if moving {
                        curve(self.values)
                    } else {
                        Vec::new()
                    },
                    live: curve(&live),
                });
                ui.data_mut(|d| d.insert_temp(curves_id, Arc::clone(&curves)));
                curves
            }
        };
        if let Some(fill) = &curves.fill {
            painter.add(fill.clone());
        }
        if moving {
            painter.add(Shape::line(
                curves.set.clone(),
                Stroke::new(1.0, accent.gamma_multiply(0.4)),
            ));
        }
        painter.add(Shape::line(curves.live.clone(), Stroke::new(2.0, accent)));

        // Double-click on an empty spot adds a band there.
        if background.double_clicked()
            && let Some(pos) = background.interact_pointer_pos()
            && let Some(free) = bands.iter().position(|b| !on(b))
        {
            let band = bands[free];
            let mut set = |index: Option<usize>, v: f32| {
                if let Some(index) = index {
                    out.changes.push((index, v));
                }
            };
            set(band.kind, type_index(BandType::Bell));
            set(band.freq, axes.hz(pos.x));
            set(band.gain, axes.db(pos.y).clamp(-RANGE_DB, RANGE_DB));
            set(band.q, 1.0);
            set(band.on, 1.0);
            out.selected = free;
            out.finished = true;
        }

        // Handles: the selected band last, on top.
        let mut order: Vec<usize> = (0..bands.len())
            .filter(|b| *b != out.selected && on(&bands[*b]))
            .collect();
        if bands.get(out.selected).is_some_and(on) {
            order.push(out.selected);
        }
        for band_index in order {
            let band = bands[band_index];
            let kind = BandType::from_index(value(band.kind).unwrap_or(2.0));
            let hz = value(band.freq).unwrap_or(1000.0);
            let gain = if kind.has_gain() {
                value(band.gain).unwrap_or(0.0)
            } else {
                0.0
            };
            let center = pos2(axes.x(hz), axes.y(gain));
            let color = band_color(band_index);
            if dynamic(&band) {
                let range = value(band.range).unwrap_or(0.0);
                let offset = self.offsets.get(band_index).copied().unwrap_or(0.0);
                let span = axes.y(gain)..=axes.y(gain + range);
                painter.line_segment(
                    [pos2(center.x, *span.start()), pos2(center.x, *span.end())],
                    Stroke::new(6.0, color.gamma_multiply(0.3)),
                );
                painter.circle_stroke(
                    pos2(center.x, axes.y(gain + offset)),
                    HANDLE_RADIUS - 2.0,
                    Stroke::new(1.5, color),
                );
            }
            let hit = Rect::from_center_size(center, vec2(18.0, 18.0));
            let response = ui.interact(
                hit,
                self.id.with(("band", band_index)),
                Sense::click_and_drag(),
            );
            let selected = band_index == out.selected;
            let active = response.hovered() || response.dragged() || selected;
            painter.circle(
                center,
                if active {
                    HANDLE_RADIUS + 1.0
                } else {
                    HANDLE_RADIUS
                },
                color,
                Stroke::new(1.0, Color32::from_black_alpha(180)),
            );
            painter.text(
                center,
                Align2::CENTER_CENTER,
                format!("{}", band_index + 1),
                FontId::proportional(crate::theme::text::TINY),
                Color32::from_black_alpha(220),
            );
            if selected {
                painter.circle_stroke(center, HANDLE_RADIUS + 3.5, Stroke::new(1.0, color));
            }
            if response.hovered() {
                let mut text = format!(
                    "{}  {}  {}",
                    band_name(band_index),
                    type_name(kind),
                    format_hz(hz)
                );
                if kind.has_gain() {
                    text = format!("{text}  {gain:+.1} dB");
                }
                response.clone().on_hover_text(text);
            }

            if response.clicked() || response.drag_started() {
                out.selected = band_index;
            }
            if response.dragged()
                && let Some(pos) = response.interact_pointer_pos()
            {
                if let Some(freq) = band.freq {
                    out.changes
                        .push((freq, axes.hz(pos.x).clamp(LOW_HZ, HIGH_HZ)));
                }
                if kind.has_gain()
                    && let Some(index) = band.gain
                {
                    out.changes
                        .push((index, axes.db(pos.y).clamp(-RANGE_DB, RANGE_DB)));
                }
            }
            if response.drag_stopped() {
                out.finished = true;
            }
            if response.hovered() {
                let notches = crate::widgets::knob::wheel_notches(ui);
                if notches != 0.0
                    && let Some(index) = band.q
                {
                    ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                    let q = value(band.q).unwrap_or(1.0) * (notches * 0.25).exp2();
                    out.changes.push((index, self.params[index].clamp(q)));
                    out.finished = true;
                    out.selected = band_index;
                }
            }
            if response.double_clicked()
                && let Some(index) = band.gain
            {
                out.changes.push((index, 0.0));
                out.finished = true;
            }
            response.context_menu(|ui| {
                for (index, choice) in BandType::ALL.iter().enumerate() {
                    if ui.radio(kind == *choice, type_name(*choice)).clicked()
                        && let Some(kind_index) = band.kind
                    {
                        out.changes.push((kind_index, index as f32));
                        out.finished = true;
                        out.selected = band_index;
                        ui.close();
                    }
                }
                ui.separator();
                if let Some(index) = band.dynamic {
                    let mut on = dynamic(&band);
                    if ui.checkbox(&mut on, "Dynamic").clicked() {
                        out.changes.push((index, if on { 1.0 } else { 0.0 }));
                        out.finished = true;
                        out.selected = band_index;
                        ui.close();
                    }
                }
                if ui.button("Remove").clicked()
                    && let Some(index) = band.on
                {
                    out.changes.push((index, 0.0));
                    out.finished = true;
                    ui.close();
                }
            });
        }
        background.on_hover_text("Double-click to add a band");
        out
    }
}

/// A band type's value as its `type` parameter holds it.
pub fn type_index(kind: BandType) -> f32 {
    BandType::ALL.iter().position(|k| *k == kind).unwrap_or(0) as f32
}

pub fn band_name(band: usize) -> String {
    format!("Band {}", band + 1)
}

pub fn type_name(kind: BandType) -> &'static str {
    match kind {
        BandType::LowCut => "Low cut",
        BandType::LowShelf => "Low shelf",
        BandType::Bell => "Bell",
        BandType::Notch => "Notch",
        BandType::HighShelf => "High shelf",
        BandType::HighCut => "High cut",
        BandType::BandPass => "Band pass",
    }
}

fn format_hz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// Where a band's handle is, for tests.
#[cfg(test)]
pub fn handle_pos(
    rect: Rect,
    params: &[ParamDescriptor],
    values: &[f32],
    band: usize,
) -> egui::Pos2 {
    let axes = Axes {
        rect: rect.shrink(PAD),
        low_hz: LOW_HZ,
        high_hz: HIGH_HZ,
        floor_db: -RANGE_DB,
        ceiling_db: RANGE_DB,
    };
    let b = BandParams::find(params, band);
    let value = |index: Option<usize>| index.map_or(0.0, |i| values[i]);
    let kind = BandType::from_index(value(b.kind));
    let db = if kind.has_gain() { value(b.gain) } else { 0.0 };
    pos2(axes.x(value(b.freq)), axes.y(db))
}

/// The y of `db` on the curve, for tests.
#[cfg(test)]
pub fn y_of(rect: Rect, db: f32) -> f32 {
    let area = rect.shrink(PAD);
    area.center().y - db / RANGE_DB * area.height() / 2.0
}
