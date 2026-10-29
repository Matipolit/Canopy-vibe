//! The left column of each track row: color, name, fold arrow, mute, solo,
//! volume and pan, plus drag-and-drop reordering.

use eframe::egui::{
    self, Align, Color32, DragValue, Key, Layout, Rect, RichText, Sense, Stroke, TextEdit,
    UiBuilder, pos2, vec2,
};
use project::{
    AutomationTarget, Color, Mixer, Project, SILENCE_DB, Track, TrackId, TrackKind, TrackPosition,
};

use super::{
    Action, DropIndicator, Gesture, RowKind, SelectMode, TimelineView, TrackDrop, color32, id,
};
use crate::session::Session;
use crate::theme::{self, anim, color, radius, size, space, stroke};
use crate::widgets::chrome;

const INDENT: f32 = 12.0;
pub(super) const COLOR_STRIP: f32 = 5.0;
const METER_WIDTH: f32 = 7.0;
/// The volume and pan fields' widths, the same on every track.
const VOLUME_WIDTH: f32 = 60.0;
const PAN_WIDTH: f32 = 40.0;

pub(super) fn show(
    view: &mut TimelineView,
    ui: &mut egui::Ui,
    session: &Session,
    track: &Track,
    depth: usize,
    rect: Rect,
) {
    let selected = view.selection.has_track(track.id);
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    // Selection fades in and out rather than snapping.
    let lit =
        ui.ctx()
            .animate_bool_with_time(id("header selected", track.id.0), selected, anim::NORMAL);
    let hovered = ui.ctx().animate_bool_with_time(
        id("header hovered", track.id.0),
        ui.rect_contains_pointer(rect),
        anim::FAST,
    );
    let background = theme::selected_fill(&visuals, visuals.panel_fill, lit)
        .lerp_to_gamma(visuals.widgets.hovered.weak_bg_fill, 0.2 * hovered);
    painter.rect_filled(rect, 0.0, background);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        visuals.widgets.noninteractive.bg_stroke,
    );
    // Guides showing which group a track is in.
    for level in 0..depth {
        let x = rect.left() + COLOR_STRIP + level as f32 * INDENT + 6.0;
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.4)),
        );
    }
    let strip_left = rect.left() + depth as f32 * INDENT;
    let strip = Rect::from_min_max(
        pos2(strip_left, rect.top()),
        pos2(strip_left + COLOR_STRIP, rect.bottom()),
    );
    painter.rect_filled(strip, 0.0, color32(track.color));
    paint_meter(&painter, rect, session.track_peaks(track.id));

    let response = ui.interact(rect, id("header", track.id.0), Sense::click_and_drag());
    handle_header_response(view, ui, session, track, &response);
    drop_effects(view, ui, track, &response, rect);

    let content = Rect::from_min_max(
        pos2(strip.right() + space::S, rect.top() + space::S),
        pos2(
            rect.right() - METER_WIDTH - space::S,
            rect.bottom() - space::XS,
        ),
    );
    // A child that doesn't allocate in the row's layout: the row has
    // already been allocated, and allocating again would move the cursor.
    // Salted with the track id so the widgets keep their ids (and any drag in
    // progress) when tracks above are added or removed.
    let ui = &mut ui.new_child(
        UiBuilder::new()
            .id_salt(("track_header", track.id.0))
            .max_rect(content)
            .layout(Layout::top_down(Align::Min)),
    );
    {
        ui.spacing_mut().item_spacing = vec2(space::S, space::S - 1.0);
        // Short rows (zoomed out) keep only the name and mute/solo, on one line.
        let compact = content.height() < 40.0;
        ui.horizontal(|ui| {
            if let TrackKind::Group { folded, .. } = &track.kind
                && chrome::fold_arrow(ui, *folded).clicked()
            {
                view.push(Action::SetFolded(track.id, !folded));
            }
            if compact {
                let width = (ui.available_width() - 46.0).max(20.0);
                ui.allocate_ui(vec2(width, ui.available_height()), |ui| {
                    ui.set_width(width);
                    name(view, ui, track);
                });
                mute_solo(view, ui, track);
            } else if let Some(instrument) = instrument_label(track) {
                // The instrument, faint, after the name: what plays here.
                let text = RichText::new(instrument).small().weak();
                let galley = egui::WidgetText::from(text.clone()).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    ui.available_width() / 2.0,
                    egui::TextStyle::Small,
                );
                let width = (ui.available_width() - galley.size().x - 6.0).max(30.0);
                ui.allocate_ui(vec2(width, ui.available_height()), |ui| {
                    ui.set_width(width);
                    name(view, ui, track);
                });
                ui.add(egui::Label::new(text).truncate().selectable(false));
            } else {
                name(view, ui, track);
            }
        });
        if !compact {
            ui.horizontal(|ui| mixer_controls(view, ui, track));
        }
    }
}

