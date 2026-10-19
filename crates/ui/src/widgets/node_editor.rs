//! A drawn modulation shape as Serum and Vital edit theirs: nodes through
//! one cycle, each with a value, joined by segments that bend.
//!
//! - Drag a node to move it; the first and last stay at the cycle's ends,
//!   and no node passes its neighbours. With the grid's snap on, nodes
//!   land on its lines (Shift moves freely).
//! - Drag the dot in the middle of a segment up or down to bend it.
//! - Double-click empty space to add a node there, double-click a node (or
//!   right-click it) to remove it.
//! - In Draw mode, drag across the editor to paint the shape cell by cell
//!   on the grid, as steps or as ramps.
//! - Right-click empty space for ready-made shapes and to flip the shape.
//!
//! The editor reports the whole new list of nodes when anything changes;
//! the caller stores them.

use eframe::egui::{self, Color32, Id, Pos2, Sense, Shape, Stroke, pos2, vec2};

const NODE_RADIUS: f32 = 4.0;
const PAD: f32 = 6.0;
/// Pixels of vertical drag per unit of curve.
const CURVE_DRAG: f32 = 60.0;

/// One node: its place in the cycle and value (`0..=1` each) and the bend
/// of the segment after it (`-1..=1`, as envelope stages bend).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node {
    pub x: f32,
    pub y: f32,
    pub curve: f32,
}

impl Node {
    pub const fn new(x: f32, y: f32, curve: f32) -> Node {
        Node { x, y, curve }
    }
}

/// How dragging across the editor edits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    /// Move nodes, bend segments, add and remove nodes.
    #[default]
    Edit,
    /// Paint a level per grid cell, held flat across it.
    Steps,
    /// Paint a level per grid line, ramping between them.
    Ramps,
}

/// What changed this frame, for the caller to apply.
#[derive(Debug, Default)]
pub struct NodeResponse {
    /// The whole shape, when it changed.
    pub nodes: Option<Vec<Node>>,
    /// A drag ended, or a one-click change (preset, add, remove) happened.
    pub finished: bool,
}

/// A shape preset: a name and its nodes.
pub type Preset = (&'static str, &'static [Node]);

/// Ready-made shapes, in the editor's menu.
pub const PRESETS: &[Preset] = &[
    (
        "Sine",
        &[
            Node::new(0.0, 0.5, -0.35),
            Node::new(0.25, 1.0, 0.35),
            Node::new(0.5, 0.5, -0.35),
            Node::new(0.75, 0.0, 0.35),
            Node::new(1.0, 0.5, 0.0),
        ],
    ),
    (
        "Triangle",
        &[
            Node::new(0.0, 0.0, 0.0),
            Node::new(0.5, 1.0, 0.0),
            Node::new(1.0, 0.0, 0.0),
        ],
    ),
    (
        "Saw up",
        &[Node::new(0.0, 0.0, 0.0), Node::new(1.0, 1.0, 0.0)],
    ),
    (
        "Saw down",
        &[Node::new(0.0, 1.0, 0.0), Node::new(1.0, 0.0, 0.0)],
    ),
    (
        "Square",
        &[
            Node::new(0.0, 1.0, 0.0),
            Node::new(0.5, 1.0, 0.0),
            Node::new(0.5, 0.0, 0.0),
            Node::new(1.0, 0.0, 0.0),
        ],
    ),
    (
        "Pulse 25%",
        &[
            Node::new(0.0, 1.0, 0.0),
            Node::new(0.25, 1.0, 0.0),
            Node::new(0.25, 0.0, 0.0),
            Node::new(1.0, 0.0, 0.0),
        ],
    ),
    (
        "Decay",
        &[Node::new(0.0, 1.0, -0.7), Node::new(1.0, 0.0, 0.0)],
    ),
    (
        "Swell",
        &[Node::new(0.0, 0.0, 0.6), Node::new(1.0, 1.0, 0.0)],
    ),
    (
        "Pump",
        &[
            Node::new(0.0, 0.0, -0.6),
            Node::new(0.35, 1.0, 0.0),
            Node::new(1.0, 1.0, 0.0),
        ],
    ),
    (
        "Stairs",
        &[
            Node::new(0.0, 0.0, 0.0),
            Node::new(0.25, 0.0, 0.0),
            Node::new(0.25, 0.33, 0.0),
            Node::new(0.5, 0.33, 0.0),
            Node::new(0.5, 0.67, 0.0),
            Node::new(0.75, 0.67, 0.0),
            Node::new(0.75, 1.0, 0.0),
            Node::new(1.0, 1.0, 0.0),
        ],
    ),
    (
        "Bounce",
        &[
            Node::new(0.0, 1.0, 0.6),
            Node::new(0.4, 0.0, -0.6),
            Node::new(0.65, 0.6, 0.6),
            Node::new(0.85, 0.0, -0.6),
            Node::new(0.95, 0.25, 0.6),
            Node::new(1.0, 0.0, 0.0),
        ],
    ),
    (
        "Flat",
        &[Node::new(0.0, 0.5, 0.0), Node::new(1.0, 0.5, 0.0)],
    ),
];

