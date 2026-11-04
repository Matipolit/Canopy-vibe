//! What the canvas draws: a chain's graph as nodes and wires, the same
//! whether the chain has a graph of its own or is a straight line (which
//! has no node ids, so the canvas shows the line it stands for), and the
//! edits the canvas makes, addressed so they work on either.

use std::collections::HashMap;

use eframe::egui::{Vec2, vec2};
use project::{
    CableKey, Chain, DeviceId, EditError, Join, NODE_SPACING, NodeId, NodeKind, PortRef, Project,
    Routing, Slope, Split,
};

/// A node, named by what it is rather than by its id: a linear chain's
/// nodes have no ids until its graph is made, and making it in the middle
/// of an edit mustn't lose track of which node was meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Input,
    Output,
    Device(DeviceId),
    /// A split or a join, which exist only in a graph.
    Node(NodeId),
}

impl Key {
    fn of(kind: &NodeKind, id: NodeId) -> Key {
        match kind {
            NodeKind::Input => Key::Input,
            NodeKind::Output => Key::Output,
            NodeKind::Device(device) => Key::Device(*device),
            NodeKind::Split(_) | NodeKind::Join(_) => Key::Node(id),
        }
    }

    /// The node it names in `routing`.
    pub(crate) fn resolve(self, routing: &Routing) -> Option<NodeId> {
        match self {
            Key::Input => routing.input(),
            Key::Output => routing.output(),
            Key::Device(device) => routing.device_node(device),
            Key::Node(id) => routing.node(id).map(|n| n.id),
        }
    }
}

/// A port: a node and the port's number, inputs and outputs counted apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct End {
    pub key: Key,
    pub port: u8,
}

/// Which wire: at most one joins two ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WireKey {
    pub from: End,
    pub to: End,
}

#[derive(Clone)]
pub(crate) struct ViewNode {
    pub key: Key,
    pub kind: NodeKind,
    /// Its top left corner, in canvas units.
    pub pos: Vec2,
    pub inputs: u8,
    pub outputs: u8,
    pub title: String,
    /// A device's bypass; `None` for the other nodes.
    pub bypassed: Option<bool>,
}

#[derive(Clone)]
pub(crate) struct Wire {
    pub key: WireKey,
    pub gain_db: f32,
    pub muted: bool,
}

pub(crate) struct Graph {
    pub nodes: Vec<ViewNode>,
    pub wires: Vec<Wire>,
    /// The chain has no graph of its own: it's drawn as the straight line
    /// it stands for, and its nodes can't be moved.
    pub linear: bool,
    /// The places come from canopy's layout rather than the graph (a linear
    /// chain, or a graph whose nodes were never placed): moving one node
    /// keeps the others where they're shown.
    pub laid_out: bool,
}

/// The canvas's layout: how far apart rows of nodes are.
pub(crate) const ROW_SPACING: f32 = NODE_SPACING * 0.7;