/// What a MIDI track plays, for its header: the instrument's name, or a
/// sampler's instrument file.
fn instrument_label(track: &Track) -> Option<String> {
    let TrackKind::Midi { instrument, .. } = &track.kind else {
        return None;
    };
    if let Some(file) = track.instrument_file()
        && let Some(stem) = file.file_stem()
    {
        return Some(stem.to_string_lossy().into_owned());
    }
    instrument.descriptor().map(|d| d.name.to_owned())
}

fn name(view: &mut TimelineView, ui: &mut egui::Ui, track: &Track) {
    match &mut view.renaming {
        Some((id, text)) if *id == track.id => {
            let response =
                ui.add(TextEdit::singleline(text).desired_width(ui.available_width() - 4.0));
            if !response.has_focus() && !response.lost_focus() {
                response.request_focus();
            }
            if response.lost_focus() {
                let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
                let (id, text) = view.renaming.take().expect("matched above");
                if !cancelled && !text.trim().is_empty() {
                    view.push(Action::RenameTrack(id, text.trim().to_owned()));
                }
            }
        }
        _ => {
            let label = egui::Label::new(
                RichText::new(&track.name)
                    .font(theme::text::medium(theme::text::BODY))
                    .strong(),
            )
            .truncate()
            .selectable(false)
            .sense(Sense::click());
            if ui.add(label).double_clicked() {
                view.renaming = Some((track.id, track.name.clone()));
            }
        }
    }
}

fn mixer_controls(view: &mut TimelineView, ui: &mut egui::Ui, track: &Track) {
    let mixer = track.mixer;
    mute_solo(view, ui, track);
    let tint = color32(track.color);
    // Fixed widths, so the fields line up from track to track.
    let mut volume = mixer.volume_db;
    let level = fader_position(mixer.volume_db);
    let response = level_field(ui, VOLUME_WIDTH, (0.0, level), tint, |ui, rect| {
        ui.put(rect, volume_drag(&mut volume))
    })
    .on_hover_text("Volume: drag, or double-click to type");
    automate_menu(view, &response, track.id, AutomationTarget::Volume);
    live_mixer(
        view,
        track.id,
        &response,
        Mixer {
            volume_db: volume,
            ..mixer
        },
        "Volume",
    );

    let mut pan = mixer.pan * 50.0;
    let middle = 0.5 + 0.5 * mixer.pan;
    let response = level_field(
        ui,
        PAN_WIDTH,
        (middle.min(0.5), middle.max(0.5)),
        tint,
        |ui, rect| {
            ui.put(
                rect,
                DragValue::new(&mut pan)
                    .range(-50.0..=50.0)
                    .speed(0.5)
                    .update_while_editing(false)
                    .custom_formatter(|v, _| format_pan(v as f32 / 50.0))
                    .custom_parser(parse_pan),
            )
        },
    )
    .on_hover_text("Pan: drag, or double-click to type (25L, C, 10R)");
    automate_menu(view, &response, track.id, AutomationTarget::Pan);
    live_mixer(
        view,
        track.id,
        &response,
        Mixer {
            pan: pan / 50.0,
            ..mixer
        },
        "Pan",
    );

    let shown = view
        .automation
        .get(&track.id)
        .is_some_and(|l| !l.is_empty());
    let lit = theme::accent(ui.visuals()).gamma_multiply(0.8);
    if chrome::toggle(ui, "A", shown, lit, "Show or hide automation lanes").clicked() {
        view.push(Action::ToggleAutomation(track.id));
    }
}

