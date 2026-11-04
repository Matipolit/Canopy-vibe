//! The Patcher canvas: a chain's graph as nodes and cables, hand-painted.
//!
//! - Nodes sit where the graph places them; a linear chain (no graph yet),
//!   or a graph whose nodes were never placed, is laid out left to right
//!   by how far the sound has come.
//! - Drag a node to move it (a view setting, not an undo step). A linear
//!   chain's nodes stay in their row until it has a graph.
//! - Drag from an output port (right) to an input port (left), or back,
//!   to connect them. A connection that can't be made (a loop, two cables
//!   between the same ports) shows in red and does nothing.
//! - Click a cable to select it; Delete or Backspace removes it.
//!   Right-click it to mute it, set its level or delete it.
//! - Right-click a node for what it is (a split's kind and crossovers, a
//!   device's bypass) and empty space to add splits, joins and effects.
//! - Drag empty space (with either button) to pan, scroll to pan, and
//!   Ctrl+scroll (or pinch) to zoom.
//!
//! The first change to a linear chain makes its graph and changes it in
//! one undo step (see `graph.rs`).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, Key as KeyCode, Modifiers, Painter, Pos2,
    Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, epaint::CubicBezierShape,
    text::LayoutJob, vec2,
};
use project::{Chain, NODE_SPACING};

use super::graph::{self, End, Graph, Key, ViewNode, WireKey};
use super::menus;
use crate::session::Session;
use crate::theme::{self, color, radius, size, space, stroke, surface, text};
use crate::timeline::TimelineView;
use crate::widgets::chrome::{self, Icon};

/// How wide a node is, in canvas units: room between columns for cables.
const NODE_WIDTH: f32 = NODE_SPACING * 0.7;
/// The zoom's range.
const MIN_ZOOM: f32 = 0.4;
const MAX_ZOOM: f32 = 2.0;
/// Sidechain ports and the cables into them: orange, apart from the
/// track's color the sound's cables take.
pub(crate) const SIDECHAIN: Color32 = theme::section::MACRO;

/// How the canvas of one chain is being looked at, and what's under way.
#[derive(Clone, Debug, Default)]
struct View {
    /// Where the canvas's origin is, from the top left of its rect.
    pan: Vec2,
    zoom: f32,
    /// Panned and zoomed to show every node yet.
    fitted: bool,
    selected: Option<WireKey>,
    /// What the context menu open over empty space is for.
    menu: Option<Menu>,
    drag: Option<Drag>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Menu {
    /// Empty space, at this place on the canvas.
    Canvas(Vec2),
    Wire(WireKey),
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Node(Key),
    /// A new cable, from the port the drag began on (an output, or an
    /// input when `backwards`).
    Wire {
        start: End,
        backwards: bool,
    },
}

/// The cable selected on the canvas shown last, for the device view's
/// Delete (see [`delete_selected_cable`]).
#[derive(Clone, Copy)]
struct Selected {
    chain: Chain,
    wire: WireKey,
    /// The frame the canvas was last drawn, so a canvas no longer shown
    /// doesn't keep the key.
    pass: u64,
}

fn view_id(chain: Chain) -> Id {
    Id::new(("patcher canvas", chain))
}

fn selected_id() -> Id {
    Id::new("patcher selected cable")
}

/// Removes the cable selected on the Patcher canvas, if the canvas is
/// showing and one is: for the device view's Delete and Backspace, which
/// run before the canvas is drawn. Returns whether it took the key.
#[allow(dead_code)] // until the device view's shortcuts call it
pub fn delete_selected_cable(ctx: &egui::Context, session: &mut Session) -> bool {
    let Some(selected) = ctx.data(|d| d.get_temp::<Selected>(selected_id())) else {
        return false;
    };
    if selected.pass + 1 < ctx.cumulative_pass_nr()
        || Graph::of(&session.project, selected.chain)
            .wire(selected.wire)
            .is_none()
    {
        return false;
    }
    let chain = selected.chain;
    session.edit("Remove Cable", |p| {
        graph::disconnect(p, chain, selected.wire)
    });
    ctx.data_mut(|d| {
        d.remove::<Selected>(selected_id());
        if let Some(mut view) = d.get_temp::<View>(view_id(chain)) {
            view.selected = None;
            d.insert_temp(view_id(chain), view);
        }
    });
    true
}

/// Where things were drawn last frame, for tests to aim at.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct Shown {
    pub nodes: Vec<(Key, egui::Rect)>,
    pub inputs: Vec<(End, Pos2)>,
    pub outputs: Vec<(End, Pos2)>,
    pub wires: Vec<(WireKey, Pos2)>,
}