impl Graph {
    /// `chain`'s graph, or the line through its devices.
    pub(crate) fn of(project: &Project, chain: Chain) -> Graph {
        let devices = project.devices(chain).unwrap_or_default();
        let device_node = |id: DeviceId| {
            let device = devices.iter().find(|d| d.id == id);
            (
                device.map_or_else(|| "Missing".to_owned(), |d| d.name().to_owned()),
                Some(device.is_some_and(|d| d.bypassed)),
            )
        };
        let mut graph = match project.routing(chain) {
            Some(routing) => {
                let nodes = routing
                    .nodes
                    .iter()
                    .map(|n| {
                        let (title, bypassed) = match &n.kind {
                            NodeKind::Device(id) => device_node(*id),
                            kind => (title(kind), None),
                        };
                        ViewNode {
                            key: Key::of(&n.kind, n.id),
                            kind: n.kind.clone(),
                            pos: vec2(n.pos[0], n.pos[1]),
                            inputs: n.kind.inputs(devices),
                            outputs: n.kind.outputs(),
                            title,
                            bypassed,
                        }
                    })
                    .collect::<Vec<_>>();
                let key_of: HashMap<NodeId, Key> = routing
                    .nodes
                    .iter()
                    .map(|n| (n.id, Key::of(&n.kind, n.id)))
                    .collect();
                let wires = routing
                    .cables
                    .iter()
                    .filter_map(|c| {
                        let from = End {
                            key: *key_of.get(&c.from.node)?,
                            port: c.from.port,
                        };
                        let to = End {
                            key: *key_of.get(&c.to.node)?,
                            port: c.to.port,
                        };
                        Some(Wire {
                            key: WireKey { from, to },
                            gain_db: c.gain_db,
                            muted: c.muted,
                        })
                    })
                    .collect();
                let laid_out = nodes.iter().all(|n| n.pos == Vec2::ZERO);
                Graph {
                    nodes,
                    wires,
                    linear: false,
                    laid_out,
                }
            }
            None => {
                let kinds = std::iter::once(NodeKind::Input)
                    .chain(devices.iter().map(|d| NodeKind::Device(d.id)))
                    .chain([NodeKind::Output]);
                let nodes: Vec<ViewNode> = kinds
                    .map(|kind| {
                        let (title, bypassed) = match &kind {
                            NodeKind::Device(id) => device_node(*id),
                            kind => (title(kind), None),
                        };
                        ViewNode {
                            key: Key::of(&kind, NodeId(0)),
                            inputs: kind.inputs(devices),
                            outputs: kind.outputs(),
                            kind,
                            pos: Vec2::ZERO,
                            title,
                            bypassed,
                        }
                    })
                    .collect();
                let wires = nodes
                    .windows(2)
                    .map(|pair| Wire {
                        key: WireKey {
                            from: End {
                                key: pair[0].key,
                                port: 0,
                            },
                            to: End {
                                key: pair[1].key,
                                port: 0,
                            },
                        },
                        gain_db: 0.0,
                        muted: false,
                    })
                    .collect();
                Graph {
                    nodes,
                    wires,
                    linear: true,
                    laid_out: true,
                }
            }
        };
        if graph.laid_out {
            graph.lay_out();
        }
        graph
    }

    /// The graph with its nodes laid out by canopy, wherever they are now.
    pub(crate) fn laid_out_anew(&self) -> Graph {
        let mut graph = Graph {
            nodes: self.nodes.clone(),
            wires: self.wires.clone(),
            linear: self.linear,
            laid_out: true,
        };
        graph.lay_out();
        graph
    }

    pub(crate) fn node(&self, key: Key) -> Option<&ViewNode> {
        self.nodes.iter().find(|n| n.key == key)
    }

    pub(crate) fn wire(&self, key: WireKey) -> Option<&Wire> {
        self.wires.iter().find(|w| w.key == key)
    }

    /// Places the nodes left to right by how many steps the sound takes to
    /// reach them from Input, Output last, each column centred on the row
    /// Input is on.
    fn lay_out(&mut self) {
        let index: HashMap<Key, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.key, i))
            .collect();
        let mut depth = vec![0usize; self.nodes.len()];
        // Longest paths by relaxation: a pass per node is enough in a graph
        // without loops, and a damaged one with loops still stops.
        for _ in 0..self.nodes.len() {
            let mut moved = false;
            for wire in &self.wires {
                let (Some(&from), Some(&to)) =
                    (index.get(&wire.key.from.key), index.get(&wire.key.to.key))
                else {
                    continue;
                };
                if depth[to] < depth[from] + 1 {
                    depth[to] = depth[from] + 1;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        if let Some(&output) = index.get(&Key::Output) {
            let last = (0..self.nodes.len())
                .filter(|i| *i != output)
                .map(|i| depth[i])
                .max()
                .unwrap_or(0);
            depth[output] = depth[output].max(last + 1);
        }
        let columns = depth.iter().copied().max().unwrap_or(0) + 1;
        let mut counts = vec![0usize; columns];
        for d in &depth {
            counts[*d] += 1;
        }
        let mut placed = vec![0usize; columns];
        for (node, d) in self.nodes.iter_mut().zip(&depth) {
            let row = placed[*d] as f32 - (counts[*d] - 1) as f32 / 2.0;
            placed[*d] += 1;
            node.pos = vec2(*d as f32 * NODE_SPACING, row * ROW_SPACING);
        }
    }

    /// Whether a wire may join `from` (an output) to `to` (an input): both
    /// ports there, no wire between them yet, and no loop made.
    pub(crate) fn check(&self, from: End, to: End) -> Result<(), &'static str> {
        let (Some(source), Some(target)) = (self.node(from.key), self.node(to.key)) else {
            return Err("No such node");
        };
        if from.port >= source.outputs || to.port >= target.inputs {
            return Err("No such port");
        }
        if self.wire(WireKey { from, to }).is_some() {
            return Err("Already connected");
        }
        if self.reaches(to.key, from.key) {
            return Err("Would loop");
        }
        Ok(())
    }

    /// Whether sound from `from` gets to `to` (a node reaches itself).
    fn reaches(&self, from: Key, to: Key) -> bool {
        let mut seen = vec![from];
        let mut next = vec![from];
        while let Some(at) = next.pop() {
            if at == to {
                return true;
            }
            for wire in self.wires.iter().filter(|w| w.key.from.key == at) {
                if !seen.contains(&wire.key.to.key) {
                    seen.push(wire.key.to.key);
                    next.push(wire.key.to.key);
                }
            }
        }
        false
    }
}

