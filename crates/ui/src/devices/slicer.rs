//! The Slicer's panel: the loop as a waveform cut into slices (each in its
//! own tint with its key's name, lit while it plays), where markers are
//! added with a double-click, dragged, and removed with a double-click or
//! Delete; then cards for the tempo matching, the ways to slice, the
//! selected slice's own settings, and the amp.

use std::path::PathBuf;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use instruments::builtin::slicer::{
    KEY_NAMES, METER_HEADS, METER_PLAYING, SLICE_LEVEL, SLICE_PAN, SLICE_PITCH, SLICE_REVERSE,
    VOICES,
};
use midi::Tick;
use project::TrackId;
use project::slicer::MAX_SLICES;

use super::panel::{DISPLAY_INSET, display};
use super::{Owner, Panel};
use crate::session::{DEFAULT_SLICE_SENSITIVITY, Session};
use crate::slicers::FileState;
use crate::theme::{self, radius, size, space, stroke, text};
use crate::widgets::keys::KeyEvent;
use crate::widgets::{chrome, knob};

/// A compact knob's width, which the cards' widths are counted in.
const KNOB: f32 = knob::compact_width(knob::COMPACT);
/// The tempo and slicing cards, and the slice and amp cards.
const TEMPO_WIDTH: f32 = 4.0 * KNOB;
const AMP_WIDTH: f32 = 7.0 * KNOB;
/// The waveform's narrowest and widest, and its least height.
const MIN_WAVE: f32 = 320.0;
const MAX_WAVE: f32 = 900.0;
const MIN_WAVE_HEIGHT: f32 = 60.0;
/// The master's column right of the timeline's lanes, which the panel
/// keeps clear of.
const MASTER_COLUMN: f32 = 84.0;
/// How near a marker the pointer grabs it.
const GRAB: f32 = 5.0;
/// The slices' tint.
const COLOR: Color32 = theme::section::NOTE;
/// Grid slicing's note values.
const DIVISIONS: [(u32, &str); 4] = [(4, "1/4"), (8, "1/8"), (16, "1/16"), (32, "1/32")];

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let Owner::Instrument(track) = panel.owner else {
        return;
    };
    let height = ui.available_height();
    let compact = std::mem::replace(&mut panel.compact, true);
    egui::ScrollArea::vertical()
        .id_salt(panel.id("slicer scroll"))
        .auto_shrink([true, true])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::M;
                // The cards take what they need; the waveform what's left
                // in view (the device view scrolls sideways, so what's
                // available is more than what shows), ending where the
                // timeline's lanes do above it.
                let cards = TEMPO_WIDTH + AMP_WIDTH + 2.0 * space::M + 6.0 * space::S;
                let visible = ui.clip_rect().right() - ui.cursor().left() - MASTER_COLUMN;
                let wave = (visible - cards).clamp(MIN_WAVE, MAX_WAVE);
                waveform_card(ui, panel, track, wave, height);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::S;
                    tempo(ui, panel, track);
                    slicing(ui, panel, track);
                });
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::S;
                    selected_slice(ui, panel, track);
                    amp(ui, panel);
                });
            });
        });
    panel.compact = compact;
}

/// The slice the panel shows the settings of, clicked on the waveform.
fn selected(ui: &Ui, track: TrackId) -> usize {
    ui.data(|d| d.get_temp(selected_id(track))).unwrap_or(0)
}

fn selected_id(track: TrackId) -> egui::Id {
    egui::Id::new(("slicer selected slice", track.0))
}

/// The track's slice starts, in the file's frames (just 0 for none).
fn starts(session: &Session, track: TrackId) -> Vec<u64> {
    match session.project.track(track).map(|t| t.slices()) {
        Some([]) | None => vec![0],
        Some(slices) => slices.to_vec(),
    }
}

