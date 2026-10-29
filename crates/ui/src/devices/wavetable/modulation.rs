//! The envelopes and LFOs page: one envelope and one LFO at a time, picked
//! by their tabs (whose lights show where each is now), each drawn large
//! with a playhead where the newest voice is in it.
//!
//! An LFO's display depends on its shape: the Points shape is edited as
//! nodes and bendable segments (with a grid that can snap, and tools to
//! paint steps or ramps across it), the custom shape is drawn freehand as
//! steps, the chaos shapes show their attractor with a dot where the LFO
//! is on it, and the stock shapes show a cycle.

use eframe::egui::{self, Pos2, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};
use instruments::builtin::wavetable as wt;

use super::super::Panel;
use super::super::panel::{DISPLAY_INSET, display};
use super::knobs::{self, Live, choice_menu, knob_row, source_color};
use crate::theme::{self, space, stroke, text};
use crate::widgets::dahdsr::DahdsrEditor;
use crate::widgets::node_editor::{Node, NodeEditor, Tool};
use crate::widgets::shape::ShapeEditor;
use crate::widgets::{chrome, knob};

/// The envelope's and LFO's cards' widths inside: six and seven compact
/// knobs.
const ENV_WIDTH: f32 = 6.0 * knob::compact_width(knob::COMPACT);
const LFO_WIDTH: f32 = 7.0 * knob::compact_width(knob::COMPACT);
/// The column of switches and menus beside each editor.
const ENV_SIDE: f32 = 52.0;
const LFO_SIDE: f32 = 116.0;
/// How many points a chip's sketch of a shape has.
const SKETCH_POINTS: usize = 16;

fn env_id(panel: &Panel) -> egui::Id {
    panel.id("env")
}

fn lfo_id(panel: &Panel) -> egui::Id {
    panel.id("lfo")
}

/// Shows envelope `env` (0-based) when the page next opens.
pub(super) fn select_env(ui: &Ui, panel: &Panel, env: usize) {
    let id = env_id(panel);
    ui.data_mut(|d| d.insert_temp(id, env));
}

/// Shows LFO `lfo` (0-based) when the page next opens.
pub(super) fn select_lfo(ui: &Ui, panel: &Panel, lfo: usize) {
    let id = lfo_id(panel);
    ui.data_mut(|d| d.insert_temp(id, lfo));
}

pub(super) fn page(ui: &mut Ui, panel: &mut Panel, live: &Live, height: f32) {
    ui.spacing_mut().item_spacing.x = space::M;
    let compact = std::mem::replace(&mut panel.compact, true);
    let editor = super::display_height(height, 0, 1);
    envelopes(ui, panel, live, editor);
    lfos(ui, panel, live, editor);
    panel.compact = compact;
}

/// Heights at `count` evenly spaced points along a line through `points`
/// (x rising from 0 to its last x), for a chip's sketch.
fn resample(points: &[(f32, f32)], count: usize) -> Vec<f32> {
    let end = points.last().map_or(1.0, |p| p.0).max(1e-6);
    (0..count)
        .map(|i| {
            let x = i as f32 / (count - 1) as f32 * end;
            let next = points
                .iter()
                .position(|p| p.0 >= x)
                .unwrap_or(points.len() - 1);
            if next == 0 {
                return points[0].1;
            }
            let (a, b) = (points[next - 1], points[next]);
            let t = if b.0 > a.0 {
                (x - a.0) / (b.0 - a.0)
            } else {
                1.0
            };
            a.1 + (b.1 - a.1) * t
        })
        .collect()
}

/// Envelope `env`'s shape for its chip: each stage as long as its knob is
/// turned, the sustain held a while.
fn env_sketch(panel: &Panel, env: usize) -> Vec<f32> {
    let layout = wt::ENVELOPE_LAYOUTS[env];
    let at = |index: usize| panel.params[index].to_normalized(panel.values()[index]);
    let sustain = panel.values()[layout.sustain].clamp(0.0, 1.0);
    let mut x = at(wt::env_param(env, wt::ENV_DELAY));
    let mut points = vec![(0.0, 0.0), (x, 0.0)];
    for (length, level) in [
        (at(layout.attack), 1.0),
        (at(wt::env_param(env, wt::ENV_HOLD)), 1.0),
        (at(layout.decay), sustain),
        (0.5, sustain),
        (at(layout.release), 0.0),
    ] {
        x += length.max(0.02);
        points.push((x, level));
    }
    resample(&points, SKETCH_POINTS)
}