#[cfg(test)]
fn shown_id(chain: Chain) -> Id {
    Id::new(("patcher shown", chain))
}

#[cfg(test)]
pub(crate) fn shown(ctx: &egui::Context, chain: Chain) -> Shown {
    ctx.data(|d| d.get_temp(shown_id(chain)))
        .unwrap_or_default()
}

/// Canvas units to the screen and back.
#[derive(Clone, Copy)]
struct Transform {
    origin: Pos2,
    zoom: f32,
}

impl Transform {
    fn screen(self, at: Vec2) -> Pos2 {
        self.origin + at * self.zoom
    }

    fn canvas(self, at: Pos2) -> Vec2 {
        (at - self.origin) / self.zoom
    }

    /// A theme size, at the zoom.
    fn len(self, length: f32) -> f32 {
        length * self.zoom
    }

    /// A font at the zoom, in half-point steps so zooming doesn't make
    /// a new font size every frame.
    fn font(self, size: f32, medium: bool) -> FontId {
        let size = ((size * self.zoom * 2.0).round() / 2.0).max(1.0);
        if medium {
            text::medium(size)
        } else {
            FontId::proportional(size)
        }
    }
}

/// How many rows of ports a node has.
fn rows(node: &ViewNode) -> u8 {
    node.inputs.max(node.outputs).max(1)
}

/// A node's rect, in canvas units.
fn node_rect(node: &ViewNode) -> Rect {
    let height = size::BAR + rows(node) as f32 * size::CONTROL + 2.0 * space::S;
    Rect::from_min_size(node.pos.to_pos2(), vec2(NODE_WIDTH, height))
}

/// Where a node's port is, in canvas units.
fn port_at(node: &ViewNode, input: bool, port: u8) -> Vec2 {
    let y = size::BAR + space::S + (port as f32 + 0.5) * size::CONTROL;
    node.pos + vec2(if input { 0.0 } else { NODE_WIDTH }, y)
}

/// A cable's curve on the screen, leaving `from` rightwards and arriving
/// at `to` from the left. One that skips past nodes in a row sags below
/// it, rather than running along the cables between them.
fn curve(from: Pos2, to: Pos2, zoom: f32) -> [Pos2; 4] {
    let reach = ((to.x - from.x).abs() * 0.5).max(NODE_SPACING * 0.3 * zoom);
    let span = to.x - from.x;
    let level = (to.y - from.y).abs() < graph::ROW_SPACING * zoom / 2.0;
    let sag = match (level, span < 0.0) {
        (false, _) => 0.0,
        (true, false) => (span - NODE_SPACING * zoom).max(0.0) * 0.4,
        // Back past where it started: round below.
        (true, true) => -span * 0.4 + graph::ROW_SPACING * zoom * 0.5,
    };
    [from, from + vec2(reach, sag), to + vec2(-reach, sag), to]
}

fn midpoint(points: [Pos2; 4]) -> Pos2 {
    let [a, b, c, d] = points.map(|p| p.to_vec2());
    ((a + 3.0 * b + 3.0 * c + d) / 8.0).to_pos2()
}

fn flatten(points: [Pos2; 4]) -> Vec<Pos2> {
    CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, Stroke::NONE)
        .flatten(Some(0.5))
}