/// The loop's card: Load…, the file's name and its slices in the header,
/// then the waveform, or what's wrong.
fn waveform_card(ui: &mut Ui, panel: &mut Panel, track: TrackId, width: f32, height: f32) {
    let file = panel
        .session
        .project
        .track(track)
        .and_then(|t| t.instrument_file())
        .map(std::path::Path::to_owned);
    chrome::section(ui, COLOR, |ui| {
        ui.set_width(width);
        chrome::section_header(ui, "", COLOR, |ui| {
            if ui
                .button(if file.is_some() {
                    "Load…"
                } else {
                    "Load Sample…"
                })
                .on_hover_text("Pick an audio file to slice, or drop one on the waveform")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        "Audio",
                        &["wav", "flac", "mp3", "ogg", "aif", "aiff", "WAV", "FLAC"],
                    )
                    .pick_file()
            {
                panel
                    .session
                    .load_slicer(path, Some(track), project::TrackPosition::END);
            }
            if file.is_some()
                && ui
                    .button("Reload")
                    .on_hover_text("Read the file again, after editing it")
                    .clicked()
            {
                panel.session.reload_slicer(track);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count = starts(panel.session, track).len();
                if file.is_some() {
                    chrome::caption(
                        ui,
                        &format!("{count} {}", if count == 1 { "slice" } else { "slices" }),
                    );
                }
                if let Some(path) = &file {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let name = path.file_name().map_or_else(
                            || path.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                            .on_hover_text(path.display().to_string());
                    });
                }
            });
        });
        // The card is as tall as the panel leaves (less the gap under it).
        let inside = height - space::S - 2.0 * space::S;
        let tall = (inside - ui.min_rect().height() - space::XS).max(MIN_WAVE_HEIGHT);
        waveform(ui, panel, track, file.is_some(), vec2(width, tall));
    });
}