/// One cycle of LFO `lfo` for its chip.
fn lfo_sketch(panel: &Panel, lfo: usize) -> Vec<f32> {
    (0..SKETCH_POINTS)
        .map(|i| {
            let t = i as f32 / (SKETCH_POINTS - 1) as f32;
            0.5 + 0.5 * wt::lfo_shape_at(panel.values(), lfo, t)
        })
        .collect()
}

/// The envelopes' card: a chip for each (its shape sketched, lit as it
/// moves), the chosen one's editor with its switches beside it, and its
/// knobs.
fn envelopes(ui: &mut Ui, panel: &mut Panel, live: &Live, editor_height: f32) {
    let id = env_id(panel);
    let mut env: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
    let color = theme::section::ENVELOPE;
    chrome::section(ui, color, |ui| {
        ui.set_width(ENV_WIDTH);
        chrome::section_header(ui, "", color, |ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            for (n, layout) in wt::ENVELOPE_LAYOUTS.iter().enumerate() {
                let text = if n == 0 {
                    "Amp".to_owned()
                } else {
                    format!("{}", n + 1)
                };
                let sketch = env_sketch(panel, n);
                let source = wt::env_source(n);
                let level = panel.meters[wt::METER_ENV + n];
                let ink = source_color(source).gamma_multiply(0.6 + 0.4 * level.clamp(0.0, 1.0));
                if chrome::chip(ui, layout.name, &text, env == n, ink, &sketch).clicked() {
                    env = n;
                }
            }
        });
        let layout = wt::ENVELOPE_LAYOUTS[env];
        ui.horizontal_top(|ui| {
            let values = panel.values().to_vec();
            let value = |index: usize| values.get(index).copied().unwrap_or(0.0);
            let stage = panel.meters[wt::METER_ENV_STAGE + env];
            let level = panel.meters[wt::METER_ENV + env];
            let playhead = (stage >= 1.0).then_some((stage, level));
            let width = ENV_WIDTH - ENV_SIDE - ui.spacing().item_spacing.x;
            let editor = DahdsrEditor::new(
                panel.id(("envelope", env)),
                layout,
                wt::env_param(env, wt::ENV_DELAY),
                wt::env_param(env, wt::ENV_HOLD),
                panel.params,
                &value,
            )
            .color(source_color(wt::env_source(env)))
            .size(vec2(width, editor_height))
            .playhead(playhead);
            let response = editor.show(ui);
            for (index, new_value) in response.changes {
                panel.set(index, new_value);
            }
            if response.finished {
                panel.session.end_live("Change Envelope");
            }
            ui.vertical(|ui| {
                ui.set_width(ENV_SIDE);
                if env > 0 {
                    knobs::switch(ui, panel, wt::env_mono_param(env), "Mono").on_hover_text(
                        "One envelope for the whole instrument, started by every note and \
                         let go with the last key",
                    );
                }
                knobs::switch(ui, panel, wt::env_sync_param(env), "Sync")
                    .on_hover_text("Times as set at 120 BPM, following the song's tempo");
                knobs::switch(ui, panel, wt::env_retrigger_param(env), "Retrig")
                    .on_hover_text("Starts again on every note, even when legato slides over");
            });
        });
        knob_row(
            ui,
            panel,
            live,
            &[
                (wt::env_param(env, wt::ENV_DELAY), "Delay"),
                (layout.attack, "Attack"),
                (wt::env_param(env, wt::ENV_HOLD), "Hold"),
                (layout.decay, "Decay"),
                (layout.sustain, "Sustain"),
                (layout.release, "Release"),
            ],
        );
    });
    ui.data_mut(|d| d.insert_temp(id, env));
}

/// How the Points editor is set: its tool and grid, kept per LFO.
#[derive(Debug, Clone, Copy)]
struct EditorState {
    tool: Tool,
    grid: (usize, usize),
    snap: bool,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            tool: Tool::Edit,
            grid: (8, 4),
            snap: true,
        }
    }
}

