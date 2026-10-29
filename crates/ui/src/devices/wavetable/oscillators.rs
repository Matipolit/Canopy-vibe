//! The oscillators page: the three wavetable oscillators side by side, the
//! sub and noise, then the filters.
//!
//! Each oscillator's display shows its table as Serum's does: in 3D, every
//! frame stacked from the first (front left) to the last (back right), the
//! frame playing lit where modulation has the position now, morphed and
//! warped as it plays; or that one frame flat (2D); or its harmonics
//! (SP). Drag it sideways to move through the table; drop a WAV file on it
//! for the oscillator's own table.
//!
//! When a warp slot remaps (Serum's Remap warps), a Curve button by the
//! warp menus opens the oscillator's remap curve: where in the cycle each
//! moment of it reads, drawn as nodes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};
use instruments::builtin::wavetable as wt;
use project::TrackId;

use super::super::panel::{DISPLAY_INSET, display};
use super::super::{Owner, Panel};
use super::knobs::{self, Live, choice_menu, knob_row, mod_knob};
use super::{editor, filters};
use crate::banks::TableState;
use crate::session::Session;
use crate::theme::{self, radius, size, space, stroke, text};
use crate::widgets::knob;
use crate::widgets::node_editor::{self, Node, NodeEditor, Preset};

/// Points drawn per oscillator frame.
const FRAME_POINTS: usize = 160;
/// Frames drawn behind the playing one in the 3D view, at most.
const DEPTH_FRAMES: usize = 24;
/// Harmonics shown in the spectrum view.
const HARMONICS: usize = 64;
/// How tall an oscillator's display is (and the filters' beside them).
pub(super) const DISPLAY_HEIGHT: f32 = 58.0;

/// The oscillators' names, by oscillator.
pub(super) const OSC_NAMES: [&str; wt::OSC_COUNT] = ["A", "B", "C"];

/// How an oscillator's display draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum View {
    #[default]
    Table,
    Wave,
    Spectrum,
}

pub(super) fn page(ui: &mut Ui, panel: &mut Panel, live: &Live) {
    for osc in 0..wt::OSC_COUNT {
        oscillator(ui, panel, live, osc);
        ui.separator();
    }
    sub_and_noise(ui, panel, live);
    ui.separator();
    filters::section(ui, panel, live);
}

/// The warp menu's names: the labels, with the cross-modulation ones short,
/// since what they listen to has its own menu.
fn warp_names(osc: usize) -> Vec<&'static str> {
    let labels = match osc {
        0 => wt::WARPS_A,
        1 => wt::WARPS_B,
        _ => wt::WARPS_C,
    };
    labels
        .iter()
        .enumerate()
        .map(|(i, label)| match i {
            wt::WARP_FM => "FM",
            wt::WARP_AM => "AM",
            wt::WARP_RM => "RM",
            _ => *label,
        })
        .collect()
}

/// What oscillator `osc`'s "Auto" cross-modulation source is, by name.
fn auto_source(osc: usize) -> &'static str {
    ["from B", "from A", "from A"][osc.min(2)]
}