/// The waveform cut into slices, and everything done on it.
fn waveform(ui: &mut Ui, panel: &mut Panel, track: TrackId, has_file: bool, size: egui::Vec2) {
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    let rect = response.rect;
    register_drop_target(ui, rect, track);
    let visuals = ui.visuals().clone();
    let accent = theme::accent(&visuals);
    let drop_hover = {
        let over = |pos: Option<Pos2>| pos.is_some_and(|p| rect.contains(p));
        let files = ui.input(|i| !i.raw.hovered_files.is_empty()) && over(response.hover_pos());
        files || over(panel.timeline.external_hover)
    };
    let view = panel.session.slicer_view(track);
    let Some(view) = view else {
        let (title, hint) = match panel.session.slicer_file(track) {
            Some(FileState::Loading) => ("Loading…", ""),
            Some(FileState::Failed(err)) => ("Can't play this file", err.as_str()),
            _ if has_file => ("Loading…", ""),
            _ => (
                "No sample loaded",
                "Drop an audio file here, or Load… one: it plays nothing until then",
            ),
        };
        let (title, hint) = (title.to_owned(), hint.to_owned());
        chrome::empty_hint(&painter, rect, &title, &hint);
        if drop_hover {
            drop_outline(&painter, rect, accent, &visuals);
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("Slices: {title}"))
        });
        return;
    };

    let area = rect.shrink2(vec2(space::XS, space::S));
    let frames = view.source.native_frames.max(1) as f64;
    let x_of = |frame: u64| area.left() + (frame as f64 / frames) as f32 * area.width();
    let frame_at = |x: f32| -> u64 {
        (((x - area.left()) / area.width()).clamp(0.0, 1.0) as f64 * frames).round() as u64
    };
    let starts = starts(panel.session, track);
    let root = panel.value("root") as usize;
    let count = starts.len();
    let span = |k: usize| {
        (
            starts[k],
            starts.get(k + 1).copied().unwrap_or(frames as u64),
        )
    };
    let playing = |k: usize| {
        let word = panel.meters[METER_PLAYING + k / 24] as u32;
        word >> (k % 24) & 1 == 1
    };
    let chosen = selected(ui, track).min(count - 1);

    // The markers under the pointer, and what's being dragged.
    let drag_id = panel.id("slicer marker drag");
    let pointer = response.hover_pos().or(response.interact_pointer_pos());
    let marker_at = |p: Pos2| {
        (1..count)
            .map(|k| (k, (x_of(starts[k]) - p.x).abs()))
            .filter(|(_, d)| *d <= GRAB)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    };
    let near = pointer.and_then(marker_at);
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));

    // Slices in alternating tints, the selected one outlined, the playing
    // ones lit, each named by its key.
    for k in 0..count {
        let (from, to) = span(k);
        let band = Rect::from_x_y_ranges(x_of(from)..=x_of(to), area.y_range());
        let fill = if playing(k) {
            0.42
        } else if k % 2 == 0 {
            0.07
        } else {
            0.15
        };
        painter.rect_filled(band, 0.0, COLOR.gamma_multiply(fill));
        if k == chosen {
            painter.rect_stroke(
                band.shrink(0.5),
                radius::SMALL,
                Stroke::new(stroke::HAIRLINE, COLOR),
                egui::StrokeKind::Inside,
            );
        }
        if band.width() > 18.0
            && let Some(name) = KEY_NAMES.get(root + k)
        {
            painter.text(
                band.left_top() + vec2(space::XS, space::XS),
                Align2::LEFT_TOP,
                *name,
                FontId::proportional(text::TINY),
                if k == chosen {
                    visuals.strong_text_color()
                } else {
                    visuals.weak_text_color()
                },
            );
        }
    }
    // The matched audio, one column a pixel.
    let peaks = view.peaks();
    let total = peaks.frames() as f64;
    let middle = area.center().y;
    let half = area.height() / 2.0 - space::S;
    let ink = visuals.text_color().gamma_multiply(0.8);
    let columns = area.width().max(1.0) as usize;
    for column in 0..columns {
        let from = column as f64 / columns as f64 * total;
        let to = (column + 1) as f64 / columns as f64 * total;
        let (low, high) = peaks.min_max(from, to);
        let x = area.left() + column as f32 + 0.5;
        painter.vline(
            x,
            (middle - high.clamp(-1.0, 1.0) * half)..=(middle - low.clamp(-1.0, 1.0) * half + 0.5),
            Stroke::new(1.0, ink),
        );
    }
    // The markers; the one under the pointer or dragged stands out.
    for (k, &start) in starts.iter().enumerate().skip(1) {
        let lit = Some(k) == near || Some(k) == dragging;
        let (width, color) = if lit {
            (stroke::INDICATOR, accent)
        } else {
            (stroke::HAIRLINE, COLOR)
        };
        let x = x_of(start);
        painter.vline(x, area.y_range(), Stroke::new(width, color));
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos2(x - 3.5, area.top()),
                pos2(x + 3.5, area.top()),
                pos2(x, area.top() + 5.0),
            ],
            color,
            Stroke::NONE,
        ));
    }
    // Where each voice is.
    for head in &panel.meters[METER_HEADS..METER_HEADS + VOICES] {
        if (0.0..=1.0).contains(head) {
            let x = area.left() + head * area.width();
            painter.vline(
                x,
                area.y_range(),
                Stroke::new(stroke::HAIRLINE, theme::color::PLAYHEAD),
            );
        }
    }
    if view.speed != 1.0 {
        let note = if view.rendering {
            format!("×{:.2} · stretching…", view.speed)
        } else {
            format!("×{:.2}", view.speed)
        };
        painter.text(
            rect.right_top() + vec2(-DISPLAY_INSET.x, DISPLAY_INSET.y),
            Align2::RIGHT_TOP,
            note,
            FontId::proportional(text::TINY),
            visuals.weak_text_color(),
        );
    }
    if drop_hover {
        drop_outline(&painter, rect, accent, &visuals);
    }

    // Moving a marker: live while dragged, one undo step when let go.
    if response.drag_started() {
        // Where the press was: the pointer has moved a little by now.
        let origin = ui.input(|i| i.pointer.press_origin());
        dragging = origin.or(pointer).and_then(marker_at);
        if dragging.is_some() {
            panel.session.begin_live();
        }
    }
    if let (Some(k), Some(pos)) = (dragging, response.interact_pointer_pos())
        && response.dragged()
        && k < count
    {
        let low = starts[k - 1] + 1;
        let high = starts.get(k + 1).map_or(frames as u64, |next| next - 1);
        let mut moved = starts.clone();
        moved[k] = frame_at(pos.x).clamp(low, high.max(low));
        if moved != starts {
            panel.session.live_edit(|p| p.set_slices(track, moved));
        }
    }
    if response.drag_stopped() && dragging.take().is_some() {
        panel.session.end_live("Move Slice Marker");
    }
    ui.data_mut(|d| match dragging {
        Some(k) => {
            d.insert_temp(drag_id, k);
        }
        None => d.remove::<usize>(drag_id),
    });

    // Double-clicks add and remove markers; a click picks a slice (and
    // hears it while held). A picked marker goes with Delete.
    let marker_id = selected_marker_id();
    if response.double_clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let mut edited = starts.clone();
        if let Some(k) = near {
            edited.remove(k);
            panel
                .session
                .edit("Remove Slice Marker", |p| p.set_slices(track, edited));
            ui.data_mut(|d| d.remove::<(TrackId, u64, u64)>(marker_id));
        } else if count < MAX_SLICES {
            edited.push(frame_at(pos.x));
            panel
                .session
                .edit("Add Slice Marker", |p| p.set_slices(track, edited));
        } else {
            panel.session.error("A slicer has at most 64 slices");
        }
    } else if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        match near {
            Some(k) => {
                let pass = ui.ctx().cumulative_pass_nr();
                ui.data_mut(|d| d.insert_temp(marker_id, (track, starts[k], pass)));
            }
            None => {
                ui.data_mut(|d| d.remove::<(TrackId, u64, u64)>(marker_id));
                let frame = frame_at(pos.x);
                let k = starts.iter().rposition(|&s| s <= frame).unwrap_or(0);
                ui.data_mut(|d| d.insert_temp(selected_id(track), k));
            }
        }
    }
    // Keeps a picked marker picked while the waveform is on screen.
    let pass = ui.ctx().cumulative_pass_nr();
    ui.data_mut(|d| {
        if let Some((t, at, _)) = d.get_temp::<(TrackId, u64, u64)>(marker_id)
            && t == track
        {
            if starts.contains(&at) {
                d.insert_temp(marker_id, (t, at, pass));
            } else {
                d.remove::<(TrackId, u64, u64)>(marker_id);
            }
        }
    });
    preview(
        ui,
        panel,
        &response,
        near.is_none() && dragging.is_none(),
        |x| {
            let frame = frame_at(x);
            root + starts.iter().rposition(|&s| s <= frame).unwrap_or(0)
        },
    );

    if near.is_some() || dragging.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            format!("Slices: {count} slices"),
        )
    });
    response.on_hover_text(
        "Click a slice to pick and hear it; double-click to add a marker, drag one to move \
         it, double-click it (or click it and press Delete) to remove it. Drop an audio file \
         to slice it instead",
    );
}