/// The LFOs' card: a chip for each (its cycle sketched), the chosen one's
/// editor with its shape, mode, switches and tools beside it, and its
/// knobs.
fn lfos(ui: &mut Ui, panel: &mut Panel, live: &Live, editor_height: f32) {
    let id = lfo_id(panel);
    let mut lfo: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
    let color = theme::section::LFO;
    chrome::section(ui, color, |ui| {
        ui.set_width(LFO_WIDTH);
        chrome::section_header(ui, "", color, |ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            for i in 0..wt::LFO_COUNT {
                let mono = panel.values()[wt::lfo_param(i, wt::LFO_MONO)] >= 0.5;
                let text = format!("{}{}", i + 1, if mono { "M" } else { "" });
                let sketch = lfo_sketch(panel, i);
                let chip = chrome::chip(
                    ui,
                    &format!("LFO {}", i + 1),
                    &text,
                    lfo == i,
                    source_color(wt::lfo_source(i)),
                    &sketch,
                );
                let chip = if mono {
                    chip.on_hover_text("Mono: one for every voice")
                } else {
                    chip
                };
                if chip.clicked() {
                    lfo = i;
                }
            }
        });
        let p = |offset: usize| wt::lfo_param(lfo, offset);
        let shape = panel.values()[p(wt::LFO_SHAPE)] as usize;
        let mode = panel.values()[p(wt::LFO_MODE)] as usize;
        let state_id = panel.id(("lfo editor", lfo));
        let mut state: EditorState = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
        ui.horizontal_top(|ui| {
            let width = LFO_WIDTH - LFO_SIDE - ui.spacing().item_spacing.x;
            let size = vec2(width, editor_height);
            match shape {
                wt::SHAPE_NODES => node_editor(ui, panel, lfo, size, &state),
                wt::SHAPE_CUSTOM => step_editor(ui, panel, lfo, size),
                s if wt::is_chaos(s) => chaos_display(ui, panel, lfo, size),
                _ => lfo_display(ui, panel, lfo, size),
            }
            ui.vertical(|ui| {
                ui.set_width(LFO_SIDE);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::XS;
                    choice_menu(ui, panel, p(wt::LFO_SHAPE), &[], 6);
                    choice_menu(ui, panel, p(wt::LFO_MODE), &[], 5);
                });
                ui.horizontal(|ui| {
                    knobs::switch(ui, panel, p(wt::LFO_SYNC), "Sync")
                        .on_hover_text("Follows the song's tempo");
                    knobs::switch(ui, panel, p(wt::LFO_MONO), "Mono").on_hover_text(
                        "One LFO for the whole instrument rather than one per voice",
                    );
                });
                if shape == wt::SHAPE_NODES {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XS;
                        for (tool, name, hover) in [
                            (Tool::Edit, "Edit", "Move, bend, add and remove nodes"),
                            (Tool::Steps, "Steps", "Paint steps on the grid"),
                            (Tool::Ramps, "Ramps", "Paint ramps on the grid"),
                        ] {
                            if ui
                                .selectable_label(state.tool == tool, name)
                                .on_hover_text(hover)
                                .clicked()
                            {
                                state.tool = tool;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XS;
                        ui.weak("Grid");
                        ui.add(egui::DragValue::new(&mut state.grid.0).range(1..=16))
                            .on_hover_text("Divisions across");
                        ui.weak("×");
                        ui.add(egui::DragValue::new(&mut state.grid.1).range(1..=8))
                            .on_hover_text("Divisions up");
                        ui.toggle_value(&mut state.snap, "Snap")
                            .on_hover_text("Nodes land on the grid (Shift moves freely)");
                    });
                }
            });
        });
        ui.data_mut(|d| d.insert_temp(state_id, state));
        let synced = panel.values()[p(wt::LFO_SYNC)] >= 0.5;
        let rate = if synced {
            (p(wt::LFO_DIVISION), "Rate")
        } else {
            (p(wt::LFO_RATE), "Rate")
        };
        let mut knobs = vec![
            rate,
            (p(wt::LFO_PHASE), "Phase"),
            (p(wt::LFO_SMOOTH), "Smooth"),
            (p(wt::LFO_RISE), "Rise"),
            (p(wt::LFO_DELAY), "Delay"),
        ];
        if matches!(mode, wt::MODE_LOOP | wt::MODE_SUSTAIN) {
            knobs.push((
                p(wt::LFO_LOOP),
                if mode == wt::MODE_LOOP {
                    "Loop"
                } else {
                    "Hold At"
                },
            ));
        }
        if shape == wt::SHAPE_CUSTOM {
            knobs.push((p(wt::LFO_GLIDE), "Glide"));
        }
        knob_row(ui, panel, live, &knobs);
    });
    ui.data_mut(|d| d.insert_temp(id, lfo));
}

