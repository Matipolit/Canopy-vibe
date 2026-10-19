//! The analyzer's panel, in the spirit of MiniMeters: a spectrum, a
//! waveform colored by frequency, a stereo goniometer with correlation,
//! loudness against a target, and peak and RMS levels. Which views show,
//! and how, are the effect's settings (the View menu). Pop Out opens the
//! same views in a window of their own that stays open whatever track is
//! selected.

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use engine::TapFrame;
use project::DeviceId;

use super::Panel;
use super::panel::display;
use crate::session::Session;
use crate::taps::Tap;
use crate::timeline::TimelineView;
use crate::widgets::scopes::{self, Channels, History, Waveform};
use crate::widgets::spectrum::{Axes, Spectrum, SpectrumSettings};

/// Meter slots, as the analyzer publishes them.
mod meter {
    pub const MOMENTARY: usize = 0;
    pub const SHORT_TERM: usize = 1;
    pub const INTEGRATED: usize = 2;
    pub const RANGE: usize = 3;
    pub const TRUE_PEAK_L: usize = 4;
    pub const TRUE_PEAK_R: usize = 5;
    pub const RMS_L: usize = 6;
    pub const RMS_R: usize = 7;
    pub const CORRELATION: usize = 8;
    pub const BALANCE: usize = 9;
    pub const PEAK_L: usize = 10;
    pub const PEAK_R: usize = 11;
    pub const TRUE_PEAK_MAX: usize = 12;
}

/// Where level displays bottom out.
const FLOOR_DB: f32 = -60.0;

/// What one place the analyzer is drawn (its panel, its window) remembers.
#[derive(Clone)]
struct ViewState {
    spectrum: Spectrum,
    waveform: Waveform,
    waveform_shape: (usize, usize),
    loudness: History,
    /// The frame this was last brought up to date, so it advances once per
    /// frame however often it's drawn.
    pass: u64,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            spectrum: Spectrum::default(),
            waveform: Waveform::default(),
            waveform_shape: (0, 0),
            loudness: History::new(60.0, 10.0),
            pass: u64::MAX,
        }
    }
}

/// The settings that shape the drawing, read once per frame.
struct Look {
    spectrum: SpectrumSettings,
    channel: usize,
    waveform_seconds: f32,
    band_colors: bool,
    scope_ms: f32,
    target: f32,
    main: Color32,
    second: Color32,
}

impl Look {
    fn read(panel: &Panel, ui: &Ui) -> Look {
        let size = [2048, 4096, 8192, 16_384][(panel.setting("spectrum_size") as usize).min(3)];
        let accent = crate::theme::accent(ui.visuals());
        let (main, second) = match panel.setting("colors") as usize {
            1 => (
                Color32::from_rgb(240, 150, 70),
                Color32::from_rgb(250, 215, 130),
            ),
            2 => (
                Color32::from_rgb(90, 200, 230),
                Color32::from_rgb(175, 145, 250),
            ),
            3 => (Color32::from_gray(225), Color32::from_gray(140)),
            _ => (accent, Color32::from_rgb(235, 170, 90)),
        };
        Look {
            spectrum: SpectrumSettings {
                size,
                slope: panel.setting("spectrum_slope"),
                floor_db: panel.setting("spectrum_floor").min(-30.0),
                ceiling_db: 6.0,
                low_hz: panel.setting("spectrum_low").max(10.0),
                high_hz: panel
                    .setting("spectrum_high")
                    .min(panel.sample_rate / 2.0)
                    .max(1000.0),
                fall: panel.setting("spectrum_fall").max(1.0),
                peak_hold: panel.setting_on("spectrum_hold"),
                ..SpectrumSettings::default()
            },
            channel: panel.setting("spectrum_channel") as usize,
            waveform_seconds: panel.setting("waveform_seconds").max(0.5),
            band_colors: panel.setting("waveform_colors") < 0.5,
            scope_ms: panel.setting("scope_ms").max(1.0),
            target: panel.setting("loudness_target"),
            main,
            second,
        }
    }
}

/// The analyzer's panel in the effect rack.
pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let Some(device) = device_of(panel) else {
        return;
    };
    ui.horizontal(|ui| {
        ui.menu_button("View", |ui| panel.settings_menu(ui));
        let open = is_open(ui.ctx(), device);
        if ui.selectable_label(open, "Pop Out").clicked() {
            set_open(ui.ctx(), device, !open);
        }
    });
    let height = (ui.available_height() - 4.0).clamp(110.0, 200.0);
    views(ui, panel, device, "rack", height, false);
}