/// Plays the slice under the pointer while the waveform is pressed (when
/// `allowed`), and lets it go on release.
fn preview(
    ui: &Ui,
    panel: &mut Panel,
    response: &egui::Response,
    allowed: bool,
    key_at: impl Fn(f32) -> usize,
) {
    let id = panel.id("slicer preview");
    let held: Option<u8> = ui.data(|d| d.get_temp(id));
    let pressed = ui.input(|i| i.pointer.primary_pressed());
    if allowed
        && pressed
        && held.is_none()
        && let Some(pos) = response.hover_pos()
        && let Ok(key) = u8::try_from(key_at(pos.x))
        && key < 128
    {
        panel.play(KeyEvent::Down(key));
        ui.data_mut(|d| d.insert_temp(id, key));
    }
    if let Some(key) = held
        && !ui.input(|i| i.pointer.primary_down())
    {
        panel.play(KeyEvent::Up(key));
        ui.data_mut(|d| d.remove::<u8>(id));
    }
}

fn drop_outline(painter: &egui::Painter, rect: Rect, accent: Color32, visuals: &egui::Visuals) {
    painter.rect_stroke(
        rect.shrink(1.0),
        radius::PANEL,
        Stroke::new(stroke::INDICATOR, accent),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center_bottom() - vec2(0.0, space::S),
        Align2::CENTER_BOTTOM,
        "Drop to slice this file",
        FontId::proportional(text::SMALL),
        visuals.strong_text_color(),
    );
}