fn distance_to(line: &[Pos2], at: Pos2) -> f32 {
    line.windows(2)
        .map(|pair| {
            let (a, b) = (pair[0], pair[1]);
            let along = b - a;
            let t = ((at - a).dot(along) / along.length_sq().max(f32::EPSILON)).clamp(0.0, 1.0);
            (a + along * t).distance(at)
        })
        .fold(f32::INFINITY, f32::min)
}

/// Draws `chain`'s graph and edits it, filling the space it's given.
pub fn canvas(
    ui: &mut Ui,
    session: &mut Session,
    _timeline: &mut TimelineView,
    chain: Chain,
    color: Color32,
) {
    let rect = ui.available_rect_before_wrap();
    let id = view_id(chain);
    let background = ui.allocate_rect(rect, Sense::click_and_drag());
    let graph = Graph::of(&session.project, chain);
    let mut view: View = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    if view.zoom <= 0.0 {
        view.zoom = 1.0;
    }
    if !view.fitted {
        fit(&mut view, &graph, rect);
    }
    if view.selected.is_some_and(|wire| graph.wire(wire).is_none()) {
        view.selected = None;
    }

    // Zoom about the pointer, and scroll to pan.
    if let Some(pointer) = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p))
        && ui.rect_contains_pointer(rect)
    {
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if zoom != 1.0 {
            let before = (pointer - rect.min - view.pan) / view.zoom;
            view.zoom = (view.zoom * zoom).clamp(MIN_ZOOM, MAX_ZOOM);
            view.pan = pointer - rect.min - before * view.zoom;
        } else if scroll != Vec2::ZERO {
            view.pan += scroll;
        }
    }
    if background.dragged() {
        view.pan += background.drag_delta();
    }
    let to = Transform {
        origin: rect.min + view.pan,
        zoom: view.zoom,
    };

    // Interaction first (nodes over the background, ports and switches
    // over nodes), painting after.
    let nodes: Vec<Response> = graph
        .nodes
        .iter()
        .map(|node| {
            let screen = Rect::from_min_max(
                to.screen(node_rect(node).min.to_vec2()),
                to.screen(node_rect(node).max.to_vec2()),
            );
            let response = ui.interact(
                screen.intersect(rect),
                id.with(("node", node.key)),
                Sense::click_and_drag(),
            );
            let title = node.title.clone();
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &title));
            response
        })
        .collect();
    let mut toggles = Vec::new();
    for node in &graph.nodes {
        let Some(bypassed) = node.bypassed else {
            toggles.push(None);
            continue;
        };
        let at = to.screen(node.pos + vec2(NODE_WIDTH - size::BAR / 2.0, size::BAR / 2.0));
        let hit = Rect::from_center_size(at, Vec2::splat(to.len(size::SMALL_CONTROL)));
        let response = ui.interact(hit, id.with(("on", node.key)), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, !bypassed, "On")
        });
        toggles.push(Some(response.on_hover_text("On/off")));
    }
    let hit = to.len(space::M).max(space::M * 0.75);
    let mut ports = Vec::new();
    for node in &graph.nodes {
        for (input, count) in [(true, node.inputs), (false, node.outputs)] {
            for port in 0..count {
                let at = to.screen(port_at(node, input, port));
                let response = ui.interact(
                    Rect::from_center_size(at, Vec2::splat(2.0 * hit)),
                    id.with(("port", node.key, input, port)),
                    Sense::drag(),
                );
                let response = match graph::port_name(&node.kind, input, port) {
                    Some((_, full)) => response.on_hover_text(full),
                    None => response,
                };
                ports.push((
                    End {
                        key: node.key,
                        port,
                    },
                    input,
                    at,
                    response,
                ));
            }
        }
    }

    // Cables on the screen, and the one under the pointer.
    let wires: Vec<(WireKey, [Pos2; 4], Vec<Pos2>)> = graph
        .wires
        .iter()
        .filter_map(|wire| {
            let from = graph.node(wire.key.from.key)?;
            let target = graph.node(wire.key.to.key)?;
            let points = curve(
                to.screen(port_at(from, false, wire.key.from.port)),
                to.screen(port_at(target, true, wire.key.to.port)),
                to.zoom,
            );
            Some((wire.key, points, flatten(points)))
        })
        .collect();
    let pointer = ui.ctx().pointer_latest_pos();
    let wire_under = |at: Pos2| {
        wires
            .iter()
            .map(|(key, _, line)| (*key, distance_to(line, at)))
            .filter(|(_, d)| *d <= hit)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(key, _)| key)
    };
    let hovered_wire = pointer
        .filter(|_| background.hovered() && view.drag.is_none())
        .and_then(wire_under);

    // Clicks on empty space: select a cable, or let go of one.
    if background.clicked() {
        view.selected = background.interact_pointer_pos().and_then(wire_under);
    }
    if background.secondary_clicked()
        && let Some(at) = background.interact_pointer_pos()
    {
        view.menu = Some(match wire_under(at) {
            Some(wire) => {
                view.selected = Some(wire);
                Menu::Wire(wire)
            }
            None => Menu::Canvas(to.canvas(at)),
        });
    }

    // Moving nodes.
    for (node, response) in graph.nodes.iter().zip(&nodes) {
        if response.drag_started() && !graph.linear {
            if graph.laid_out {
                pin(session, chain, &graph);
            }
            view.drag = Some(Drag::Node(node.key));
        }
        if response.dragged()
            && matches!(view.drag, Some(Drag::Node(key)) if key == node.key)
            && let Ok(id) = graph::node_id(&session.project, chain, node.key)
        {
            let pos = node.pos + response.drag_delta() / to.zoom;
            session.view_edit(|p| p.set_node_position(id, [pos.x, pos.y]));
        }
        if response.drag_stopped() {
            view.drag = None;
        }
        if response.clicked() {
            view.selected = None;
        }
    }

    // New cables.
    let mut pending = None;
    for (end, input, at, response) in &ports {
        if response.drag_started() {
            view.drag = Some(Drag::Wire {
                start: *end,
                backwards: *input,
            });
        }
        let Some(Drag::Wire { start, backwards }) = view.drag else {
            continue;
        };
        if start != *end || backwards != *input {
            continue;
        }
        let tip = pointer.unwrap_or(*at);
        // The nearest port of the other kind within reach.
        let target = ports
            .iter()
            .filter(|(_, other, ..)| *other != backwards)
            .map(|(end, _, at, _)| (*end, *at, at.distance(tip)))
            .filter(|(.., d)| *d <= hit * 1.5)
            .min_by(|a, b| a.2.total_cmp(&b.2));
        let check = target.map(|(other, there, _)| {
            let (from, to) = if backwards {
                (other, start)
            } else {
                (start, other)
            };
            (from, to, there, graph.check(from, to))
        });
        if response.drag_stopped() {
            view.drag = None;
            if let Some((from, to, _, Ok(()))) = check {
                session.edit("Connect", |p| graph::connect(p, chain, from, to));
            }
        } else {
            let end_at = check.as_ref().map_or(tip, |(.., there, _)| *there);
            pending = Some((*at, end_at, backwards, check.map(|c| c.3)));
        }
    }
    if view.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
        view.drag = None;
    }

    // Bypass switches.
    for (node, toggle) in graph.nodes.iter().zip(&toggles) {
        if let (Some(toggle), Key::Device(device), Some(bypassed)) =
            (toggle, node.key, node.bypassed)
            && toggle.clicked()
        {
            let label = if bypassed {
                "Turn On Effect"
            } else {
                "Bypass Effect"
            };
            session.edit(label, |p| p.set_device_bypassed(device, !bypassed));
        }
    }

    // The keyboard: Delete removes the selected cable, Escape lets go.
    if let Some(wire) = view.selected
        && ui.rect_contains_pointer(rect)
    {
        let delete = ui.input_mut(|i| {
            i.consume_key(Modifiers::NONE, KeyCode::Delete)
                | i.consume_key(Modifiers::NONE, KeyCode::Backspace)
        });
        if delete {
            session.edit("Remove Cable", |p| graph::disconnect(p, chain, wire));
            view.selected = None;
        } else if ui.input_mut(|i| i.consume_key(Modifiers::NONE, KeyCode::Escape)) {
            view.selected = None;
        }
    }

    // Painting.
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, surface::CANVAS);
    paint_grid(&painter, rect, to);
    for (key, points, line) in &wires {
        let Some(wire) = graph.wire(*key) else {
            continue;
        };
        let target = graph.node(key.to.key);
        let sidechain = target.is_some_and(|n| graph::is_sidechain(&n.kind, key.to.port));
        let base = if sidechain { SIDECHAIN } else { color };
        let selected = view.selected == Some(*key);
        let hovered = hovered_wire == Some(*key);
        let ink = if wire.muted {
            base.gamma_multiply(0.4)
        } else {
            base
        };
        let ink = if selected || hovered {
            ink.lerp_to_gamma(Color32::WHITE, if selected { 0.45 } else { 0.25 })
        } else {
            ink
        };
        let width = if selected {
            stroke::INDICATOR
        } else {
            stroke::EMPHASIS
        };
        let line_stroke = Stroke::new(to.len(width).max(stroke::HAIRLINE), ink);
        if wire.muted {
            painter.extend(Shape::dashed_line(
                line,
                line_stroke,
                to.len(space::M),
                to.len(space::S),
            ));
        } else {
            painter.add(CubicBezierShape::from_points_stroke(
                *points,
                false,
                Color32::TRANSPARENT,
                line_stroke,
            ));
        }
        if selected || hovered || wire.gain_db != 0.0 {
            let label = if wire.muted {
                "Muted".to_owned()
            } else {
                format!("{:+.1} dB", wire.gain_db)
            };
            let ink = if selected || hovered {
                surface::TEXT
            } else {
                surface::TEXT_WEAK
            };
            tag(
                &painter,
                midpoint(*points),
                &label,
                to.font(text::TINY, false),
                ink,
            );
        }
    }
    for ((node, response), toggle) in graph.nodes.iter().zip(&nodes).zip(&toggles) {
        paint_node(ui, &painter, node, response, toggle.as_ref(), to, color);
    }
    for (end, input, at, response) in &ports {
        let Some(node) = graph.node(end.key) else {
            continue;
        };
        let fill = if *input && graph::is_sidechain(&node.kind, end.port) {
            SIDECHAIN
        } else if *input {
            surface::TEXT_WEAK
        } else {
            color
        };
        let active = response.hovered() || response.dragged();
        let r = to.len(space::S).max(space::XS);
        painter.circle(
            *at,
            if active { r * 1.4 } else { r },
            fill,
            Stroke::new(stroke::HAIRLINE, surface::CANVAS),
        );
        if let Some((short, _)) = graph::port_name(&node.kind, *input, end.port) {
            let (offset, align) = if *input {
                (vec2(to.len(space::M), 0.0), Align2::LEFT_CENTER)
            } else {
                (vec2(-to.len(space::M), 0.0), Align2::RIGHT_CENTER)
            };
            painter.text(
                *at + offset,
                align,
                short,
                to.font(text::TINY, false),
                surface::TEXT_WEAK,
            );
        }
    }
    if let Some((from, end, backwards, check)) = pending {
        let refused = matches!(check, Some(Err(_)));
        let ink = if refused { color::RECORD } else { color };
        let (a, b) = if backwards { (end, from) } else { (from, end) };
        painter.add(CubicBezierShape::from_points_stroke(
            curve(a, b, to.zoom),
            false,
            Color32::TRANSPARENT,
            Stroke::new(to.len(stroke::EMPHASIS).max(stroke::HAIRLINE), ink),
        ));
        if let Some(Err(why)) = check {
            let at = end + vec2(0.0, -size::CONTROL);
            tag(
                &painter,
                at,
                why,
                FontId::proportional(text::SMALL),
                color::RECORD,
            );
        }
    }
    if graph.nodes.len() <= 2 && graph.linear {
        painter.text(
            rect.center_bottom() - vec2(0.0, space::L),
            Align2::CENTER_BOTTOM,
            "Right-click to add",
            FontId::proportional(text::SMALL),
            surface::TEXT_WEAK,
        );
    }

    // Menus.
    for (node, response) in graph.nodes.iter().zip(&nodes) {
        if menus::has_node_menu(node) {
            response.context_menu(|ui| menus::node(ui, session, chain, &graph, node));
        }
    }
    if let Some(menu) = view.menu {
        background.context_menu(|ui| match menu {
            Menu::Canvas(at) => menus::canvas(ui, session, chain, &graph, at),
            Menu::Wire(wire) => {
                if menus::wire(ui, session, chain, &graph, wire) {
                    view.selected = None;
                }
            }
        });
    }

    // Fit everything in view.
    let corner = Rect::from_min_size(
        rect.right_top() + vec2(-size::ICON - space::S, space::S),
        Vec2::splat(size::ICON),
    );
    if ui
        .put(corner, |ui: &mut Ui| {
            chrome::icon_button(ui, Icon::Fit, "Fit", "Show every node")
        })
        .clicked()
        || background.double_clicked()
    {
        fit(&mut view, &graph, rect);
    }

    let pass = ui.ctx().cumulative_pass_nr();
    ui.data_mut(|d| {
        match view.selected {
            Some(wire) => {
                d.insert_temp(selected_id(), Selected { chain, wire, pass });
            }
            None => {
                if d.get_temp::<Selected>(selected_id())
                    .is_some_and(|s| s.chain == chain)
                {
                    d.remove::<Selected>(selected_id());
                }
            }
        }
        d.insert_temp(id, view);
    });
    #[cfg(test)]
    {
        let shown = Shown {
            nodes: graph
                .nodes
                .iter()
                .zip(&nodes)
                .map(|(n, r)| (n.key, r.rect))
                .collect(),
            inputs: ports
                .iter()
                .filter(|p| p.1)
                .map(|(end, _, at, _)| (*end, *at))
                .collect(),
            outputs: ports
                .iter()
                .filter(|p| !p.1)
                .map(|(end, _, at, _)| (*end, *at))
                .collect(),
            wires: wires
                .iter()
                .map(|(key, points, _)| (*key, midpoint(*points)))
                .collect(),
        };
        ui.data_mut(|d| d.insert_temp(shown_id(chain), shown));
    }
}