fn device_of(panel: &Panel) -> Option<DeviceId> {
    match panel.owner {
        super::Owner::Device(device) => Some(device),
        super::Owner::Instrument(_) => None,
    }
}

/// Every view that's switched on, side by side (`wide`: the spectrum on a
/// row of its own above the rest), `height` tall.
fn views(ui: &mut Ui, panel: &mut Panel, device: DeviceId, place: &str, height: f32, wide: bool) {
    let look = Look::read(panel, ui);
    let id = egui::Id::new(("analyzer", device, place));
    let mut state: ViewState = ui.data_mut(|d| d.remove_temp(id)).unwrap_or_default();
    let pass = ui.ctx().cumulative_pass_nr();
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let fresh = state.pass != pass;
    state.pass = pass;
    let meters = panel.meters;
    if fresh {
        state.loudness.push(meters[meter::SHORT_TERM], dt);
    }
    let Some(tap) = panel.tap() else {
        ui.data_mut(|d| d.insert_temp(id, state));
        return;
    };
    if fresh {
        let samples = channel_samples(tap, look.spectrum.size, look.channel);
        state.spectrum.update(&samples, &look.spectrum, dt);
    }
    let rate = panel.sample_rate;
    let frames: Vec<TapFrame> = tap.latest((rate * 0.2) as usize).collect();

    let on = |name: &str| panel.setting_on(name);
    let shown = [
        on("view_spectrum"),
        on("view_waveform"),
        on("view_scope"),
        on("view_stereo"),
        on("view_loudness"),
        on("view_levels"),
    ];
    let row_height = if wide && shown[0] {
        height * 0.55
    } else {
        height
    };
    let rest = if wide && shown[0] {
        height - row_height - 6.0
    } else {
        height
    };
    let spectrum = |ui: &mut Ui, state: &mut ViewState, size: Vec2| {
        spectrum_view(ui, state, &look, rate, size);
    };
    if wide && shown[0] {
        let width = ui.available_width().max(200.0);
        spectrum(ui, &mut state, vec2(width, row_height));
    }
    let h = rest;
    // Wider in a window of its own. In the rack, the spectrum, waveform and
    // scope narrow (down to half) so every view fits beside the others.
    let scale = if wide {
        1.4
    } else {
        // Each view's width at full size, in the order of `shown`.
        let widths = [2.0 * h, 1.5 * h, 1.2 * h, 0.8 * h, 170.0, 86.0];
        let total = |views: std::ops::Range<usize>| -> f32 {
            views.filter(|v| shown[*v]).map(|v| widths[v]).sum()
        };
        let (flex, fixed) = (total(0..3), total(3..6));
        let count = shown.iter().filter(|s| **s).count().max(1);
        let gaps = ui.spacing().item_spacing.x * (count - 1) as f32;
        let room = ui.available_width() - fixed - gaps;
        if flex > 0.0 {
            (room / flex).clamp(0.5, 1.0)
        } else {
            1.0
        }
    };
    ui.horizontal_top(|ui| {
        if shown[0] && !wide {
            spectrum(ui, &mut state, vec2(h * 2.0 * scale, h));
        }
        if shown[1] {
            let size = vec2(h * 1.5 * scale, h);
            waveform_view(ui, &mut state, tap, &look, rate, size, fresh);
        }
        if shown[2] {
            let window = (look.scope_ms / 1000.0 * rate) as usize;
            let (response, painter) = display(ui, vec2(h * 1.2 * scale, h), Sense::hover());
            title(&painter, response.rect, "Oscilloscope", ui.visuals());
            let recent: Vec<TapFrame> = tap.latest(window * 3).collect();
            scopes::oscilloscope(
                &painter,
                response.rect.shrink(6.0),
                &recent,
                window,
                Channels::Stereo,
                [look.main, look.second],
            );
        }
        if shown[3] {
            stereo_view(ui, &frames, &meters, &look, vec2(h * 0.8, h));
        }
        if shown[4] {
            let width = if wide { 170.0 * scale } else { 170.0 };
            loudness_view(ui, &state, &meters, &look, vec2(width, h));
        }
        if shown[5] {
            levels_view(ui, &meters, vec2(86.0, h));
        }
    });
    ui.data_mut(|d| d.insert_temp(id, state));
}

/// The newest `count` samples of one channel (or the mid or side).
fn channel_samples(tap: &Tap, count: usize, channel: usize) -> Vec<f32> {
    tap.latest(count)
        .map(|f| match channel {
            1 => 0.5 * (f[2] - f[3]),
            2 => f[2],
            3 => f[3],
            _ => 0.5 * (f[2] + f[3]),
        })
        .collect()
}