fn mute_solo(view: &mut TimelineView, ui: &mut egui::Ui, track: &Track) {
    let mixer = track.mixer;
    if chrome::toggle(ui, "M", mixer.mute, color::MUTE, "Mute").clicked() {
        let mixer = Mixer {
            mute: !mixer.mute,
            ..mixer
        };
        view.push(Action::SetMixer {
            track: track.id,
            mixer,
            label: "Mute",
        });
    }
    if chrome::toggle(ui, "S", mixer.solo, color::SOLO, "Solo").clicked() {
        let mixer = Mixer {
            solo: !mixer.solo,
            ..mixer
        };
        view.push(Action::SetMixer {
            track: track.id,
            mixer,
            label: "Solo",
        });
    }
}

/// Right-click → Automate, or link a MIDI controller, for any control that
/// automation can drive.
pub fn automate_menu(
    view: &mut TimelineView,
    response: &egui::Response,
    track: TrackId,
    target: AutomationTarget,
) {
    control_menu(view, response, Some(track), target);
}

/// The right-click menu of a control: Automate (on tracks), and linking a
/// MIDI controller. `track` is `None` for the master.
pub fn control_menu(
    view: &mut TimelineView,
    response: &egui::Response,
    track: Option<TrackId>,
    target: AutomationTarget,
) {
    // Sliders only sense drags; a context menu needs clicks.
    let response = response.interact(Sense::click());
    clicked_control_menu(view, &response, track, target);
}

/// [`control_menu`] for a control that already senses clicks (a knob, a
/// button), so no second widget is laid over it to catch them.
pub fn clicked_control_menu(
    view: &mut TimelineView,
    response: &egui::Response,
    track: Option<TrackId>,
    target: AutomationTarget,
) {
    clicked_control_menu_with(view, response, track, target, |_| {});
}

/// [`clicked_control_menu`] with the control's own items (`own`) first.
pub fn clicked_control_menu_with(
    view: &mut TimelineView,
    response: &egui::Response,
    track: Option<TrackId>,
    target: AutomationTarget,
    own: impl FnOnce(&mut egui::Ui),
) {
    let linked = view
        .controls
        .links
        .iter()
        .find(|l| (l.track, &l.target) == (track, &target))
        .map(|l| l.controller);
    let learning = view.controls.learning.as_ref() == Some(&(track, target.clone()));
    if let Some(controller) = linked {
        response
            .clone()
            .on_hover_text(format!("Linked to CC {controller}"));
    }
    response.context_menu(|ui| {
        own(ui);
        if let Some(track) = track
            && ui.button("Automate").clicked()
        {
            view.push(Action::ShowAutomation(track, target.clone()));
            ui.close();
        }
        let learn = if learning {
            "Cancel Linking"
        } else {
            "Link to Controller"
        };
        if ui.button(learn).clicked() {
            view.push(Action::LearnController(track, target.clone()));
            ui.close();
        }
        if let Some(controller) = linked
            && ui.button(format!("Unlink CC {controller}")).clicked()
        {
            view.push(Action::UnlinkController(track, target.clone()));
            ui.close();
        }
    });
}

/// Where a fader at `db` sits along its travel, 0 to 1: the fourth root of
/// its gain, so the dB that matter (around 0) get most of the travel.
pub fn fader_position(db: f32) -> f32 {
    if db <= SILENCE_DB {
        return 0.0;
    }
    let top = project::volume_gain(project::MAX_VOLUME_DB).powf(0.25);
    (project::volume_gain(db).powf(0.25) / top).clamp(0.0, 1.0)
}