fn oscillator(ui: &mut Ui, panel: &mut Panel, live: &Live, osc: usize) {
    let p = |offset: usize| wt::osc_param(osc, offset);
    let name = OSC_NAMES[osc];
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            knobs::switch(ui, panel, p(wt::OSC_ON), &format!("Osc {name}"));
            table_menu(ui, panel, osc);
            load_button(ui, panel, osc);
            if ui
                .add(button("Edit…"))
                .on_hover_text("Draw, morph and save this oscillator's table")
                .clicked()
            {
                editor::open(ui, panel, osc);
            }
            ui.separator();
            let names = warp_names(osc);
            for mode in [wt::OSC_WARP_MODE, wt::OSC_WARP2_MODE] {
                choice_menu(ui, panel, p(mode), &names, 10);
            }
            let values = panel.values();
            let modes = [wt::OSC_WARP_MODE, wt::OSC_WARP2_MODE].map(|m| values[p(m)] as usize);
            let cross = modes.iter().any(|&m| wt::is_cross_mod(m));
            if modes.iter().any(|&m| wt::is_remap(m)) {
                remap_menu(ui, panel, osc);
            }
            if cross {
                let sources = [
                    auto_source(osc),
                    "from A",
                    "from B",
                    "from C",
                    "from Sub",
                    "from Noise",
                ];
                choice_menu(ui, panel, p(wt::OSC_MOD_SOURCE), &sources, 6);
            }
            ui.separator();
            choice_menu(ui, panel, p(wt::OSC_MORPH_MODE), &["No Morph"], 6);
            options_menu(ui, panel, osc);
        });
        // As wide as the rows of nine knobs under it.
        let width = 9.0 * knob::width(panel.knob_size);
        wave_display(ui, panel, live, osc, vec2(width, DISPLAY_HEIGHT));
        knob_row(
            ui,
            panel,
            live,
            &[
                (p(wt::OSC_POSITION), "Position"),
                (p(wt::OSC_WARP), "Warp"),
                (p(wt::OSC_WARP2), "Warp 2"),
                (p(wt::OSC_MORPH), "Morph"),
                (p(wt::OSC_LEVEL), "Level"),
                (p(wt::OSC_PAN), "Pan"),
                (p(wt::OSC_OCTAVE), "Octave"),
                (p(wt::OSC_SEMI), "Semi"),
                (p(wt::OSC_FINE), "Fine"),
            ],
        );
        knob_row(
            ui,
            panel,
            live,
            &[
                (p(wt::OSC_UNISON), "Unison"),
                (p(wt::OSC_DETUNE), "Detune"),
                (p(wt::OSC_BLEND), "Blend"),
                (p(wt::OSC_WIDTH), "Width"),
                (p(wt::OSC_STACK), "Stack"),
                (p(wt::OSC_WARP_SPREAD), "Warp Spr"),
                (p(wt::OSC_POSITION_SPREAD), "Pos Spr"),
                (p(wt::OSC_PHASE), "Phase"),
                (p(wt::OSC_RANDOM), "Random"),
            ],
        );
    });
}

/// Ready-made remap curves: where in the cycle each moment of it reads.
const REMAP_PRESETS: &[Preset] = &[
    (
        "Straight",
        &[Node::new(0.0, 0.0, 0.0), Node::new(1.0, 1.0, 0.0)],
    ),
    (
        "Slow Then Fast",
        &[Node::new(0.0, 0.0, 0.6), Node::new(1.0, 1.0, 0.0)],
    ),
    (
        "Fast Then Slow",
        &[Node::new(0.0, 0.0, -0.6), Node::new(1.0, 1.0, 0.0)],
    ),
    (
        "Rush the Middle",
        &[
            Node::new(0.0, 0.0, 0.6),
            Node::new(0.5, 0.5, -0.6),
            Node::new(1.0, 1.0, 0.0),
        ],
    ),
    (
        "Linger in the Middle",
        &[
            Node::new(0.0, 0.0, -0.6),
            Node::new(0.5, 0.5, 0.6),
            Node::new(1.0, 1.0, 0.0),
        ],
    ),
    (
        "Backwards",
        &[Node::new(0.0, 1.0, 0.0), Node::new(1.0, 0.0, 0.0)],
    ),
    (
        "There and Back",
        &[
            Node::new(0.0, 0.0, 0.0),
            Node::new(0.5, 1.0, 0.0),
            Node::new(1.0, 0.0, 0.0),
        ],
    ),
    (
        "Twice (Octave Up)",
        &[
            Node::new(0.0, 0.0, 0.0),
            Node::new(0.5, 1.0, 0.0),
            Node::new(0.5, 0.0, 0.0),
            Node::new(1.0, 1.0, 0.0),
        ],
    ),
    (
        "Wait, Then Play",
        &[
            Node::new(0.0, 0.0, 0.0),
            Node::new(0.4, 0.0, 0.0),
            Node::new(1.0, 1.0, 0.0),
        ],
    ),
    (
        "Steps",
        &[
            Node::new(0.0, 0.0, 0.0),
            Node::new(0.25, 0.0, 0.0),
            Node::new(0.25, 0.25, 0.0),
            Node::new(0.5, 0.25, 0.0),
            Node::new(0.5, 0.5, 0.0),
            Node::new(0.75, 0.5, 0.0),
            Node::new(0.75, 0.75, 0.0),
            Node::new(1.0, 0.75, 0.0),
        ],
    ),
];