fn title(painter: &egui::Painter, rect: Rect, text: &str, visuals: &egui::Visuals) {
    painter.text(
        rect.left_top() + super::panel::DISPLAY_INSET,
        Align2::LEFT_TOP,
        text,
        FontId::proportional(crate::theme::text::TINY),
        visuals.weak_text_color(),
    );
}

fn spectrum_view(ui: &mut Ui, state: &mut ViewState, look: &Look, rate: f32, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::click());
    let axes = Axes {
        rect: response.rect.shrink2(vec2(4.0, 6.0)),
        low_hz: look.spectrum.low_hz,
        high_hz: look.spectrum.high_hz,
        floor_db: look.spectrum.floor_db,
        ceiling_db: look.spectrum.ceiling_db,
    };
    axes.paint_grid(&painter, ui.visuals(), 12.0);
    state.spectrum.paint(
        &painter,
        &axes,
        &look.spectrum,
        rate,
        Stroke::new(1.5, look.main),
        look.main.gamma_multiply(0.16),
    );
    // Hovering reads out the frequency and note under the pointer.
    if let Some(pos) = response.hover_pos() {
        let hz = axes.hz(pos.x);
        painter.vline(
            pos.x,
            axes.rect.y_range(),
            Stroke::new(1.0, look.second.gamma_multiply(0.5)),
        );
        painter.text(
            pos2(pos.x + 4.0, axes.rect.top()),
            Align2::LEFT_TOP,
            format!("{} · {}", format_hz(hz), note_name(hz)),
            FontId::proportional(crate::theme::text::SMALL),
            ui.visuals().strong_text_color(),
        );
    }
    if response.double_clicked() {
        state.spectrum.clear_peaks();
    }
    response.on_hover_text("Double-click to clear the peak hold");
}

fn format_hz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// The nearest note to `hz`, with how far off it is in cents.
fn note_name(hz: f32) -> String {
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let nearest = midi.round();
    let cents = ((midi - nearest) * 100.0).round() as i32;
    let n = nearest as i32;
    format!(
        "{}{} {cents:+}¢",
        project::NOTE_NAMES[n.rem_euclid(12) as usize],
        n.div_euclid(12) - 1
    )
}

fn waveform_view(
    ui: &mut Ui,
    state: &mut ViewState,
    tap: &Tap,
    look: &Look,
    rate: f32,
    size: Vec2,
    fresh: bool,
) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(2.0, 6.0));
    let columns = rect.width().max(1.0) as usize;
    let per_column = ((look.waveform_seconds * rate) / columns as f32).max(1.0) as usize;
    if state.waveform_shape != (columns, per_column) {
        // A new scale: start over rather than mix column widths.
        state.waveform = Waveform::default();
        state.waveform_shape = (columns, per_column);
        let backlog = tap.total().saturating_sub((columns * per_column) as u64);
        state
            .waveform
            .update(std::iter::empty(), backlog, per_column, columns, rate);
    }
    if fresh {
        let seen = state.waveform.seen();
        state
            .waveform
            .update(tap.since(seen), tap.total(), per_column, columns, rate);
    }
    state
        .waveform
        .paint(&painter, rect, look.band_colors, look.main);
    title(
        &painter,
        response.rect,
        &format!("Waveform · {:.0} s", look.waveform_seconds),
        ui.visuals(),
    );
}

fn stereo_view(ui: &mut Ui, frames: &[TapFrame], meters: &[f32], look: &Look, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect;
    let visuals = ui.visuals().clone();
    let bar_height = 10.0;
    let scope = Rect::from_min_max(
        rect.min + vec2(4.0, 14.0),
        rect.max - vec2(4.0, bar_height + 22.0),
    );
    let newest = &frames[frames.len().saturating_sub(2048)..];
    scopes::goniometer(&painter, scope, newest, look.main);
    let correlation = if meters[meter::CORRELATION] != 0.0 {
        meters[meter::CORRELATION]
    } else {
        scopes::correlation(newest)
    };
    let bar = Rect::from_min_size(
        pos2(rect.left() + 6.0, rect.bottom() - bar_height - 18.0),
        vec2(rect.width() - 12.0, bar_height),
    );
    scopes::correlation_bar(&painter, bar, correlation, &visuals);
    let balance = meters[meter::BALANCE];
    painter.text(
        pos2(rect.center().x, rect.bottom() - 4.0),
        Align2::CENTER_BOTTOM,
        format!("Corr {correlation:+.2} · Bal {}", format_balance(balance)),
        FontId::monospace(crate::theme::text::TINY),
        visuals.text_color(),
    );
    title(&painter, rect, "Stereo", &visuals);
}