/// The tempo matching: the mode, the loop's own tempo and whether it
/// follows the project's, and how fast that makes it play.
fn tempo(ui: &mut Ui, panel: &mut Panel, track: TrackId) {
    let color = theme::section::OSC_A;
    chrome::section(ui, color, |ui| {
        ui.set_width(TEMPO_WIDTH);
        chrome::section_header(ui, "Tempo", color, |ui| {
            panel.choice_menu(ui, "mode");
        });
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            panel.knob_as(ui, "original_tempo", Some("Tempo"));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = space::XS;
                ui.add_space(space::S);
                panel.toggle(ui, "follow", "Follow");
                let speed = panel.session.project.track(track).map_or(1.0, |t| {
                    project::slicer::speed(t, &panel.session.project.tempo)
                });
                let text = if speed == 1.0 {
                    "Plays as is".to_owned()
                } else {
                    format!("Plays at ×{speed:.2}")
                };
                chrome::caption(ui, &text);
            });
        });
    });
}

/// The ways to slice: at transients (how eagerly), by a grid, and the
/// slices written out as a clip.
fn slicing(ui: &mut Ui, panel: &mut Panel, track: TrackId) {
    let color = theme::section::CONTROL;
    chrome::section(ui, color, |ui| {
        ui.set_width(TEMPO_WIDTH);
        chrome::section_header(ui, "Slicing", color, |_| {});
        let id = panel.id("slicer sensitivity");
        let mut sensitivity: f32 = ui
            .data(|d| d.get_temp(id))
            .unwrap_or(DEFAULT_SLICE_SENSITIVITY);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            if ui
                .button("Transients")
                .on_hover_text("Slice at every hit, more of them the higher the sensitivity")
                .clicked()
            {
                panel.session.slice_at_transients(track, sensitivity);
            }
            let mut percent = sensitivity * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut percent)
                        .range(0.0..=100.0)
                        .speed(0.5)
                        .suffix(" %"),
                )
                .on_hover_text("Sensitivity: how quiet a hit still counts")
                .changed()
            {
                sensitivity = percent / 100.0;
                ui.data_mut(|d| d.insert_temp(id, sensitivity));
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            chrome::caption(ui, "Grid");
            for (division, label) in DIVISIONS {
                if ui
                    .add(egui::Button::new(label).min_size(vec2(0.0, size::CONTROL)))
                    .on_hover_text(format!(
                        "A slice every {label} note at the loop's own tempo"
                    ))
                    .clicked()
                {
                    panel.session.slice_by_grid(track, division);
                }
            }
        });
        if ui
            .button("Slices to Piano Roll")
            .on_hover_text("Write a clip that plays every slice in order, at the playhead's bar")
            .clicked()
        {
            slices_to_piano_roll(panel, track);
        }
    });
}

/// Writes the slices out as a clip at the playhead's bar, and selects it.
fn slices_to_piano_roll(panel: &mut Panel, track: TrackId) {
    let tempo = panel.session.project.tempo;
    let playhead = Tick::new(panel.session.playhead().max(0.0) as i64);
    let bar = tempo.bar_len().get().max(1);
    let at = Tick::new(playhead.get() / bar * bar);
    if let Some(clip) = panel.session.slices_to_piano_roll(track, at) {
        panel.timeline.selection.clips = vec![clip];
    }
}

/// The selected slice's own settings.
fn selected_slice(ui: &mut Ui, panel: &mut Panel, track: TrackId) {
    let count = starts(panel.session, track).len();
    let k = selected(ui, track).min(count - 1);
    let color = COLOR;
    let root = panel.value("root") as usize;
    let key = KEY_NAMES.get(root + k).copied().unwrap_or("–");
    chrome::section(ui, color, |ui| {
        ui.set_width(AMP_WIDTH);
        chrome::section_header(ui, &format!("Slice {} · {key}", k + 1), color, |ui| {
            panel.toggle_at(ui, SLICE_REVERSE + k, "Reverse");
        });
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            panel.knob_at(ui, SLICE_PITCH + k, Some("Pitch"), None);
            panel.knob_at(ui, SLICE_LEVEL + k, Some("Level"), None);
            panel.knob_at(ui, SLICE_PAN + k, Some("Pan"), None);
        });
    });
}