pub struct NodeEditor<'a> {
    id: Id,
    nodes: &'a [Node],
    /// The shape as it plays at a phase (0 to 1), smoothing included.
    curve: &'a dyn Fn(f32) -> f32,
    playhead: Option<f32>,
    /// Where a looping shape loops back to, drawn as a marker.
    loop_point: Option<f32>,
    size: egui::Vec2,
    color: Color32,
    /// Grid divisions across and up.
    grid: (usize, usize),
    snap: bool,
    tool: Tool,
    max_nodes: usize,
    /// Whether right-clicking offers ready-made shapes ([`PRESETS`]).
    context_menu: bool,
}

/// What a drag in progress started from.
#[derive(Debug, Clone, Copy)]
enum Grab {
    Node(usize),
    Curve { segment: usize, start: f32 },
}

impl<'a> NodeEditor<'a> {
    pub fn new(id: Id, nodes: &'a [Node], curve: &'a dyn Fn(f32) -> f32) -> Self {
        NodeEditor {
            id,
            nodes,
            curve,
            playhead: None,
            loop_point: None,
            size: vec2(300.0, 100.0),
            color: Color32::LIGHT_BLUE,
            grid: (4, 4),
            snap: false,
            tool: Tool::Edit,
            max_nodes: 16,
            context_menu: true,
        }
    }

    pub fn size(mut self, size: egui::Vec2) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    pub fn playhead(mut self, phase: Option<f32>) -> Self {
        self.playhead = phase;
        self
    }

    pub fn loop_point(mut self, point: Option<f32>) -> Self {
        self.loop_point = point;
        self
    }

    /// Grid lines across (time) and up (value), and whether nodes snap to them.
    pub fn grid(mut self, across: usize, up: usize, snap: bool) -> Self {
        self.grid = (across.max(1), up.max(1));
        self.snap = snap;
        self
    }

    pub fn tool(mut self, tool: Tool) -> Self {
        self.tool = tool;
        self
    }

    pub fn max_nodes(mut self, max: usize) -> Self {
        self.max_nodes = max.max(2);
        self
    }