/// A volume or pan field drawn as a small bar: the stretch `filled` (0 to
/// 1 across it) tinted with the track's color under the value. `add` puts
/// the field's own widget (a `DragValue`) over `rect`, without a fill of
/// its own.
fn level_field(
    ui: &mut egui::Ui,
    width: f32,
    filled: (f32, f32),
    tint: Color32,
    add: impl FnOnce(&mut egui::Ui, Rect) -> egui::Response,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, size::SMALL_CONTROL), Sense::hover());
    let ground = ui.painter().add(egui::Shape::Noop);
    let response = ui
        .scope(|ui| {
            let widgets = &mut ui.visuals_mut().widgets;
            for state in [
                &mut widgets.inactive,
                &mut widgets.hovered,
                &mut widgets.active,
            ] {
                state.weak_bg_fill = Color32::TRANSPARENT;
                state.bg_fill = Color32::TRANSPARENT;
            }
            add(ui, rect)
        })
        .inner;
    let visuals = ui.visuals();
    let h = theme::hover(ui, &response);
    let back = visuals
        .extreme_bg_color
        .lerp_to_gamma(visuals.widgets.inactive.weak_bg_fill, 0.5 + 0.5 * h);
    let corner = egui::CornerRadius::same(radius::CONTROL);
    let inner = rect.shrink(1.0);
    let fill = Rect::from_min_max(
        pos2(inner.left() + filled.0 * inner.width(), inner.top()),
        pos2(inner.left() + filled.1 * inner.width(), inner.bottom()),
    );
    let mut shapes = vec![egui::Shape::rect_filled(rect, corner, back)];
    if fill.width() > 0.5 {
        shapes.push(egui::Shape::rect_filled(
            fill,
            egui::CornerRadius::same(radius::SMALL),
            tint.gamma_multiply(0.45 + 0.15 * h),
        ));
    }
    ui.painter().set(ground, egui::Shape::Vec(shapes));
    response
}

pub fn volume_drag(volume: &mut f32) -> DragValue<'_> {
    DragValue::new(volume)
        .range(SILENCE_DB..=project::MAX_VOLUME_DB)
        .speed(0.25)
        // Typed values apply on Enter, not with every keystroke.
        .update_while_editing(false)
        .fixed_decimals(1)
        .custom_formatter(|v, _| {
            if v as f32 <= SILENCE_DB {
                "-inf dB".to_owned()
            } else {
                format!("{v:.1} dB")
            }
        })
}

/// Turns a fader drag into live engine commands and one undo step at the end.
pub fn live_mixer(
    view: &mut TimelineView,
    track: TrackId,
    response: &egui::Response,
    mixer: Mixer,
    label: &'static str,
) {
    if response.changed() {
        view.push(Action::LiveMixer {
            track: Some(track),
            mixer,
        });
    }
    if response.drag_stopped()
        || (response.changed() && !response.dragged())
        || response.lost_focus()
    {
        view.push(Action::EndLive(label));
    }
}

pub fn format_pan(pan: f32) -> String {
    let percent = (pan * 50.0).round() as i32;
    match percent {
        0 => "C".to_owned(),
        p if p < 0 => format!("{}L", -p),
        p => format!("{p}R"),
    }
}

fn parse_pan(text: &str) -> Option<f64> {
    let text = text.trim().to_uppercase();
    if text == "C" {
        return Some(0.0);
    }
    if let Some(left) = text.strip_suffix('L') {
        return left.trim().parse::<f64>().ok().map(|v| -v);
    }
    if let Some(right) = text.strip_suffix('R') {
        return right.trim().parse().ok();
    }
    text.parse().ok()
}