/// What a split or a join is called on its node. Devices go by their
/// effect's name, and Input and Output by theirs.
pub(crate) fn title(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Input => "Input".to_owned(),
        NodeKind::Output => "Output".to_owned(),
        NodeKind::Device(_) => "Effect".to_owned(),
        NodeKind::Split(Split::Bands { crossovers, .. }) => {
            format!("Split {} bands", crossovers.len() + 1)
        }
        NodeKind::Split(Split::LeftRight) => "Split L/R".to_owned(),
        NodeKind::Split(Split::MidSide) => "Split M/S".to_owned(),
        NodeKind::Join(Join::LeftRight) => "Join L/R".to_owned(),
        NodeKind::Join(Join::MidSide) => "Join M/S".to_owned(),
    }
}

/// A port's short name beside it, and what it is in full for its tooltip;
/// `None` for a node's only port, which needs no name.
pub(crate) fn port_name(
    kind: &NodeKind,
    input: bool,
    port: u8,
) -> Option<(&'static str, &'static str)> {
    match (kind, input) {
        (NodeKind::Device(_), true) if port == 1 => Some(("SC", "Sidechain")),
        (NodeKind::Split(Split::Bands { crossovers, .. }), false) => {
            let names: &[(&str, &str)] = match crossovers.len() {
                1 => &[("Lo", "Low band"), ("Hi", "High band")],
                2 => &[
                    ("Lo", "Low band"),
                    ("Mid", "Middle band"),
                    ("Hi", "High band"),
                ],
                _ => &[
                    ("Lo", "Low band"),
                    ("LM", "Low middle band"),
                    ("HM", "High middle band"),
                    ("Hi", "High band"),
                ],
            };
            names.get(port as usize).copied()
        }
        (NodeKind::Split(Split::LeftRight), false) | (NodeKind::Join(Join::LeftRight), true) => {
            [("L", "Left"), ("R", "Right")].get(port as usize).copied()
        }
        (NodeKind::Split(Split::MidSide), false) | (NodeKind::Join(Join::MidSide), true) => {
            [("M", "Mid"), ("S", "Side")].get(port as usize).copied()
        }
        _ => None,
    }
}

/// Whether a device node's input `port` is its sidechain.
pub(crate) fn is_sidechain(kind: &NodeKind, port: u8) -> bool {
    matches!(kind, NodeKind::Device(_)) && port == 1
}

/// A band split of `bands` bands, at crossovers spread over the spectrum.
pub(crate) fn bands(bands: usize, slope: Slope) -> Split {
    let crossovers = match bands {
        2 => vec![1000.0],
        3 => vec![200.0, 2000.0],
        _ => vec![120.0, 1000.0, 5000.0],
    };
    Split::Bands { crossovers, slope }
}

// The edits. Each runs inside one `Session::edit`, making the chain's graph
// first if it's a straight line, so the first change to a linear chain is
// one undo step.

fn missing() -> EditError {
    EditError::Invalid("that node isn't in the graph any more".into())
}

fn port(project: &Project, chain: Chain, end: End) -> Result<PortRef, EditError> {
    let routing = project.routing(chain).ok_or_else(missing)?;
    let node = end.key.resolve(routing).ok_or_else(missing)?;
    Ok(PortRef {
        node,
        port: end.port,
    })
}