/// The amp: level, pitch and envelope, Cut, and the key slice 1 is on.
fn amp(ui: &mut Ui, panel: &mut Panel) {
    let color = theme::section::ENVELOPE;
    chrome::section(ui, color, |ui| {
        ui.set_width(AMP_WIDTH);
        chrome::section_header(ui, "Amp", color, |ui| {
            panel
                .toggle(ui, "cut", "Cut")
                .map(|r| r.on_hover_text("A new slice stops the one playing"));
        });
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            panel.knobs(
                ui,
                &["volume", "pitch", "attack", "decay", "sustain", "release"],
            );
            panel.knob_as(ui, "root", Some("Root"));
        });
    });
}

/// Where slicer waveforms were drawn, for files dropped on them.
#[derive(Clone, Default)]
struct DropTargets {
    pass: u64,
    targets: Vec<(Rect, TrackId)>,
}

fn drop_targets_id() -> egui::Id {
    egui::Id::new("slicer drop targets")
}

/// A marker clicked on: its track, its frame and the pass it was last
/// drawn in.
fn selected_marker_id() -> egui::Id {
    egui::Id::new("slicer selected marker")
}

fn register_drop_target(ui: &Ui, rect: Rect, track: TrackId) {
    let rect = rect.intersect(ui.clip_rect());
    let ctx = ui.ctx();
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let drops = d.get_temp_mut_or_default::<DropTargets>(drop_targets_id());
        if drops.pass != pass {
            drops.pass = pass;
            drops.targets.clear();
        }
        if rect.is_positive() {
            drops.targets.push((rect, track));
        }
    });
}

/// Files dropped at `pos`: if that's on a slicer's waveform (as drawn last
/// frame) and the first is an audio file, the slicer loads it, and the
/// drop is taken. Otherwise false: the drop is someone else's.
pub(crate) fn take_drop(
    ctx: &egui::Context,
    session: &mut Session,
    pos: Option<Pos2>,
    paths: &[PathBuf],
) -> bool {
    let pass = ctx.cumulative_pass_nr();
    let Some(track) = pos.and_then(|pos| {
        ctx.data(|d| {
            let drops = d.get_temp::<DropTargets>(drop_targets_id())?;
            if drops.pass + 1 < pass {
                return None;
            }
            let (_, track) = drops.targets.iter().find(|(rect, _)| rect.contains(pos))?;
            Some(*track)
        })
    }) else {
        return false;
    };
    let Some(path) = paths.first().filter(|p| media::is_supported_extension(p)) else {
        return false;
    };
    session.load_slicer(path.clone(), Some(track), project::TrackPosition::END);
    true
}

/// Delete pressed while a marker is picked on a slicer's waveform drawn
/// this pass or the last: the marker goes, as one undo step, and the key
/// is taken. False if no marker is picked.
pub(crate) fn delete_marker(ctx: &egui::Context, session: &mut Session) -> bool {
    let pass = ctx.cumulative_pass_nr();
    let Some((track, at, drawn)) =
        ctx.data(|d| d.get_temp::<(TrackId, u64, u64)>(selected_marker_id()))
    else {
        return false;
    };
    ctx.data_mut(|d| d.remove::<(TrackId, u64, u64)>(selected_marker_id()));
    if drawn + 1 < pass {
        return false;
    }
    let Some(t) = session.project.track(track) else {
        return false;
    };
    let starts: Vec<u64> = t.slices().iter().copied().filter(|&s| s != at).collect();
    if starts.len() == t.slices().len() {
        return false;
    }
    session.edit("Remove Slice Marker", |p| p.set_slices(track, starts));
    true
}