fn paint_meter(painter: &egui::Painter, rect: Rect, peaks: [f32; 2]) {
    let area = Rect::from_min_max(
        pos2(rect.right() - METER_WIDTH, rect.top() + 4.0),
        pos2(rect.right() - 2.0, rect.bottom() - 4.0),
    );
    let half = (area.width() - 1.0) / 2.0;
    for (i, peak) in peaks.into_iter().enumerate() {
        let x = area.left() + i as f32 * (half + 1.0);
        let bar = Rect::from_min_max(pos2(x, area.top()), pos2(x + half, area.bottom()));
        painter.rect_filled(bar, 0.0, Color32::from_black_alpha(90));
        let db = dsp::gain_to_db(peak).clamp(-60.0, 6.0);
        let level = (db + 60.0) / 66.0;
        if level > 0.0 {
            let top = bar.bottom() - level * bar.height();
            let color = if peak >= 1.0 {
                color::METER_HIGH
            } else if db > -6.0 {
                color::METER_MID
            } else {
                color::METER_LOW
            };
            painter.rect_filled(
                Rect::from_min_max(pos2(bar.left(), top), bar.max),
                0.0,
                color,
            );
        }
    }
}

/// Effects dragged from the device view onto the header go to the end of
/// the track's chain (with Ctrl, copies of them).
fn drop_effects(
    view: &mut TimelineView,
    ui: &egui::Ui,
    track: &Track,
    response: &egui::Response,
    rect: Rect,
) {
    let Some(drag) = egui::DragAndDrop::payload::<crate::effect_rack::DeviceDrag>(ui.ctx()) else {
        return;
    };
    if !response.contains_pointer() {
        return;
    }
    ui.painter().rect_stroke(
        rect.shrink(1.0),
        radius::CONTROL,
        Stroke::new(stroke::INDICATOR, theme::accent(ui.visuals())),
        egui::StrokeKind::Inside,
    );
    if let Some(pointer) = ui.ctx().pointer_hover_pos() {
        let copy = ui.input(|i| i.modifiers.command);
        let verb = if copy { "Copy" } else { "Move" };
        chrome::drop_label(ui.ctx(), pointer, &format!("{verb} to {}", track.name));
    }
    if ui.input(|i| i.pointer.any_released()) {
        let copy = ui.input(|i| i.modifiers.command);
        egui::DragAndDrop::clear_payload(ui.ctx());
        view.push(Action::DropDevices {
            ids: drag.ids.clone(),
            to: Some(track.id),
            copy,
        });
    }
}

fn handle_header_response(
    view: &mut TimelineView,
    ui: &egui::Ui,
    session: &Session,
    track: &Track,
    response: &egui::Response,
) {
    let modifiers = ui.input(|i| i.modifiers);
    if response.clicked() {
        view.push(Action::SelectTrack {
            track: track.id,
            mode: SelectMode::from_modifiers(modifiers),
        });
    }
    if response.double_clicked() {
        view.renaming = Some((track.id, track.name.clone()));
    }

    if response.drag_started() {
        let ids = if view.selection.has_track(track.id) {
            view.selection.tracks.clone()
        } else {
            view.push(Action::SelectTrack {
                track: track.id,
                mode: SelectMode::Replace,
            });
            vec![track.id]
        };
        view.gesture = Some(Gesture::MoveTracks { ids, target: None });
    }
    let _ = session;
    response.context_menu(|ui| context_menu(view, ui, track));
}