fn format_balance(balance: f32) -> String {
    let percent = (balance * 100.0).round() as i32;
    match percent {
        0 => "C".to_owned(),
        p if p < 0 => format!("{}L", -p),
        p => format!("{p}R"),
    }
}

/// Loudness numbers against the target, and the short-term loudness over
/// the last minute.
fn loudness_view(ui: &mut Ui, state: &ViewState, meters: &[f32], look: &Look, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink(6.0);
    let visuals = ui.visuals().clone();
    let target = look.target;
    let lufs = |v: f32| {
        if v <= -99.0 || !v.is_finite() {
            "  -inf".to_owned()
        } else {
            format!("{v:6.1}")
        }
    };
    // Green within a unit of the target, yellow under, red over.
    let judge = |v: f32| {
        if v > target + 1.0 {
            Color32::from_rgb(235, 90, 70)
        } else if v >= target - 1.0 {
            Color32::from_rgb(90, 205, 120)
        } else if v > -99.0 {
            Color32::from_rgb(225, 200, 90)
        } else {
            visuals.weak_text_color()
        }
    };
    let integrated = meters[meter::INTEGRATED];
    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        "Integrated",
        FontId::proportional(crate::theme::text::TINY),
        visuals.weak_text_color(),
    );
    painter.text(
        rect.left_top() + vec2(0.0, crate::theme::space::L),
        Align2::LEFT_TOP,
        format!("{} LUFS", lufs(integrated).trim()),
        FontId::monospace(crate::theme::text::HEADING),
        judge(integrated),
    );
    let rows = [
        (
            "Momentary",
            lufs(meters[meter::MOMENTARY]),
            judge(meters[meter::MOMENTARY]),
        ),
        (
            "Short-term",
            lufs(meters[meter::SHORT_TERM]),
            judge(meters[meter::SHORT_TERM]),
        ),
        (
            "Range",
            format!("{:6.1} LU", meters[meter::RANGE].max(0.0)),
            visuals.text_color(),
        ),
        (
            "True peak",
            format!("{:6.1} dB", meters[meter::TRUE_PEAK_MAX].max(-99.0)),
            if meters[meter::TRUE_PEAK_MAX] > -1.0 {
                Color32::from_rgb(235, 90, 70)
            } else {
                visuals.text_color()
            },
        ),
        ("Target", format!("{target:6.1}"), visuals.weak_text_color()),
    ];
    let mut y = rect.top() + 38.0;
    for (name, value, color) in rows {
        painter.text(
            pos2(rect.left(), y),
            Align2::LEFT_TOP,
            name,
            FontId::proportional(crate::theme::text::SMALL),
            visuals.weak_text_color(),
        );
        painter.text(
            pos2(rect.right(), y),
            Align2::RIGHT_TOP,
            value,
            FontId::monospace(crate::theme::text::SMALL),
            color,
        );
        y += 14.0;
    }
    // The last minute of short-term loudness, around the target.
    let graph = Rect::from_min_max(pos2(rect.left(), y + 4.0), rect.max);
    if graph.height() > 24.0 {
        let (top, bottom) = (target + 9.0, target - 27.0);
        let to_y = |v: f32| {
            let t = ((v - bottom) / (top - bottom)).clamp(0.0, 1.0);
            graph.bottom() - t * graph.height()
        };
        painter.rect_filled(graph, crate::theme::radius::SMALL, visuals.faint_bg_color);
        painter.hline(
            graph.x_range(),
            to_y(target),
            Stroke::new(1.0, Color32::from_rgb(90, 205, 120).gamma_multiply(0.6)),
        );
        state.loudness.paint(
            &painter,
            graph,
            to_y,
            graph.bottom(),
            Stroke::new(1.2, look.main),
            look.main.gamma_multiply(0.15),
        );
    }
}