/// Oscillator `osc`'s remap curve as editable nodes.
fn remap_nodes_of(values: &[f32], osc: usize) -> Vec<Node> {
    let nodes = wt::remap_nodes(values, osc);
    (0..nodes.count)
        .map(|i| {
            let curve = values[wt::osc_param(osc, wt::OSC_REMAP_CURVE + i)];
            Node::new(nodes.x[i], nodes.y[i], curve)
        })
        .collect()
}

/// A button opening oscillator `osc`'s remap curve in a node editor, with a
/// grid to snap to: across, the cycle as it plays; up, where it reads.
fn remap_menu(ui: &mut Ui, panel: &mut Panel, osc: usize) {
    let config = egui::containers::menu::MenuConfig::new()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    egui::containers::menu::MenuButton::new("Curve")
        .config(config)
        .ui(ui, |ui| {
            let values = panel.values().to_vec();
            let nodes = remap_nodes_of(&values, osc);
            let remap = wt::remap_table(&values, osc);
            let last = remap.len() - 1;
            let curve = |x: f32| remap[((x * last as f32) as usize).min(last)];
            let snap_id = panel.id(("remap snap", osc));
            let mut snap: bool = ui.data(|d| d.get_temp(snap_id)).unwrap_or(true);
            ui.label(
                egui::RichText::new(format!(
                    "Osc {} remap: where each moment reads",
                    OSC_NAMES[osc]
                ))
                .weak(),
            );
            let response = NodeEditor::new(panel.id(("remap", osc)), &nodes, &curve)
                .size(vec2(240.0, 170.0))
                .color(theme::accent(ui.visuals()))
                .grid(8, 8, snap)
                .max_nodes(wt::MAX_REMAP_NODES)
                // A menu opened inside this popup would close it: the
                // shapes are in their own submenu instead.
                .context_menu(false)
                .show(ui);
            let mut changed = response.nodes;
            let mut finished = response.finished;
            ui.horizontal(|ui| {
                ui.toggle_value(&mut snap, "Snap")
                    .on_hover_text("Nodes land on the grid (Shift moves freely)");
                ui.menu_button("Shapes", |ui| {
                    let chosen =
                        node_editor::shape_menu(ui, &nodes, REMAP_PRESETS, wt::MAX_REMAP_NODES);
                    if chosen.is_some() {
                        changed = chosen;
                        finished = true;
                    }
                });
            });
            if let Some(changed) = changed {
                set_remap_nodes(panel, osc, &changed);
            }
            if finished {
                panel.finish();
            }
            ui.data_mut(|d| d.insert_temp(snap_id, snap));
        })
        .0
        .on_hover_text("The remap curve the Remap warps read through");
}

/// Stores `nodes` as oscillator `osc`'s remap curve.
fn set_remap_nodes(panel: &mut Panel, osc: usize, nodes: &[Node]) {
    let p = |offset: usize| wt::osc_param(osc, offset);
    let count = nodes.len().clamp(2, wt::MAX_REMAP_NODES);
    panel.set(p(wt::OSC_REMAP_NODES), (count - 2) as f32);
    for (i, node) in nodes.iter().take(count).enumerate() {
        panel.set(p(wt::OSC_REMAP_X + i), node.x);
        panel.set(p(wt::OSC_REMAP_Y + i), node.y);
        panel.set(p(wt::OSC_REMAP_CURVE + i), node.curve);
    }
}

/// The remap curve oscillator `osc`'s display draws through: its own if a
/// warp slot remaps, else a straight one (nothing reads it).
fn display_remap(values: &[f32], osc: usize) -> Box<wt::Remap> {
    let modes = [wt::OSC_WARP_MODE, wt::OSC_WARP2_MODE];
    if modes
        .iter()
        .any(|&m| wt::is_remap(values[wt::osc_param(osc, m)] as usize))
    {
        Box::new(wt::remap_table(values, osc))
    } else {
        Box::new(wt::STRAIGHT_REMAP)
    }
}