/// Pans and zooms to show every node, no larger than life.
fn fit(view: &mut View, graph: &Graph, rect: Rect) {
    let Some(bounds) = graph.nodes.iter().map(node_rect).reduce(|a, b| a.union(b)) else {
        return;
    };
    let room = rect.shrink(space::XL);
    view.zoom = (room.width() / bounds.width())
        .min(room.height() / bounds.height())
        .clamp(MIN_ZOOM, 1.0);
    view.pan = rect.center() - rect.min - bounds.center().to_vec2() * view.zoom;
    view.fitted = true;
}

/// Stores the places the canvas shows its nodes at, before moving one in
/// a graph that was laid out by canopy: the others stay put.
pub(crate) fn pin(session: &mut Session, chain: Chain, graph: &Graph) {
    for node in &graph.nodes {
        if let Ok(id) = graph::node_id(&session.project, chain, node.key) {
            session.view_edit(|p| p.set_node_position(id, [node.pos.x, node.pos.y]));
        }
    }
}

/// Faint dots a node's spacing apart, so panning shows movement.
fn paint_grid(painter: &Painter, rect: Rect, to: Transform) {
    let step = NODE_SPACING / 4.0;
    let spacing = to.len(step);
    if spacing < space::M {
        return;
    }
    let first = to.canvas(rect.min);
    let start = vec2(
        (first.x / step).floor() * step,
        (first.y / step).floor() * step,
    );
    let ink = surface::BORDER;
    let mut y = start.y;
    while to.screen(vec2(0.0, y)).y <= rect.max.y {
        let mut x = start.x;
        while to.screen(vec2(x, 0.0)).x <= rect.max.x {
            painter.circle_filled(to.screen(vec2(x, y)), stroke::HAIRLINE, ink);
            x += step;
        }
        y += step;
    }
}