/// Where LFO `lfo` is in its cycle, `0..=1`: a one-shot that has finished
/// stays at the end.
fn lfo_phase(panel: &Panel, lfo: usize) -> f32 {
    let phase = panel.meters[wt::METER_LFO_PHASE + lfo];
    if phase >= 1.0 {
        1.0
    } else {
        phase.rem_euclid(1.0)
    }
}

/// The loop point to mark on an LFO's editor, if its mode has one.
fn loop_marker(panel: &Panel, lfo: usize) -> Option<f32> {
    let mode = panel.values()[wt::lfo_param(lfo, wt::LFO_MODE)] as usize;
    matches!(mode, wt::MODE_LOOP | wt::MODE_SUSTAIN)
        .then(|| panel.values()[wt::lfo_param(lfo, wt::LFO_LOOP)])
}

/// One cycle of a stock shape, with a playhead where it is now.
fn lfo_display(ui: &mut Ui, panel: &Panel, lfo: usize, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(space::S + space::XS, space::M));
    let visuals = ui.visuals().clone();
    let color = source_color(wt::lfo_source(lfo));
    let shape = panel.values()[wt::lfo_param(lfo, wt::LFO_SHAPE)] as usize;
    let to_y = |v: f32| rect.center().y - v.clamp(-1.0, 1.0) * rect.height() / 2.0;
    painter.hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(
            stroke::HAIRLINE,
            visuals.weak_text_color().gamma_multiply(0.2),
        ),
    );
    // Random shapes change each cycle: draw a few.
    let random = matches!(shape, wt::SHAPE_SAMPLE_HOLD | wt::SHAPE_SMOOTH_RANDOM);
    let cycles = if random { 4.0 } else { 1.0 };
    let points: Vec<Pos2> = (0..=200)
        .map(|i| {
            let t = i as f32 / 200.0;
            pos2(
                rect.left() + t * rect.width(),
                to_y(wt::lfo_shape_at(panel.values(), lfo, t * cycles)),
            )
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(1.8, color)));
    if let Some(point) = loop_marker(panel, lfo) {
        let x = rect.left() + point * rect.width();
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(stroke::HAIRLINE, visuals.warn_fg_color.gamma_multiply(0.6)),
        );
    }
    let phase = lfo_phase(panel, lfo);
    let value = panel.meters[wt::METER_LFO_VALUE + lfo];
    let x = rect.left() + phase / cycles * rect.width();
    painter.vline(
        x,
        rect.y_range(),
        Stroke::new(
            stroke::HAIRLINE,
            visuals.strong_text_color().gamma_multiply(0.6),
        ),
    );
    painter.circle_filled(pos2(x, to_y(value)), 3.5, visuals.strong_text_color());
}

/// A chaos shape's attractor, drawn from a fixed start, with a dot where
/// the LFO (the newest voice's, or the mono one) is on it now.
fn chaos_display(ui: &mut Ui, panel: &Panel, lfo: usize, size: Vec2) {
    let (response, painter) = display(ui, size, Sense::hover());
    let rect = response.rect.shrink2(vec2(space::M + space::XS, space::M));
    let visuals = ui.visuals().clone();
    let color = source_color(wt::lfo_source(lfo));
    let shape = panel.values()[wt::lfo_param(lfo, wt::LFO_SHAPE)] as usize;
    let cache_id = panel.id(("chaos trace", shape));
    let trace: Vec<(f32, f32)> = ui
        .data(|d| d.get_temp::<std::sync::Arc<Vec<(f32, f32)>>>(cache_id))
        .map(|t| t.to_vec())
        .unwrap_or_else(|| {
            let trace = wt::chaos_trace(shape, 24.0, 0.01);
            ui.data_mut(|d| d.insert_temp(cache_id, std::sync::Arc::new(trace.clone())));
            trace
        });
    // The output across, the second coordinate up.
    let to_screen = |(x, y): (f32, f32)| {
        pos2(
            rect.center().x + x * rect.width() / 2.0,
            rect.center().y - y * rect.height() / 2.0,
        )
    };
    let points: Vec<Pos2> = trace.iter().map(|p| to_screen(*p)).collect();
    painter.add(Shape::line(
        points,
        Stroke::new(stroke::HAIRLINE, color.gamma_multiply(0.45)),
    ));
    let value = panel.meters[wt::METER_LFO_VALUE + lfo];
    let second = 2.0 * panel.meters[wt::METER_LFO_PHASE + lfo] - 1.0;
    let at = to_screen((value, second));
    painter.circle_filled(at, 5.0, color.gamma_multiply(0.35));
    painter.circle_filled(at, 3.0, visuals.strong_text_color());
    painter.text(
        response.rect.right_top() + vec2(-DISPLAY_INSET.x, DISPLAY_INSET.y),
        egui::Align2::RIGHT_TOP,
        wt::LFO_SHAPES[shape],
        egui::FontId::proportional(text::TINY),
        visuals.weak_text_color(),
    );
}