fn context_menu(view: &mut TimelineView, ui: &mut egui::Ui, track: &Track) {
    let selected = if view.selection.has_track(track.id) {
        view.selection.tracks.clone()
    } else {
        vec![track.id]
    };
    if ui.button("Rename").clicked() {
        view.renaming = Some((track.id, track.name.clone()));
        ui.close();
    }
    ui.menu_button("Color", |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.set_max_width(6.0 * 22.0);
            for color in Color::PALETTE {
                let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                ui.painter().rect_filled(rect, 3.0, color32(color));
                if color == track.color {
                    ui.painter().rect_stroke(
                        rect,
                        3.0,
                        Stroke::new(2.0, Color32::WHITE),
                        egui::StrokeKind::Inside,
                    );
                }
                if response.clicked() {
                    for id in &selected {
                        view.push(Action::SetTrackColor(*id, color));
                    }
                    ui.close();
                }
            }
        });
    });
    ui.separator();
    if chrome::menu_item(ui, "Group Tracks", "Ctrl+G").clicked() {
        view.push(Action::GroupTracks(selected.clone()));
        ui.close();
    }
    if track.is_group() && chrome::menu_item(ui, "Ungroup", "Ctrl+Shift+G").clicked() {
        view.push(Action::Ungroup(track.id));
        ui.close();
    }
    ui.menu_button("Automation", |ui| {
        let targets = automation_targets(track);
        grouped_menu(ui, &targets, &mut |ui, choice| {
            let shown = view
                .automation
                .get(&track.id)
                .is_some_and(|l| l.contains(&choice.target));
            if ui.selectable_label(shown, &choice.label).clicked() {
                let target = choice.target.clone();
                view.push(if shown {
                    Action::HideAutomation(track.id, target)
                } else {
                    Action::ShowAutomation(track.id, target)
                });
                ui.close();
            }
        });
    });
    if ui.button("Export…").clicked() {
        view.export_requested = Some(selected.clone());
        ui.close();
    }
    ui.separator();
    if chrome::menu_item(ui, "Delete", "Del").clicked() {
        view.push(Action::DeleteTracks(selected));
        ui.close();
    }
}

/// Something on a track that automation can drive, as menus list it.
pub struct AutomationChoice {
    pub target: AutomationTarget,
    /// The submenus it sits in, outermost first ("" for none), and its
    /// name within them.
    pub groups: [&'static str; 2],
    pub label: String,
}

/// Everything on a track that automation can drive, with display names.
pub fn automation_targets(track: &Track) -> Vec<AutomationChoice> {
    let plain = |target, name: &str| AutomationChoice {
        target,
        groups: ["", ""],
        label: name.to_owned(),
    };
    let mut targets = vec![
        plain(AutomationTarget::Volume, "Volume"),
        plain(AutomationTarget::Pan, "Pan"),
    ];
    if let TrackKind::Midi { instrument, .. } = &track.kind
        && let Some(descriptor) = instrument.descriptor()
    {
        let plugin = hosting::describe::param_groups(instrument.format, &instrument.id);
        let first = targets.len();
        for (index, param) in descriptor.params.iter().enumerate() {
            let groups = match plugin {
                Some(table) => table.get(index).copied().unwrap_or(["", ""]),
                None => (descriptor.groups)(index),
            };
            targets.push(AutomationChoice {
                target: AutomationTarget::InstrumentParam(param.id.to_owned()),
                groups,
                label: shorten(param.name, groups).to_owned(),
            });
        }
        if plugin.is_some() {
            sort_groups(&mut targets[first..]);
        }
    }
    for device in &track.devices {
        let Some(descriptor) = device.effect.descriptor() else {
            continue;
        };
        let plugin = hosting::describe::param_groups(device.effect.format, &device.effect.id)
            .filter(|table| table.iter().any(|g| !g[0].is_empty()));
        let first = targets.len();
        for (index, param) in descriptor.params.iter().enumerate() {
            let target = AutomationTarget::DeviceParam {
                device: device.id,
                param: param.id.to_owned(),
            };
            match plugin {
                // A plugin's many parameters in a submenu of its own.
                Some(table) => {
                    let [outer, _] = table.get(index).copied().unwrap_or(["", ""]);
                    let groups = [descriptor.name, outer];
                    targets.push(AutomationChoice {
                        target,
                        groups,
                        label: shorten(param.name, [outer, ""]).to_owned(),
                    });
                }
                _ => targets.push(plain(
                    target,
                    &format!("{}: {}", descriptor.name, param.name),
                )),
            }
        }
        if plugin.is_some() {
            sort_groups(&mut targets[first..]);
        }
    }
    targets
}

/// A parameter's name within its submenus: "Kick Layer 1 Cutoff" is
/// "Cutoff" in Kick ▸ Layer 1, and "Oscillator 2 Detune" is "Detune" in
/// Oscillator ▸ Oscillator 2.
fn shorten(name: &'static str, groups: [&str; 2]) -> &'static str {
    if let Some(inner) = groups.iter().rev().find(|g| !g.is_empty())
        && let Some(rest) = name.strip_prefix(inner)
        && !rest.trim().is_empty()
    {
        return rest.trim_start();
    }
    let mut label = name;
    for group in groups.iter().filter(|g| !g.is_empty()) {
        label = label.strip_prefix(group).map_or(label, str::trim_start);
    }
    if label.is_empty() { name } else { label }
}

/// Puts a plugin's choices in group order (numbers counted, so
/// "Oscillator 2" comes before "Oscillator 10"), since menus gather only
/// neighbours into a submenu; ungrouped ones stay first, in the plugin's
/// order.
fn sort_groups(choices: &mut [AutomationChoice]) {
    choices.sort_by(|a, b| {
        let key = |c: &AutomationChoice| (!c.groups[0].is_empty(), c.groups[0], c.groups[1]);
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0)
            .then_with(|| natural(ka.1, kb.1))
            .then_with(|| natural(ka.2, kb.2))
    });
}

/// Compares text with runs of digits as numbers.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> Vec<(String, u64)> {
        let mut parts = Vec::new();
        let mut text = String::new();
        let mut digits = String::new();
        for c in s.chars() {
            if c.is_ascii_digit() {
                digits.push(c);
            } else {
                if !digits.is_empty() {
                    parts.push((
                        std::mem::take(&mut text),
                        digits.parse().unwrap_or(u64::MAX),
                    ));
                    digits.clear();
                }
                text.push(c);
            }
        }
        parts.push((text, digits.parse().unwrap_or(0)));
        parts
    };
    split(a).cmp(&split(b))
}