fn cable(project: &Project, chain: Chain, wire: WireKey) -> Result<CableKey, EditError> {
    Ok(CableKey {
        from: port(project, chain, wire.from)?,
        to: port(project, chain, wire.to)?,
    })
}

/// Runs `change` on `chain`'s graph, made from its straight line first if
/// it has none, and brings it back to canonical form after.
fn in_graph<T>(
    project: &mut Project,
    chain: Chain,
    change: impl FnOnce(&mut Project) -> Result<T, EditError>,
) -> Result<T, EditError> {
    project.ensure_routing(chain)?;
    let value = change(project)?;
    project.normalize_routing(chain)?;
    Ok(value)
}

pub(crate) fn connect(
    project: &mut Project,
    chain: Chain,
    from: End,
    to: End,
) -> Result<(), EditError> {
    in_graph(project, chain, |p| {
        let (from, to) = (port(p, chain, from)?, port(p, chain, to)?);
        p.connect(chain, from, to).map(|_| ())
    })
}

pub(crate) fn disconnect(
    project: &mut Project,
    chain: Chain,
    wire: WireKey,
) -> Result<(), EditError> {
    in_graph(project, chain, |p| {
        let key = cable(p, chain, wire)?;
        p.disconnect(chain, key)
    })
}

pub(crate) fn set_wire(
    project: &mut Project,
    chain: Chain,
    wire: WireKey,
    gain_db: f32,
    muted: bool,
) -> Result<(), EditError> {
    in_graph(project, chain, |p| {
        let key = cable(p, chain, wire)?;
        p.set_cable(chain, key, gain_db, muted)
    })
}

/// The node's id, for edits only a graph's own nodes take.
pub(crate) fn node_id(project: &Project, chain: Chain, key: Key) -> Result<NodeId, EditError> {
    key.resolve(project.routing(chain).ok_or_else(missing)?)
        .ok_or_else(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use project::EffectRef;

    fn patched() -> (Project, Chain, [DeviceId; 2]) {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let patcher = p
            .add_device(Some(track), EffectRef::builtin(project::PATCHER), None)
            .unwrap();
        let chain = Chain::Patch(patcher);
        let ids = ["canopy.eq", "canopy.reverb"]
            .map(|id| p.add_device(chain, EffectRef::builtin(id), None).unwrap());
        (p, chain, ids)
    }

    #[test]
    fn a_linear_chain_is_drawn_as_its_line_left_to_right() {
        let (p, chain, [eq, reverb]) = patched();
        let graph = Graph::of(&p, chain);
        assert!(graph.linear);
        let keys: Vec<Key> = graph.nodes.iter().map(|n| n.key).collect();
        assert_eq!(
            keys,
            [
                Key::Input,
                Key::Device(eq),
                Key::Device(reverb),
                Key::Output
            ]
        );
        let xs: Vec<f32> = graph.nodes.iter().map(|n| n.pos.x).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
        assert_eq!(graph.wires.len(), 3);
    }

    #[test]
    fn a_wire_back_upstream_would_loop() {
        let (p, chain, [eq, reverb]) = patched();
        let graph = Graph::of(&p, chain);
        let from = End {
            key: Key::Device(reverb),
            port: 0,
        };
        let to = End {
            key: Key::Device(eq),
            port: 0,
        };
        assert_eq!(graph.check(from, to), Err("Would loop"));
        let skip = End {
            key: Key::Input,
            port: 0,
        };
        assert_eq!(graph.check(skip, to), Err("Already connected"));
        let output = End {
            key: Key::Output,
            port: 0,
        };
        assert_eq!(graph.check(skip, output), Ok(()));
    }

    #[test]
    fn the_first_connection_in_a_linear_chain_makes_its_graph() {
        let (mut p, chain, _) = patched();
        let from = End {
            key: Key::Input,
            port: 0,
        };
        let to = End {
            key: Key::Output,
            port: 0,
        };
        connect(&mut p, chain, from, to).unwrap();
        let graph = Graph::of(&p, chain);
        assert!(!graph.linear);
        assert_eq!(graph.wires.len(), 4);
    }
}