    /// Whether right-clicking offers the shapes (on unless set). Inside a
    /// popup, where opening another would close it, offer them with
    /// [`shape_menu`] instead.
    pub fn context_menu(mut self, on: bool) -> Self {
        self.context_menu = on;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> NodeResponse {
        let mut out = NodeResponse::default();
        let (rect, response) = ui.allocate_exact_size(self.size, Sense::click_and_drag());
        #[cfg(test)]
        ui.ctx()
            .data_mut(|d| d.insert_temp(self.id.with("rect"), rect));
        let area = rect.shrink(PAD);
        let to_screen = |x: f32, y: f32| {
            pos2(
                area.left() + x * area.width(),
                area.bottom() - y * area.height(),
            )
        };
        let from_screen = |p: Pos2| {
            (
                ((p.x - area.left()) / area.width()).clamp(0.0, 1.0),
                ((area.bottom() - p.y) / area.height()).clamp(0.0, 1.0),
            )
        };
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        painter.rect_stroke(
            rect,
            4.0,
            visuals.widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        // The grid, with the middle (zero) line stronger.
        let faint = visuals.weak_text_color().gamma_multiply(0.15);
        for i in 1..self.grid.0 {
            let x = area.left() + area.width() * i as f32 / self.grid.0 as f32;
            painter.vline(x, area.y_range(), Stroke::new(1.0, faint));
        }
        for i in 1..self.grid.1 {
            let y = area.bottom() - area.height() * i as f32 / self.grid.1 as f32;
            painter.hline(area.x_range(), y, Stroke::new(1.0, faint));
        }
        painter.hline(
            area.x_range(),
            area.center().y,
            Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.3)),
        );
        if let Some(point) = self.loop_point {
            let x = area.left() + point.clamp(0.0, 1.0) * area.width();
            let dash = Stroke::new(1.0, visuals.warn_fg_color.gamma_multiply(0.7));
            painter.add(Shape::dashed_line(
                &[pos2(x, area.top()), pos2(x, area.bottom())],
                dash,
                3.0,
                3.0,
            ));
        }

        // The shape as it plays, filled to the middle.
        let steps = (area.width() / 2.0).max(16.0) as usize;
        let line: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let x = i as f32 / steps as f32;
                to_screen(x, (self.curve)(x.min(0.999_999)))
            })
            .collect();
        for pair in line.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let mid = area.center().y;
            painter.add(Shape::convex_polygon(
                vec![a, b, pos2(b.x, mid), pos2(a.x, mid)],
                self.color.gamma_multiply(0.12),
                Stroke::NONE,
            ));
        }
        painter.add(Shape::line(line, Stroke::new(1.8, self.color)));

        let nodes = self.nodes;
        let grab_id = self.id.with("grab");
        let snap = |v: f32, divisions: usize, free: bool| {
            if self.snap && !free {
                (v * divisions as f32).round() / divisions as f32
            } else {
                v
            }
        };
        let free = ui.input(|i| i.modifiers.shift);

        if self.tool == Tool::Edit {
            let mut grab: Option<Grab> = ui.data(|d| d.get_temp(grab_id));
            // Hit targets: nodes, then segment handles.
            let handle_of = |i: usize| -> Pos2 {
                let (a, b) = (nodes[i], nodes[i + 1]);
                let t = 0.5;
                let y = a.y + (b.y - a.y) * dsp::stage_shape(t, a.curve);
                to_screen((a.x + b.x) / 2.0, y)
            };
            let pointer = response.interact_pointer_pos().or(response.hover_pos());
            let hit = |p: Pos2| -> Option<Grab> {
                let node = (0..nodes.len())
                    .map(|i| (i, to_screen(nodes[i].x, nodes[i].y).distance(p)))
                    .filter(|(_, d)| *d < NODE_RADIUS + 4.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((i, _)) = node {
                    return Some(Grab::Node(i));
                }
                (0..nodes.len().saturating_sub(1))
                    .filter(|&i| nodes[i + 1].x > nodes[i].x)
                    .map(|i| (i, handle_of(i).distance(p)))
                    .filter(|(_, d)| *d < NODE_RADIUS + 3.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(segment, _)| Grab::Curve {
                        segment,
                        start: nodes[segment].curve,
                    })
            };
            let hovered = pointer.and_then(hit);
            if response.drag_started() {
                // Where the press was: a drag only starts once the pointer
                // has moved a little way from it.
                let origin = ui.input(|i| i.pointer.press_origin());
                grab = origin.or(response.interact_pointer_pos()).and_then(hit);
                ui.data_mut(|d| match grab {
                    Some(g) => {
                        d.insert_temp(grab_id, g);
                    }
                    None => d.remove::<Grab>(grab_id),
                });
            }
            if response.dragged()
                && let (Some(g), Some(p)) = (grab, response.interact_pointer_pos())
            {
                let mut changed = nodes.to_vec();
                match g {
                    Grab::Node(i) if i < changed.len() => {
                        let (x, y) = from_screen(p);
                        let last = changed.len() - 1;
                        let x = if i == 0 {
                            0.0
                        } else if i == last {
                            1.0
                        } else {
                            snap(x, self.grid.0, free).clamp(changed[i - 1].x, changed[i + 1].x)
                        };
                        changed[i].x = x;
                        changed[i].y = snap(y, self.grid.1, free);
                    }
                    Grab::Curve { segment, start } if segment + 1 < changed.len() => {
                        // Up bends toward the later node's side: a fast
                        // start when rising, as dragging the curve up.
                        let total = response.total_drag_delta().unwrap_or_default().y;
                        let rising = changed[segment + 1].y >= changed[segment].y;
                        let sign = if rising { 1.0 } else { -1.0 };
                        changed[segment].curve =
                            (start + sign * total / CURVE_DRAG).clamp(-1.0, 1.0);
                    }
                    _ => {}
                }
                if changed != nodes {
                    out.nodes = Some(changed);
                }
            }
            if response.drag_stopped() {
                ui.data_mut(|d| d.remove::<Grab>(grab_id));
                out.finished = true;
            }
            if response.double_clicked()
                && let Some(p) = response.interact_pointer_pos()
            {
                match hit(p) {
                    Some(Grab::Node(i)) if i > 0 && i + 1 < nodes.len() => {
                        let mut changed = nodes.to_vec();
                        changed.remove(i);
                        out.nodes = Some(changed);
                        out.finished = true;
                    }
                    Some(Grab::Curve { segment, .. }) => {
                        let mut changed = nodes.to_vec();
                        changed[segment].curve = 0.0;
                        out.nodes = Some(changed);
                        out.finished = true;
                    }
                    None if nodes.len() < self.max_nodes => {
                        let (x, y) = from_screen(p);
                        let (x, y) = (snap(x, self.grid.0, free), snap(y, self.grid.1, free));
                        let at = nodes
                            .iter()
                            .position(|n| n.x > x)
                            .unwrap_or(nodes.len() - 1)
                            .max(1);
                        let mut changed = nodes.to_vec();
                        changed.insert(at, Node::new(x, y, 0.0));
                        out.nodes = Some(changed);
                        out.finished = true;
                    }
                    _ => {}
                }
            }
            if hovered.is_some() {
                ui.ctx().set_cursor_icon(match hovered {
                    Some(Grab::Curve { .. }) => egui::CursorIcon::ResizeVertical,
                    _ => egui::CursorIcon::Move,
                });
            }
            // Handles, then nodes over them.
            for i in 0..nodes.len().saturating_sub(1) {
                if nodes[i + 1].x <= nodes[i].x {
                    continue;
                }
                let at = handle_of(i);
                let active = matches!(hovered, Some(Grab::Curve { segment, .. }) if segment == i)
                    || matches!(grab, Some(Grab::Curve { segment, .. }) if segment == i && response.dragged());
                let size = if active { 3.5 } else { 2.5 };
                painter.add(Shape::convex_polygon(
                    vec![
                        at + vec2(0.0, -size),
                        at + vec2(size, 0.0),
                        at + vec2(0.0, size),
                        at + vec2(-size, 0.0),
                    ],
                    visuals.extreme_bg_color,
                    Stroke::new(1.2, self.color.gamma_multiply(0.8)),
                ));
            }
            for (i, node) in nodes.iter().enumerate() {
                let at = to_screen(node.x, node.y);
                let active = matches!(hovered, Some(Grab::Node(n)) if n == i)
                    || matches!(grab, Some(Grab::Node(n)) if n == i && response.dragged());
                let radius = if active {
                    NODE_RADIUS + 1.0
                } else {
                    NODE_RADIUS
                };
                painter.circle(
                    at,
                    radius,
                    self.color,
                    Stroke::new(1.0, visuals.extreme_bg_color),
                );
            }
        } else {
            // Painting: one level per grid cell (or line), from the shape as it was.
            let cells = match self.tool {
                Tool::Steps => self.grid.0.min(self.max_nodes / 2),
                _ => self.grid.0.min(self.max_nodes - 1),
            }
            .max(1);
            let paint_id = self.id.with("paint");
            if response.drag_started() {
                let levels: Vec<f32> = (0..=cells)
                    .map(|i| {
                        let x = if self.tool == Tool::Steps {
                            (i as f32 + 0.5) / cells as f32
                        } else {
                            i as f32 / cells as f32
                        };
                        (self.curve)(x.min(0.999_999))
                    })
                    .collect();
                ui.data_mut(|d| d.insert_temp(paint_id, levels));
            }
            if (response.dragged() || response.clicked())
                && let Some(p) = response.interact_pointer_pos()
            {
                let mut levels: Vec<f32> = ui
                    .data(|d| d.get_temp(paint_id))
                    .unwrap_or_else(|| vec![0.5; cells + 1]);
                levels.resize(cells + 1, 0.5);
                let (x, y) = from_screen(p);
                let y = snap(y, self.grid.1, free);
                let cell = if self.tool == Tool::Steps {
                    ((x * cells as f32) as usize).min(cells - 1)
                } else {
                    (x * cells as f32).round() as usize
                };
                levels[cell] = y;
                let changed: Vec<Node> = if self.tool == Tool::Steps {
                    (0..cells)
                        .flat_map(|i| {
                            let (a, b) = (i as f32 / cells as f32, (i + 1) as f32 / cells as f32);
                            [Node::new(a, levels[i], 0.0), Node::new(b, levels[i], 0.0)]
                        })
                        .collect()
                } else {
                    (0..=cells)
                        .map(|i| Node::new(i as f32 / cells as f32, levels[i], 0.0))
                        .collect()
                };
                ui.data_mut(|d| d.insert_temp(paint_id, levels));
                out.nodes = Some(changed);
            }
            if response.drag_stopped() || response.clicked() {
                out.finished = true;
            }
            for node in nodes {
                painter.circle_filled(
                    to_screen(node.x, node.y),
                    2.0,
                    self.color.gamma_multiply(0.6),
                );
            }
        }

        if let Some(phase) = self.playhead {
            let x = phase.clamp(0.0, 1.0);
            let at = to_screen(x, (self.curve)(x.min(0.999_999)));
            painter.vline(
                at.x,
                area.y_range(),
                Stroke::new(1.0, visuals.strong_text_color().gamma_multiply(0.5)),
            );
            painter.circle_filled(at, 3.5, visuals.strong_text_color());
        }

        if self.context_menu {
            response.context_menu(|ui| {
                ui.label(egui::RichText::new("Shape").weak());
                if let Some(changed) = shape_menu(ui, nodes, PRESETS, self.max_nodes) {
                    out.nodes = Some(changed);
                    out.finished = true;
                }
            });
        }
        let response = response.on_hover_text(match (self.tool, self.context_menu) {
            (Tool::Edit, true) => {
                "Drag nodes to move them and the diamonds to bend; double-click to add or \
                 remove a node; right-click for shapes"
            }
            (Tool::Edit, false) => {
                "Drag nodes to move them and the diamonds to bend; double-click to add or \
                 remove a node"
            }
            (_, true) => "Drag across to paint the shape on the grid; right-click for shapes",
            (_, false) => "Drag across to paint the shape on the grid",
        });
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Shape editor")
        });
        out
    }
}