/// What [`automation_targets`] calls `target`, without listing them all.
pub fn automation_target_name(track: &Track, target: &AutomationTarget) -> String {
    match target {
        AutomationTarget::Volume => "Volume".to_owned(),
        AutomationTarget::Pan => "Pan".to_owned(),
        AutomationTarget::InstrumentParam(id) => match &track.kind {
            TrackKind::Midi { instrument, .. } => instrument
                .descriptor()
                .and_then(|d| d.params.iter().find(|p| p.id == id.as_str()))
                .map_or_else(|| "?".to_owned(), |p| p.name.to_owned()),
            _ => "?".to_owned(),
        },
        AutomationTarget::DeviceParam { device, param } => track
            .devices
            .iter()
            .find(|d| d.id == *device)
            .and_then(|d| d.effect.descriptor())
            .and_then(|d| {
                let p = d.params.iter().find(|p| p.id == param.as_str())?;
                Some(format!("{}: {}", d.name, p.name))
            })
            .unwrap_or_else(|| "?".to_owned()),
        AutomationTarget::ModulatorParam { modulator, param } => {
            track.modulator(*modulator).map_or_else(
                || "?".to_owned(),
                |m| format!("{}: {param}", m.name.as_deref().unwrap_or(&m.kind.id)),
            )
        }
    }
}

/// `choices` as menu items, in submenus by their groups; `item` draws each.
pub fn grouped_menu(
    ui: &mut egui::Ui,
    choices: &[AutomationChoice],
    item: &mut dyn FnMut(&mut egui::Ui, &AutomationChoice),
) {
    let all: Vec<&AutomationChoice> = choices.iter().collect();
    grouped_level(ui, &all, 0, item);
}