/// Sample peak bars with the RMS inside them, true peaks underneath.
fn levels_view(ui: &mut Ui, meters: &[f32], size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(6.0, 6.0));
    let visuals = ui.visuals().clone();
    let bars = Rect::from_min_max(rect.min + vec2(18.0, 0.0), rect.max - vec2(0.0, 26.0));
    scopes::db_scale(
        &painter,
        Rect::from_min_max(rect.min, pos2(rect.left() + 15.0, bars.bottom())),
        &[0.0, -6.0, -12.0, -24.0, -48.0],
        FLOOR_DB,
        0.0,
        visuals.weak_text_color(),
    );
    scopes::level_bars(
        &painter,
        bars,
        &[meters[meter::PEAK_L], meters[meter::PEAK_R]],
        FLOOR_DB,
        0.0,
        &visuals,
    );
    // RMS as a brighter line across each bar.
    let half = (bars.width() - 2.0) / 2.0;
    for (i, rms) in [meters[meter::RMS_L], meters[meter::RMS_R]]
        .iter()
        .enumerate()
    {
        let t = ((rms - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
        if t > 0.0 {
            let y = bars.bottom() - t * bars.height();
            let left = bars.left() + i as f32 * (half + 2.0);
            painter.hline(
                left..=left + half,
                y,
                Stroke::new(2.0, visuals.strong_text_color()),
            );
        }
    }
    for (i, peak) in [meters[meter::TRUE_PEAK_L], meters[meter::TRUE_PEAK_R]]
        .iter()
        .enumerate()
    {
        let text = if *peak <= -99.0 {
            "-inf".to_owned()
        } else {
            format!("{peak:.1}")
        };
        let x = bars.left() + half * (i as f32 + 0.5) + i as f32 * 2.0;
        painter.text(
            pos2(x, rect.bottom() - 12.0),
            Align2::CENTER_TOP,
            text,
            FontId::monospace(crate::theme::text::TINY),
            if *peak > -1.0 {
                Color32::from_rgb(235, 90, 70)
            } else {
                visuals.text_color()
            },
        );
    }
    painter.text(
        pos2(bars.center().x, rect.bottom() - 24.0),
        Align2::CENTER_TOP,
        "L  TP  R",
        FontId::proportional(crate::theme::text::TINY),
        visuals.weak_text_color(),
    );
}

fn open_id() -> egui::Id {
    egui::Id::new("analyzer_windows")
}

fn is_open(ctx: &egui::Context, device: DeviceId) -> bool {
    ctx.data(|d| d.get_temp::<Vec<DeviceId>>(open_id()))
        .is_some_and(|open| open.contains(&device))
}

fn set_open(ctx: &egui::Context, device: DeviceId, open: bool) {
    ctx.data_mut(|d| {
        let list = d.get_temp_mut_or_default::<Vec<DeviceId>>(open_id());
        list.retain(|d| *d != device);
        if open {
            list.push(device);
        }
    });
}

/// The analyzers popped out into windows of their own. Call once per frame.
pub(crate) fn windows(ctx: &egui::Context, session: &mut Session, timeline: &mut TimelineView) {
    let open: Vec<DeviceId> = ctx
        .data(|d| d.get_temp::<Vec<DeviceId>>(open_id()))
        .unwrap_or_default();
    for device in open {
        let Some((track, _)) = session.project.find_device(device) else {
            set_open(ctx, device, false);
            continue;
        };
        let Some(entry) = session.project.device(device).cloned() else {
            continue;
        };
        let name = match track.and_then(|t| session.project.track(t)) {
            Some(t) => format!("Analyzer · {}", t.name),
            None => "Analyzer · Master".to_owned(),
        };
        let color = track
            .and_then(|t| session.project.track(t))
            .map_or(Color32::GRAY, |t| crate::timeline::color32(t.color));
        let viewport = egui::ViewportId::from_hash_of(("analyzer", device));
        let builder = egui::ViewportBuilder::default()
            .with_title(name.clone())
            .with_inner_size([900.0, 520.0]);
        let mut close = false;
        // Where the platform can't open windows, egui puts this in one of
        // its own inside the main window.
        ctx.show_viewport_immediate(viewport, builder, |ctx, _| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(mut panel) = Panel::effect(session, timeline, track, &entry, color) {
                    ui.menu_button("View", |ui| panel.settings_menu(ui));
                    let height = ui.available_height() - 4.0;
                    views(ui, &mut panel, device, "window", height, true);
                    panel.animate(ui);
                }
            });
            close = ctx.input(|i| i.viewport().close_requested());
        });
        if close {
            set_open(ctx, device, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_balance_read_naturally() {
        assert_eq!(note_name(440.0), "A4 +0¢");
        assert_eq!(note_name(261.63), "C4 +0¢");
        assert_eq!(format_balance(0.0), "C");
        assert_eq!(format_balance(-0.25), "25L");
        assert_eq!(format_hz(1500.0), "1.50 kHz");
    }
}