/// Buttons for `presets` (each kept to `max_nodes` nodes), then flipping
/// and reversing `nodes`: the shape chosen, if one was. Closes the menu
/// it is in when one is.
pub fn shape_menu(
    ui: &mut egui::Ui,
    nodes: &[Node],
    presets: &[Preset],
    max_nodes: usize,
) -> Option<Vec<Node>> {
    let mut chosen = None;
    for (name, preset) in presets {
        if ui.button(*name).clicked() {
            chosen = Some(preset.iter().take(max_nodes.max(2)).copied().collect());
        }
    }
    ui.separator();
    if ui.button("Flip Up and Down").clicked() {
        chosen = Some(
            nodes
                .iter()
                .map(|n| Node::new(n.x, 1.0 - n.y, n.curve))
                .collect(),
        );
    }
    if ui.button("Reverse").clicked() {
        let count = nodes.len();
        chosen = Some(
            (0..count)
                .map(|i| {
                    let node = nodes[count - 1 - i];
                    // The segment before it, reversed, bends the other way.
                    let curve = if i + 1 < count {
                        -nodes[count - 2 - i].curve
                    } else {
                        0.0
                    };
                    Node::new(1.0 - node.x, node.y, curve)
                })
                .collect(),
        );
    }
    if chosen.is_some() {
        ui.close();
    }
    chosen
}