/// The oscillator's table: the built-in tables in columns, then its own.
fn table_menu(ui: &mut Ui, panel: &mut Panel, osc: usize) {
    let index = wt::osc_param(osc, wt::OSC_TABLE);
    let current = panel.values()[index] as usize;
    let name = wt::TABLE_LABELS.get(current).copied().unwrap_or("?");
    let text = match panel.name_color(index) {
        Some(color) => egui::RichText::new(name).color(color),
        None => egui::RichText::new(name),
    };
    let mut chosen = None;
    let mut response = ui
        .menu_button(text, |ui| {
            ui.horizontal_top(|ui| {
                let tables: Vec<usize> = (0..wt::tables().len()).collect();
                for column in tables.chunks(8) {
                    ui.vertical(|ui| {
                        for &table in column {
                            let choice = wt::choice_of(table);
                            if ui
                                .selectable_label(current == choice, wt::tables()[table])
                                .clicked()
                            {
                                chosen = Some(choice);
                                ui.close();
                            }
                        }
                    });
                }
            });
            ui.separator();
            let user = ui
                .selectable_label(current == wt::USER_TABLE, "User (its own WAV)")
                .on_hover_text("The table loaded from a file for this oscillator");
            if user.clicked() {
                chosen = Some(wt::USER_TABLE);
                ui.close();
            }
        })
        .response
        .on_hover_text(panel.params[index].name);
    if let Some(choice) = chosen {
        panel.set(index, choice as f32);
        response.mark_changed();
    }
    knobs::attach_choice(panel, &response, index);
}

/// The oscillator's rarer settings: how its unison detunes, whether it
/// follows the keys, and what its cross-modulation hears.
fn options_menu(ui: &mut Ui, panel: &mut Panel, osc: usize) {
    let p = |offset: usize| wt::osc_param(osc, offset);
    ui.menu_button("…", |ui| {
        ui.label(heading("Unison detune"));
        let mode = panel.values()[p(wt::OSC_DETUNE_MODE)] as usize;
        for (i, name) in wt::DETUNE_MODES.iter().enumerate() {
            if ui.selectable_label(mode == i, *name).clicked() {
                panel.set(p(wt::OSC_DETUNE_MODE), i as f32);
                panel.finish();
            }
        }
        ui.separator();
        let mut keytrack = panel.values()[p(wt::OSC_KEYTRACK)] >= 0.5;
        if ui.checkbox(&mut keytrack, "Follows the keys").changed() {
            panel.set(p(wt::OSC_KEYTRACK), if keytrack { 1.0 } else { 0.0 });
            panel.finish();
        }
        ui.separator();
        ui.label(heading("FM, AM and RM listen to"));
        let source = panel.values()[p(wt::OSC_MOD_SOURCE)] as usize;
        for (i, name) in wt::MOD_SOURCES.iter().enumerate() {
            let name = if i == 0 {
                format!("Auto ({})", auto_source(osc))
            } else {
                (*name).to_owned()
            };
            if ui.selectable_label(source == i, name).clicked() {
                panel.set(p(wt::OSC_MOD_SOURCE), i as f32);
                panel.finish();
            }
        }
    })
    .response
    .on_hover_text("Unison detune mode, key tracking and what FM listens to");
}

/// A weak heading inside a menu.
fn heading(text: &str) -> egui::RichText {
    egui::RichText::new(text).weak()
}

/// The track a Wavetable panel belongs to.
pub(super) fn track_of(panel: &Panel) -> Option<TrackId> {
    match panel.owner {
        Owner::Instrument(track) => Some(track),
        Owner::Device(_) => None,
    }
}

/// Oscillator `osc`'s user wavetable file, if it has one.
pub(super) fn table_file(panel: &Panel, osc: usize) -> Option<PathBuf> {
    let track = panel.session.project.track(track_of(panel)?)?;
    track.wavetable_file(osc).map(Path::to_owned)
}

pub(super) fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Picks a WAV file for oscillator `osc`'s own table, and plays it.
fn pick_table(panel: &mut Panel, osc: usize) {
    let Some(track) = track_of(panel) else {
        return;
    };
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Wavetable (WAV)", &["wav", "WAV", "wave"])
        .pick_file()
    {
        panel.session.load_wavetable(track, osc, path);
    }
}