/// The custom shape, drawn by dragging across its 32 steps, with the glide
/// between them as it plays and a playhead where the LFO is now.
fn step_editor(ui: &mut Ui, panel: &mut Panel, lfo: usize, size: Vec2) {
    let values = panel.values().to_vec();
    let point = |i: usize| wt::lfo_param(lfo, wt::LFO_POINT + i);
    let points: Vec<f32> = (0..wt::LFO_POINTS).map(|i| values[point(i)]).collect();
    let curve = |phase: f32| 0.5 + 0.5 * wt::lfo_shape_at(&values, lfo, phase);
    let response = ShapeEditor::new(panel.id(("lfo shape", lfo)), &points, &curve)
        .size(size)
        .color(source_color(wt::lfo_source(lfo)))
        .playhead(Some(lfo_phase(panel, lfo)))
        .divisions(4)
        .show(ui);
    for (i, value) in response.changes {
        panel.set(point(i), value);
    }
    if response.finished {
        panel.finish();
    }
}

/// LFO `lfo`'s nodes as the editor takes them.
fn nodes_of(panel: &Panel, lfo: usize) -> Vec<Node> {
    let shape = wt::lfo_nodes(panel.values(), lfo);
    (0..shape.count)
        .map(|i| {
            let curve = panel.values()[wt::lfo_param(lfo, wt::LFO_NODE_CURVE + i)];
            Node::new(shape.x[i], shape.y[i], curve)
        })
        .collect()
}

/// The Points shape, edited as nodes (see `widgets::node_editor`).
fn node_editor(ui: &mut Ui, panel: &mut Panel, lfo: usize, size: Vec2, state: &EditorState) {
    let values = panel.values().to_vec();
    let nodes = nodes_of(panel, lfo);
    let curve = |phase: f32| 0.5 + 0.5 * wt::lfo_shape_at(&values, lfo, phase);
    let response = NodeEditor::new(panel.id(("lfo nodes", lfo)), &nodes, &curve)
        .size(size)
        .color(source_color(wt::lfo_source(lfo)))
        .playhead(Some(lfo_phase(panel, lfo)))
        .loop_point(loop_marker(panel, lfo))
        .grid(state.grid.0, state.grid.1, state.snap)
        .tool(state.tool)
        .max_nodes(wt::MAX_NODES)
        .show(ui);
    if let Some(changed) = response.nodes {
        set_nodes(panel, lfo, &changed);
    }
    if response.finished {
        panel.finish();
    }
}

/// Stores `nodes` as LFO `lfo`'s Points shape.
fn set_nodes(panel: &mut Panel, lfo: usize, nodes: &[Node]) {
    let p = |offset: usize| wt::lfo_param(lfo, offset);
    let count = nodes.len().clamp(2, wt::MAX_NODES);
    panel.set(p(wt::LFO_NODES), (count - 2) as f32);
    for (i, node) in nodes.iter().take(count).enumerate() {
        panel.set(p(wt::LFO_NODE_X + i), node.x);
        panel.set(p(wt::LFO_NODE_Y + i), node.y);
        panel.set(p(wt::LFO_NODE_CURVE + i), node.curve);
    }
}