/// The value `nodes` make at `x`, bends included, for checking presets.
#[cfg(test)]
fn rough_value(nodes: &[Node], x: f32) -> f32 {
    let i = nodes
        .windows(2)
        .position(|w| x >= w[0].x && x <= w[1].x && w[1].x > w[0].x)
        .unwrap_or(0);
    let (a, b) = (nodes[i], nodes[(i + 1).min(nodes.len() - 1)]);
    if b.x <= a.x {
        return b.y;
    }
    let t = ((x - a.x) / (b.x - a.x)).clamp(0.0, 1.0);
    a.y + (b.y - a.y) * dsp::stage_shape(t, a.curve)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_start_at_zero_end_at_one_and_stay_in_order() {
        for (name, nodes) in PRESETS {
            assert!(nodes.len() >= 2 && nodes.len() <= 16, "{name}");
            assert_eq!(nodes[0].x, 0.0, "{name}");
            assert_eq!(nodes[nodes.len() - 1].x, 1.0, "{name}");
            assert!(nodes.windows(2).all(|w| w[1].x >= w[0].x), "{name}");
            for node in *nodes {
                assert!((0.0..=1.0).contains(&node.y), "{name}");
                assert!((-1.0..=1.0).contains(&node.curve), "{name}");
            }
            assert!((0.0..=1.0).contains(&rough_value(nodes, 0.3)));
        }
    }
}