/// Loads a WAV file as the oscillator's own table and switches it to
/// "User"; its hover says which file it has.
fn load_button(ui: &mut Ui, panel: &mut Panel, osc: usize) {
    let hover = match table_file(panel, osc) {
        Some(path) => format!(
            "Load a WAV wavetable for this oscillator (it has {}), or drop one on its display",
            file_name(&path)
        ),
        None => "Load a WAV wavetable for this oscillator (2048 samples a frame, as Serum \
                 and Vital write them), or drop one on its display"
            .to_owned(),
    };
    if ui.add(button("Load…")).on_hover_text(hover).clicked() {
        pick_table(panel, osc);
    }
}

/// A button in an oscillator's header, as tall as the menus beside it.
fn button(text: &str) -> egui::Button<'_> {
    egui::Button::new(text).min_size(vec2(0.0, size::CONTROL))
}

/// What an oscillator's display draws: a built-in table, or its user table.
#[derive(Clone)]
pub(super) enum Drawn {
    BuiltIn(usize),
    User(Arc<wt::UserTable>),
}

impl Drawn {
    pub fn source(&self) -> wt::TableSource<'_> {
        match self {
            Drawn::BuiltIn(table) => wt::TableSource::BuiltIn(*table),
            Drawn::User(table) => wt::TableSource::User(table),
        }
    }

    /// Something that tells tables apart, for caching what was drawn.
    fn identity(&self) -> usize {
        match self {
            Drawn::BuiltIn(table) => *table,
            Drawn::User(table) => Arc::as_ptr(table) as usize,
        }
    }
}

/// What oscillator `osc` plays (as the synth resolves "User": its own
/// table if loaded, else the first built-in), and what the display says
/// about it, in the error color if `true`.
pub(super) fn drawn_table(panel: &Panel, osc: usize, live: f32) -> (Drawn, String, bool) {
    let choice = panel.values()[wt::osc_param(osc, wt::OSC_TABLE)] as usize;
    let percent = format!("{:.0}%", live * 100.0);
    if let Some(table) = wt::table_of(choice) {
        let name = wt::tables().get(table).copied().unwrap_or("?");
        return (Drawn::BuiltIn(table), format!("{name} · {percent}"), false);
    }
    let fallback = Drawn::BuiltIn(0);
    let Some(path) = table_file(panel, osc) else {
        let text = format!("User: no file, plays {}", wt::tables()[0]);
        return (fallback, text, false);
    };
    let name = file_name(&path);
    let state = track_of(panel).and_then(|t| panel.session.wavetable(t, osc));
    match state {
        Some(TableState::Ready(table)) => {
            let frames = table.frames();
            let text = format!(
                "{name} · {frames} frame{} · {percent}",
                if frames == 1 { "" } else { "s" }
            );
            (Drawn::User(table.clone()), text, false)
        }
        Some(TableState::Failed(err)) => (fallback, format!("{name}: {err}"), true),
        Some(TableState::Loading) | None => (fallback, format!("Loading {name}…"), false),
    }
}

/// Where oscillators' displays were drawn, for file drops: the visible part
/// of each, with its track and oscillator, and the pass they were drawn in.
#[derive(Clone, Default)]
struct DropTargets {
    pass: u64,
    targets: Vec<(Rect, TrackId, usize)>,
}

fn drop_targets_id() -> egui::Id {
    egui::Id::new("wavetable drop targets")
}

fn register_drop_target(ui: &Ui, rect: Rect, track: TrackId, osc: usize) {
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
            drops.targets.push((rect, track, osc));
        }
    });
}

/// The oscillator whose display is at `pos`, as drawn in the last pass (or
/// this one).
fn drop_target(ctx: &egui::Context, pos: Pos2) -> Option<(TrackId, usize)> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| {
        let drops = d.get_temp::<DropTargets>(drop_targets_id())?;
        if drops.pass + 1 < pass {
            return None;
        }
        let (_, track, osc) = drops.targets.iter().find(|(rect, ..)| rect.contains(pos))?;
        Some((*track, *osc))
    })
}

/// Files dropped at `pos`: if that's on an oscillator's display (as drawn
/// last frame) and the first is an audio file, it becomes the oscillator's
/// own table, and the drop is taken. Otherwise false: the drop is someone
/// else's.
pub(crate) fn take_drop(
    ctx: &egui::Context,
    session: &mut Session,
    pos: Option<Pos2>,
    paths: &[PathBuf],
) -> bool {
    let Some((track, osc)) = pos.and_then(|pos| drop_target(ctx, pos)) else {
        return false;
    };
    let Some(path) = paths.first().filter(|p| media::is_supported_extension(p)) else {
        return false;
    };
    session.load_wavetable(track, osc, path.clone());
    true
}