fn grouped_level(
    ui: &mut egui::Ui,
    choices: &[&AutomationChoice],
    depth: usize,
    item: &mut dyn FnMut(&mut egui::Ui, &AutomationChoice),
) {
    let group_of = |choice: &AutomationChoice| choice.groups.get(depth).copied().unwrap_or("");
    let mut i = 0;
    while i < choices.len() {
        let group = group_of(choices[i]);
        if group.is_empty() {
            item(ui, choices[i]);
            i += 1;
            continue;
        }
        let end = i + choices[i..]
            .iter()
            .take_while(|c| group_of(c) == group)
            .count();
        let members = &choices[i..end];
        ui.menu_button(group, |ui| {
            egui::ScrollArea::vertical()
                .max_height(ui.ctx().content_rect().height() * 0.8)
                .show(ui, |ui| grouped_level(ui, members, depth + 1, item));
        });
        i = end;
    }
}

/// Where dragged tracks would land if dropped at screen height `y`.
pub(super) fn drop_target(
    view: &TimelineView,
    project: &Project,
    y: f32,
    moving: &[TrackId],
) -> Option<TrackDrop> {
    let rows: Vec<_> = view.track_rows().copied().collect();
    let last = rows.last()?;
    if y > last.rect.bottom() {
        return Some(TrackDrop {
            position: TrackPosition::END,
            indicator: DropIndicator::Line(last.rect.bottom()),
        });
    }
    let row = rows.iter().find(|r| r.rect.y_range().contains(y))?;
    let relative = (y - row.rect.top()) / row.rect.height();
    let inside_moving = moving
        .iter()
        .any(|m| *m == row.track || project.is_inside(row.track, *m));
    if row.kind == RowKind::Group && (0.3..0.7).contains(&relative) && !inside_moving {
        return Some(TrackDrop {
            position: TrackPosition {
                parent: Some(row.track),
                before: None,
            },
            indicator: DropIndicator::Into(row.rect),
        });
    }
    if relative < 0.5 {
        return Some(TrackDrop {
            position: TrackPosition {
                parent: project.parent(row.track),
                before: Some(row.track),
            },
            indicator: DropIndicator::Line(row.rect.top()),
        });
    }
    // Below an open group's header means its first child slot.
    let track = project.track(row.track)?;
    let position = match track.children().first() {
        Some(first) if !track.is_folded() && !inside_moving => TrackPosition {
            parent: Some(track.id),
            before: Some(first.id),
        },
        _ => TrackPosition::after(project, row.track),
    };
    Some(TrackDrop {
        position,
        indicator: DropIndicator::Line(row.rect.bottom()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_text() {
        assert_eq!(format_pan(0.0), "C");
        assert_eq!(format_pan(-0.5), "25L");
        assert_eq!(format_pan(1.0), "50R");
        assert_eq!(parse_pan("25l"), Some(-25.0));
        assert_eq!(parse_pan("c"), Some(0.0));
        assert_eq!(parse_pan("10"), Some(10.0));
    }

    #[test]
    fn names_shorten_by_the_submenus_they_sit_in() {
        assert_eq!(
            shorten("Kick Layer 1 Cutoff", ["Kick", "Layer 1"]),
            "Cutoff"
        );
        assert_eq!(
            shorten("Oscillator 2 Detune", ["Oscillator", "Oscillator 2"]),
            "Detune"
        );
        assert_eq!(shorten("Attack", ["Dynamics", "Compressor"]), "Attack");
        assert_eq!(shorten("Oscillator", ["Oscillator", ""]), "Oscillator");
    }

    #[test]
    fn numbered_groups_sort_as_numbers() {
        use std::cmp::Ordering::Less;
        assert_eq!(natural("Oscillator 2", "Oscillator 10"), Less);
        assert_eq!(natural("LFO", "Oscillator 1"), Less);
        assert_eq!(natural("Env 9 Attack", "Env 10 Attack"), Less);
    }
}