/// A small label on a dark ground, centred on `at`.
fn tag(painter: &Painter, at: Pos2, label: &str, font: FontId, ink: Color32) {
    let galley = painter.layout_no_wrap(label.to_owned(), font, ink);
    let back = Rect::from_center_size(at, galley.size() + Vec2::splat(2.0 * space::XS));
    painter.rect_filled(back, CornerRadius::same(radius::SMALL), surface::CANVAS);
    painter.galley(back.min + Vec2::splat(space::XS), galley, ink);
}

fn paint_node(
    ui: &Ui,
    painter: &Painter,
    node: &ViewNode,
    response: &Response,
    toggle: Option<&Response>,
    to: Transform,
    color: Color32,
) {
    let rect = node_rect(node);
    let rect = Rect::from_min_max(to.screen(rect.min.to_vec2()), to.screen(rect.max.to_vec2()));
    let corner = CornerRadius::same(radius::PANEL);
    let hover = theme::hover(ui, response);
    painter.rect_filled(rect, corner, surface::RAISED);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), to.len(size::BAR)));
    let tint = match node.kind {
        project::NodeKind::Input | project::NodeKind::Output => color.gamma_multiply(0.3),
        project::NodeKind::Split(_) | project::NodeKind::Join(_) => {
            theme::section::NEUTRAL.gamma_multiply(0.25)
        }
        project::NodeKind::Device(_) => {
            chrome::bar_fill(ui, response.id, response.hovered(), false)
        }
    };
    painter.rect_filled(
        bar,
        CornerRadius {
            nw: radius::PANEL,
            ne: radius::PANEL,
            sw: 0,
            se: 0,
        },
        tint,
    );
    let border = surface::BORDER.lerp_to_gamma(surface::TEXT_WEAK, hover);
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(stroke::HAIRLINE, border),
        StrokeKind::Inside,
    );

    let bypassed = node.bypassed == Some(true);
    let ink = if bypassed {
        surface::TEXT_WEAK
    } else {
        surface::TEXT
    };
    let room = rect.width()
        - to.len(space::M)
        - if node.bypassed.is_some() {
            to.len(size::BAR)
        } else {
            to.len(space::M)
        };
    let mut job = LayoutJob::simple_singleline(node.title.clone(), to.font(text::SMALL, true), ink);
    job.wrap.max_width = room.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = painter.layout_job(job);
    let at = bar.left_center() + vec2(to.len(space::M), -galley.size().y / 2.0);
    painter.galley(at, galley, ink);

    if let (Some(toggle), Some(bypassed)) = (toggle, node.bypassed) {
        let on = theme::fade(ui.ctx(), toggle.id.with("lit"), !bypassed);
        let h = theme::hover(ui, toggle);
        let off = surface::CONTROL.lerp_to_gamma(surface::CONTROL_HOVER, h);
        painter.circle(
            toggle.rect.center(),
            to.len(space::S).max(space::XS),
            off.lerp_to_gamma(color::ACTIVE, on),
            Stroke::new(stroke::HAIRLINE, surface::BORDER),
        );
    }
}