/// Oscillator `osc`'s two warps and morph where modulation has them now:
/// `[(mode, amount); 2]` and `(mode, amount, note Hz)`.
pub(super) fn live_shape(
    panel: &Panel,
    live: &Live,
    osc: usize,
) -> ([(usize, f32); 2], (usize, f32, f32)) {
    let p = |offset: usize| wt::osc_param(osc, offset);
    let values = panel.values();
    let warps = [
        (
            values[p(wt::OSC_WARP_MODE)] as usize,
            live.param(panel, p(wt::OSC_WARP)),
        ),
        (
            values[p(wt::OSC_WARP2_MODE)] as usize,
            live.param(panel, p(wt::OSC_WARP2)),
        ),
    ];
    let hz = 440.0 * ((live.pitch() - 69.0) / 12.0).exp2();
    let morph = (
        values[p(wt::OSC_MORPH_MODE)] as usize,
        live.param(panel, p(wt::OSC_MORPH)),
        hz,
    );
    (warps, morph)
}

/// The 3D view's frames behind the playing one, as drawn for the settings
/// in `key`: kept between frames, since they only change with the settings.
#[derive(Clone, Default)]
struct DepthCache {
    key: Vec<u64>,
    frames: Vec<Vec<f32>>,
}

/// The oscillator's table in 3D, 2D or as a spectrum (see the module
/// docs). Drag sideways to move through the table; drop a WAV file on it
/// (or right-click it) for the oscillator's own table.
fn wave_display(ui: &mut Ui, panel: &mut Panel, live: &Live, osc: usize, size: Vec2) {
    let p = |offset: usize| wt::osc_param(osc, offset);
    let (response, painter) = display(ui, size, Sense::click_and_drag());
    #[cfg(test)]
    ui.ctx()
        .data_mut(|d| d.insert_temp(panel.id(("wave", osc)).with("rect"), response.rect));
    let view_id = panel.id(("wave view", osc));
    let mut view: View = ui.data(|d| d.get_temp(view_id)).unwrap_or_default();
    let rect = response.rect.shrink2(vec2(space::M, space::S + space::XS));
    let visuals = ui.visuals().clone();
    let accent = theme::accent(&visuals);
    let position_now = panel.meters[wt::METER_POSITION + osc];
    let (drawn, label, failed) = drawn_table(panel, osc, position_now);
    let values = panel.values().to_vec();
    let on = values[p(wt::OSC_ON)] >= 0.5;
    let (warps, morph) = live_shape(panel, live, osc);
    let color = if on {
        accent
    } else {
        visuals.weak_text_color()
    };
    let remap = display_remap(&values, osc);
    let mut samples = [0.0f32; FRAME_POINTS];
    drawn
        .source()
        .draw_remapped(position_now, morph, warps, &remap, &mut samples);
    match view {
        View::Table => {
            // The table as set (warps and morph unmodulated), back to front.
            let set_warps = [
                (warps[0].0, values[p(wt::OSC_WARP)]),
                (warps[1].0, values[p(wt::OSC_WARP2)]),
            ];
            let set_morph = (morph.0, values[p(wt::OSC_MORPH)], morph.2);
            let frames = drawn.source().frames();
            let count = frames.clamp(2, DEPTH_FRAMES);
            // The remap curve counts when a warp reads it.
            let remap_key = (wt::osc_param(osc, wt::OSC_REMAP_NODES)
                ..wt::osc_param(osc, wt::OSC_PARAMS))
                .filter(|_| warps.iter().any(|w| wt::is_remap(w.0)))
                .fold(0u64, |h, i| {
                    h.rotate_left(5) ^ u64::from(values[i].to_bits())
                });
            let key = vec![
                remap_key,
                drawn.identity() as u64,
                set_warps[0].0 as u64,
                (set_warps[0].1 * 1000.0) as u64,
                set_warps[1].0 as u64,
                (set_warps[1].1 * 1000.0) as u64,
                set_morph.0 as u64,
                (set_morph.1 * 1000.0) as u64,
                count as u64,
            ];
            let cache_id = panel.id(("depth", osc));
            // Behind an `Arc`: egui hands out copies of what it keeps.
            let mut cache: Arc<DepthCache> = ui.data(|d| d.get_temp(cache_id)).unwrap_or_default();
            if cache.key != key {
                let frames = (0..count)
                    .map(|i| {
                        let t = i as f32 / (count - 1) as f32;
                        let mut out = vec![0.0; FRAME_POINTS / 2];
                        drawn
                            .source()
                            .draw_remapped(t, set_morph, set_warps, &remap, &mut out);
                        out
                    })
                    .collect();
                cache = Arc::new(DepthCache { key, frames });
                ui.data_mut(|d| d.insert_temp(cache_id, Arc::clone(&cache)));
            }
            // Each frame further back sits up and to the right; the waves
            // overlap a little, so they keep most of the height.
            let depth = vec2(rect.width() * 0.13, -rect.height() * 0.3);
            let front = Rect::from_min_max(
                pos2(rect.left(), rect.top() - depth.y),
                pos2(rect.right() - depth.x, rect.bottom()),
            );
            let trace = |samples: &[f32], t: f32| -> Vec<Pos2> {
                let n = samples.len().max(2) - 1;
                samples
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        pos2(
                            front.left() + front.width() * i as f32 / n as f32,
                            front.center().y - s.clamp(-1.2, 1.2) * front.height() * 0.5,
                        ) + depth * t
                    })
                    .collect()
            };
            for (i, frame) in cache.frames.iter().enumerate().rev() {
                let t = i as f32 / (cache.frames.len() - 1) as f32;
                let near = 1.0 - (t - position_now).abs().min(1.0);
                let alpha = 0.10 + 0.25 * near * near;
                painter.add(Shape::line(
                    trace(frame, t),
                    Stroke::new(
                        stroke::HAIRLINE,
                        visuals.weak_text_color().gamma_multiply(alpha),
                    ),
                ));
            }
            // The frame playing, at its depth, lit and filled to its middle.
            let line = trace(&samples, position_now);
            let middle = front.center().y + depth.y * position_now;
            let mut fill = egui::Mesh::default();
            for pair in line.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let index = fill.vertices.len() as u32;
                let tint = color.gamma_multiply(0.22);
                fill.colored_vertex(a, tint);
                fill.colored_vertex(b, tint);
                fill.colored_vertex(pos2(b.x, middle), tint.gamma_multiply(0.2));
                fill.colored_vertex(pos2(a.x, middle), tint.gamma_multiply(0.2));
                fill.add_triangle(index, index + 1, index + 2);
                fill.add_triangle(index, index + 2, index + 3);
            }
            painter.add(fill);
            painter.add(Shape::line(line, Stroke::new(1.8, color)));
        }
        View::Wave => {
            let line: Vec<Pos2> = samples
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    pos2(
                        rect.left() + rect.width() * i as f32 / (FRAME_POINTS - 1) as f32,
                        rect.center().y - s.clamp(-1.2, 1.2) * rect.height() * 0.45,
                    )
                })
                .collect();
            painter.hline(
                rect.x_range(),
                rect.center().y,
                Stroke::new(
                    stroke::HAIRLINE,
                    visuals.weak_text_color().gamma_multiply(0.25),
                ),
            );
            painter.add(Shape::line(line, Stroke::new(1.8, color)));
        }
        View::Spectrum => {
            // The harmonics of the frame as drawn: warps included.
            let n = samples.len();
            let bar = rect.width() / HARMONICS as f32;
            for k in 1..=HARMONICS {
                let (mut re, mut im) = (0.0f32, 0.0f32);
                for (i, s) in samples.iter().enumerate() {
                    let angle = std::f32::consts::TAU * (k * i) as f32 / n as f32;
                    re += s * angle.cos();
                    im += s * angle.sin();
                }
                let level = 2.0 * (re * re + im * im).sqrt() / n as f32;
                let db = 20.0 * level.max(1e-6).log10();
                let height = ((db + 60.0) / 60.0).clamp(0.0, 1.0) * rect.height();
                let x = rect.left() + (k - 1) as f32 * bar;
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(x + 0.5, rect.bottom() - height),
                        pos2(x + bar - 0.5, rect.bottom()),
                    ),
                    0.0,
                    color.gamma_multiply(0.85),
                );
            }
        }
    }
    let text_color = if failed {
        visuals.error_fg_color
    } else {
        visuals.text_color()
    };
    painter.text(
        response.rect.right_top() + vec2(-DISPLAY_INSET.x, DISPLAY_INSET.y),
        Align2::RIGHT_TOP,
        &label,
        FontId::proportional(text::TINY),
        text_color,
    );
    // The view switch, bottom left.
    let mut x = response.rect.left() + space::S;
    for (choice, name) in [
        (View::Table, "3D"),
        (View::Wave, "2D"),
        (View::Spectrum, "SP"),
    ] {
        let galley = painter.layout_no_wrap(
            name.to_owned(),
            FontId::proportional(text::TINY),
            if choice == view {
                visuals.strong_text_color()
            } else {
                visuals.weak_text_color()
            },
        );
        let at = pos2(x, response.rect.bottom() - space::S - galley.size().y);
        let hit = Rect::from_min_size(at, galley.size()).expand(space::XS);
        let switch = ui.interact(hit, view_id.with(name), Sense::click());
        if switch.clicked() {
            view = choice;
        }
        painter.galley(at, galley, visuals.text_color());
        x += hit.width() + space::XS;
    }
    ui.data_mut(|d| d.insert_temp(view_id, view));
    let track = track_of(panel);
    if let Some(track) = track {
        register_drop_target(ui, response.rect, track, osc);
        // Files being dragged over it: say what a drop does.
        let over = |pos: Option<Pos2>| pos.is_some_and(|p| response.rect.contains(p));
        let files = ui.input(|i| !i.raw.hovered_files.is_empty()) && over(response.hover_pos());
        if files || over(panel.timeline.external_hover) {
            painter.rect_stroke(
                response.rect.shrink(1.0),
                radius::PANEL,
                Stroke::new(stroke::INDICATOR, accent),
                egui::StrokeKind::Inside,
            );
            painter.text(
                response.rect.center_bottom() - vec2(0.0, space::S),
                Align2::CENTER_BOTTOM,
                format!("Drop to load as Osc {}'s table", OSC_NAMES[osc]),
                FontId::proportional(text::SMALL),
                visuals.strong_text_color(),
            );
        }
    }
    let position = values[p(wt::OSC_POSITION)];
    if response.dragged() {
        let moved = position + response.drag_delta().x / rect.width();
        panel.set(p(wt::OSC_POSITION), moved);
    }
    if response.drag_stopped() {
        panel.finish();
    }
    let has_file = table_file(panel, osc).is_some();
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            format!("Osc {} wave: {label}", OSC_NAMES[osc]),
        )
    });
    let response = response.on_hover_text(
        "Drag sideways to move through the table. Drop a WAV file here (or right-click) \
         for the oscillator's own table",
    );
    response.context_menu(|ui| {
        if ui.button("Load Wavetable…").clicked() {
            ui.close();
            pick_table(panel, osc);
        }
        if ui.button("Edit Wavetable…").clicked() {
            ui.close();
            editor::open(ui, panel, osc);
        }
        if has_file
            && let Some(track) = track
            && ui.button("Remove Wavetable").clicked()
        {
            ui.close();
            panel.session.remove_wavetable(track, osc);
        }
    });
}

fn sub_and_noise(ui: &mut Ui, panel: &mut Panel, live: &Live) {
    ui.vertical(|ui| {
        knobs::switch(ui, panel, wt::SUB_ON, "Sub");
        choice_menu(ui, panel, wt::SUB_SHAPE, &[], 4);
        knob_row(
            ui,
            panel,
            live,
            &[(wt::SUB_OCTAVE, "Octave"), (wt::SUB_LEVEL, "Level")],
        );
        ui.add_space(space::XS);
        knobs::switch(ui, panel, wt::NOISE_ON, "Noise");
        choice_menu(ui, panel, wt::NOISE_TYPE, &[], 4);
        mod_knob(ui, panel, live, wt::NOISE_LEVEL, "Level");
    });
}

/// The color the oscillators draw in, for other displays to match: the
/// theme's accent.
pub(super) fn accent(ui: &Ui) -> Color32 {
    theme::accent(ui.visuals())
}
