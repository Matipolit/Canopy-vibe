//! Effect graphs: how a chain's sound flows when it isn't a straight line.
//!
//! A track's (or the master's) effects stay one flat pool,
//! `Track::devices`, whatever the routing; every effect keeps its
//! `DeviceId`, its automation lanes and its controller links, and the
//! engine keeps its instance, as before. `Track::routing` is an optional
//! overlay on that pool:
//!
//! - **`None`** means the chain is linear: the sound goes through the
//!   devices in pool order. Nearly every chain is, and every file from
//!   before graphs existed.
//! - **`Some(Routing)`** is a node graph over the pool: an Input node (the
//!   track's sound), an Output node (to the fader), a node per device, and
//!   splits and joins. Cables carry stereo audio from an output port to an
//!   input port; an input port sums every cable into it, so parallel
//!   branches need no "sum" node, and a branch's dry/wet is two cables with
//!   gains.
//!
//! Edits keep a graph in canonical form ([`Project::normalize_routing`]
//! after each): one that has become a straight line at unity gain goes
//! back to `None`, and otherwise the pool is kept in topological order, so
//! the rack can list it in the order sound reaches each device. Graphs are
//! acyclic (feedback would need a delay node; not yet).
//!
//! Ports: Input has one output; Output one input; a device one input, plus
//! a second for its sidechain if it listens to one (or if it's an effect
//! this build doesn't know, so its cables survive), and one output; a split
//! one input and an output per band (or two, for left/right and mid/side,
//! each carried as dual mono: (L, L) and (R, R)); a join two inputs, read
//! from their first channels, and one output.

use std::collections::{BTreeSet, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::edit::EditResult;
use crate::{Device, DeviceId, EditError, NodeId, Project, TrackId};

/// The most crossover points a band split has (four bands).
pub const MAX_CROSSOVERS: usize = 3;

/// How far apart nodes are laid out when canopy places them, in canvas
/// units.
pub const NODE_SPACING: f32 = 180.0;

/// A node graph over a chain's devices.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Routing {
    pub nodes: Vec<GraphNode>,
    pub cables: Vec<Cable>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Where it sits on the Patcher's canvas. A view setting, like a
    /// device's fold: not an undo step.
    #[serde(default)]
    pub pos: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// The track's sound: its instrument, clips, or group bus.
    Input,
    /// To the fader.
    Output,
    /// One of the pool's devices.
    Device(DeviceId),
    Split(Split),
    Join(Join),
}

/// Divides the sound into parts that can be processed apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Split {
    /// Frequency bands, split at each crossover (Hz, ascending, 1 to
    /// [`MAX_CROSSOVERS`] of them) with Linkwitz-Riley filters, which add
    /// no latency and sum back to the whole.
    Bands {
        crossovers: Vec<f32>,
        #[serde(default)]
        slope: Slope,
    },
    LeftRight,
    MidSide,
}

/// How steep a band split's crossovers are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Slope {
    Db12,
    #[default]
    Db24,
    Db48,
}

/// Puts the parts of a left/right or mid/side split back together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Join {
    LeftRight,
    MidSide,
}

/// One of a node's ports, inputs and outputs counted separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortRef {
    pub node: NodeId,
    pub port: u8,
}

/// Sound from an output port to an input port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cable {
    pub from: PortRef,
    pub to: PortRef,
    /// How much of the sound it sends, like an FL Patcher cable's level.
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub muted: bool,
}

/// Which cable: there's at most one between two ports. Stable while both
/// ends are, so the engine keys a cable's state (its gain ramp, its delay)
/// by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CableKey {
    pub from: PortRef,
    pub to: PortRef,
}

impl Cable {
    pub fn key(&self) -> CableKey {
        CableKey {
            from: self.from,
            to: self.to,
        }
    }

    /// At unity gain and unmuted: what a linear chain's links are.
    pub fn is_plain(&self) -> bool {
        self.gain_db == 0.0 && !self.muted
    }
}

/// Why a graph can't be played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingError {
    /// Not exactly one Input node.
    InputCount(usize),
    /// Not exactly one Output node.
    OutputCount(usize),
    DuplicateNode(NodeId),
    /// A device node for a device that isn't in the pool.
    UnknownDevice(DeviceId),
    /// A device with two nodes.
    DuplicateDeviceNode(DeviceId),
    /// A device in the pool with no node.
    DeviceWithoutNode(DeviceId),
    /// A band split without 1 to [`MAX_CROSSOVERS`] finite crossovers.
    BadSplit(NodeId),
    /// A cable from an output port, or to an input port, that isn't there.
    NoSuchPort(PortRef),
    DuplicateCable(CableKey),
    /// Sound would go round in a loop.
    Cycle,
}

impl fmt::Display for RoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputCount(n) => write!(f, "a graph needs one input node, not {n}"),
            Self::OutputCount(n) => write!(f, "a graph needs one output node, not {n}"),
            Self::DuplicateNode(id) => write!(f, "two nodes are called {id}"),
            Self::UnknownDevice(id) => write!(f, "effect {id} isn't in this chain"),
            Self::DuplicateDeviceNode(id) => write!(f, "effect {id} has two nodes"),
            Self::DeviceWithoutNode(id) => write!(f, "effect {id} has no node"),
            Self::BadSplit(id) => write!(f, "split {id} needs 1 to {MAX_CROSSOVERS} crossovers"),
            Self::NoSuchPort(port) => write!(f, "node {} has no port {}", port.node, port.port),
            Self::DuplicateCable(_) => write!(f, "two cables join the same ports"),
            Self::Cycle => write!(f, "that would send the sound round in a loop"),
        }
    }
}

impl std::error::Error for RoutingError {}

impl NodeKind {
    /// How many output ports it has.
    pub fn outputs(&self) -> u8 {
        match self {
            NodeKind::Output => 0,
            NodeKind::Input | NodeKind::Device(_) | NodeKind::Join(_) => 1,
            NodeKind::Split(Split::Bands { crossovers, .. }) => {
                crossovers.len().min(MAX_CROSSOVERS) as u8 + 1
            }
            NodeKind::Split(_) => 2,
        }
    }

    /// How many input ports it has. A device's second is its sidechain:
    /// there if its effect listens to one, or if this build doesn't know
    /// the effect (so a missing plugin's cables are kept).
    pub fn inputs(&self, devices: &[Device]) -> u8 {
        match self {
            NodeKind::Input => 0,
            NodeKind::Output | NodeKind::Split(_) => 1,
            NodeKind::Join(_) => 2,
            NodeKind::Device(id) => {
                let listens = devices
                    .iter()
                    .find(|d| d.id == *id)
                    .is_some_and(|d| d.effect.descriptor().is_none_or(|e| e.sidechain));
                1 + u8::from(listens)
            }
        }
    }
}

impl Routing {
    /// The graph of a linear chain: Input, each device in order, Output,
    /// laid out left to right. `new_id` hands out node ids.
    pub fn linear(devices: &[Device], mut new_id: impl FnMut() -> NodeId) -> Routing {
        let mut nodes = vec![GraphNode {
            id: new_id(),
            kind: NodeKind::Input,
            pos: [0.0, 0.0],
        }];
        let kinds = devices.iter().map(|d| NodeKind::Device(d.id));
        for (i, kind) in kinds.chain([NodeKind::Output]).enumerate() {
            nodes.push(GraphNode {
                id: new_id(),
                kind,
                pos: [(i + 1) as f32 * NODE_SPACING, 0.0],
            });
        }
        let cables = nodes
            .windows(2)
            .map(|pair| Cable {
                from: PortRef {
                    node: pair[0].id,
                    port: 0,
                },
                to: PortRef {
                    node: pair[1].id,
                    port: 0,
                },
                gain_db: 0.0,
                muted: false,
            })
            .collect();
        Routing { nodes, cables }
    }

    pub fn node(&self, id: NodeId) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The node of device `id`, if it has one.
    pub fn device_node(&self, id: DeviceId) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|n| n.kind == NodeKind::Device(id))
            .map(|n| n.id)
    }

    /// The Input node (the first, if a damaged graph has several).
    pub fn input(&self) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|n| n.kind == NodeKind::Input)
            .map(|n| n.id)
    }

    /// The Output node (the first, if a damaged graph has several).
    pub fn output(&self) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|n| n.kind == NodeKind::Output)
            .map(|n| n.id)
    }

    pub fn cable(&self, key: CableKey) -> Option<&Cable> {
        self.cables.iter().find(|c| c.key() == key)
    }

    /// Checks everything the engine and every edit rely on: one Input and
    /// one Output, a node for each of `devices` (the pool) and for nothing
    /// else, unique node ids, cables between ports that exist, no two
    /// between the same ports, and no loops.
    pub fn validate(&self, devices: &[Device]) -> Result<(), RoutingError> {
        let mut ids = HashSet::new();
        let mut placed = HashSet::new();
        let (mut inputs, mut outputs) = (0, 0);
        for node in &self.nodes {
            if !ids.insert(node.id) {
                return Err(RoutingError::DuplicateNode(node.id));
            }
            match &node.kind {
                NodeKind::Input => inputs += 1,
                NodeKind::Output => outputs += 1,
                NodeKind::Device(id) => {
                    if !devices.iter().any(|d| d.id == *id) {
                        return Err(RoutingError::UnknownDevice(*id));
                    }
                    if !placed.insert(*id) {
                        return Err(RoutingError::DuplicateDeviceNode(*id));
                    }
                }
                NodeKind::Split(Split::Bands { crossovers, .. }) => {
                    let count = (1..=MAX_CROSSOVERS).contains(&crossovers.len());
                    if !count || !crossovers.iter().all(|f| f.is_finite()) {
                        return Err(RoutingError::BadSplit(node.id));
                    }
                }
                NodeKind::Split(_) | NodeKind::Join(_) => {}
            }
        }
        if inputs != 1 {
            return Err(RoutingError::InputCount(inputs));
        }
        if outputs != 1 {
            return Err(RoutingError::OutputCount(outputs));
        }
        if let Some(device) = devices.iter().find(|d| !placed.contains(&d.id)) {
            return Err(RoutingError::DeviceWithoutNode(device.id));
        }
        let mut keys = HashSet::new();
        for cable in &self.cables {
            let from = self.node(cable.from.node);
            if from.is_none_or(|n| cable.from.port >= n.kind.outputs()) {
                return Err(RoutingError::NoSuchPort(cable.from));
            }
            let to = self.node(cable.to.node);
            if to.is_none_or(|n| cable.to.port >= n.kind.inputs(devices)) {
                return Err(RoutingError::NoSuchPort(cable.to));
            }
            if !keys.insert(cable.key()) {
                return Err(RoutingError::DuplicateCable(cable.key()));
            }
        }
        match self.topological_order() {
            Some(_) => Ok(()),
            None => Err(RoutingError::Cycle),
        }
    }

    /// Every node, each after every node that sends it sound; ties in the
    /// order of `nodes`, so it's the same every time. `None` if the cables
    /// make a loop. Cables to or from missing nodes are ignored.
    pub fn topological_order(&self) -> Option<Vec<NodeId>> {
        self.order(false)
    }

    /// Like [`Self::topological_order`], but a loop is broken at its first
    /// node in `nodes` order rather than failing: an order for a damaged
    /// graph.
    pub fn order_breaking_loops(&self) -> Vec<NodeId> {
        self.order(true).unwrap_or_default()
    }

    fn order(&self, break_loops: bool) -> Option<Vec<NodeId>> {
        let index = |id: NodeId| self.nodes.iter().position(|n| n.id == id);
        let edges: Vec<(usize, usize)> = self
            .cables
            .iter()
            .filter_map(|c| Some((index(c.from.node)?, index(c.to.node)?)))
            .collect();
        let mut waiting = vec![0usize; self.nodes.len()];
        for (_, to) in &edges {
            waiting[*to] += 1;
        }
        let mut ready: BTreeSet<usize> =
            (0..self.nodes.len()).filter(|i| waiting[*i] == 0).collect();
        let mut done = vec![false; self.nodes.len()];
        let mut order = Vec::with_capacity(self.nodes.len());
        while order.len() < self.nodes.len() {
            let next = match ready.pop_first() {
                Some(next) => next,
                None if break_loops => (0..self.nodes.len()).find(|i| !done[*i])?,
                None => return None,
            };
            if std::mem::replace(&mut done[next], true) {
                continue;
            }
            order.push(self.nodes[next].id);
            for (from, to) in &edges {
                if *from == next && !done[*to] {
                    waiting[*to] = waiting[*to].saturating_sub(1);
                    if waiting[*to] == 0 {
                        ready.insert(*to);
                    }
                }
            }
        }
        Some(order)
    }

    /// The devices in order, if the graph is just Input → each device →
    /// Output through plain cables (unity gain, unmuted, main ports), with
    /// nothing else in it: a linear chain.
    pub fn linear_chain(&self) -> Option<Vec<DeviceId>> {
        if self.cables.len() + 1 != self.nodes.len() {
            return None;
        }
        let mut chain = Vec::new();
        let mut at = self.input()?;
        let mut visited = 1;
        loop {
            let mut out = self.cables.iter().filter(|c| c.from.node == at);
            let (Some(cable), None) = (out.next(), out.next()) else {
                return None;
            };
            if !cable.is_plain() || cable.from.port != 0 || cable.to.port != 0 {
                return None;
            }
            at = cable.to.node;
            visited += 1;
            match self.node(at)?.kind {
                NodeKind::Output => break,
                NodeKind::Device(id) => chain.push(id),
                _ => return None,
            }
            if visited > self.nodes.len() {
                return None;
            }
        }
        (visited == self.nodes.len()).then_some(chain)
    }

    /// Takes a node out, joining whatever fed its main input to whatever it
    /// fed (gains add, a mute on either mutes the join), so the sound
    /// carries on around it. Cables into its other inputs go.
    pub fn splice_out(&mut self, node: NodeId) {
        let into: Vec<Cable> = self
            .cables
            .iter()
            .filter(|c| c.to.node == node && c.to.port == 0)
            .cloned()
            .collect();
        let out_of: Vec<Cable> = self
            .cables
            .iter()
            .filter(|c| c.from.node == node)
            .cloned()
            .collect();
        self.cables
            .retain(|c| c.from.node != node && c.to.node != node);
        self.nodes.retain(|n| n.id != node);
        for a in &into {
            for b in &out_of {
                let joined = Cable {
                    from: a.from,
                    to: b.to,
                    gain_db: a.gain_db + b.gain_db,
                    muted: a.muted || b.muted,
                };
                if self.cable(joined.key()).is_none() {
                    self.cables.push(joined);
                }
            }
        }
    }

    /// Adds a node with one input and one output (a device's) just before
    /// Output: everything that went into Output goes through it instead.
    /// Output moves a step right on the canvas to make room. Does nothing
    /// if there's no Output.
    pub fn splice_before_output(&mut self, id: NodeId, kind: NodeKind) {
        let Some(output) = self.nodes.iter_mut().find(|n| n.kind == NodeKind::Output) else {
            return;
        };
        let pos = output.pos;
        output.pos[0] += NODE_SPACING;
        let output = output.id;
        let new = PortRef { node: id, port: 0 };
        for cable in &mut self.cables {
            if cable.to
                == (PortRef {
                    node: output,
                    port: 0,
                })
            {
                cable.to = new;
            }
        }
        // Cables merged into one port might now be duplicates.
        let mut keys = HashSet::new();
        self.cables.retain(|c| keys.insert(c.key()));
        self.cables.push(Cable {
            from: new,
            to: PortRef {
                node: output,
                port: 0,
            },
            gain_db: 0.0,
            muted: false,
        });
        self.nodes.push(GraphNode { id, kind, pos });
    }
}

/// Brings a chain's graph back to canonical form after an edit to the chain
/// or its graph: nodes of devices that left the pool are spliced out;
/// devices that joined it with no node are spliced in before Output, in
/// pool order; a graph left linear becomes `None` with the pool in its
/// order; any other is kept with the pool in topological order.
pub(crate) fn normalize(
    devices: &mut [Device],
    routing: &mut Option<Routing>,
    mut new_id: impl FnMut() -> NodeId,
) {
    let Some(graph) = routing else {
        return;
    };
    let gone: Vec<NodeId> = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Device(id) if !devices.iter().any(|d| d.id == id)))
        .map(|n| n.id)
        .collect();
    for node in gone {
        graph.splice_out(node);
    }
    for device in devices.iter() {
        if graph.device_node(device.id).is_none() {
            graph.splice_before_output(new_id(), NodeKind::Device(device.id));
        }
    }
    let order: Vec<DeviceId> = match graph.linear_chain() {
        Some(chain) => {
            *routing = None;
            chain
        }
        None => match graph.topological_order() {
            Some(order) => order
                .iter()
                .filter_map(|id| match graph.node(*id)?.kind {
                    NodeKind::Device(device) => Some(device),
                    _ => None,
                })
                .collect(),
            None => return,
        },
    };
    sort_devices(devices, &order);
}

/// Puts `devices` in `order`; any not in it keep their places after.
pub(crate) fn sort_devices(devices: &mut [Device], order: &[DeviceId]) {
    devices.sort_by_key(|d| {
        order
            .iter()
            .position(|id| *id == d.id)
            .unwrap_or(usize::MAX)
    });
}

impl Project {
    /// A track's graph, or the master's (`None`); `None` if its chain is
    /// linear (or there's no such track).
    pub fn routing(&self, owner: Option<TrackId>) -> Option<&Routing> {
        match owner {
            Some(id) => self.track(id)?.routing.as_ref(),
            None => self.master_routing.as_ref(),
        }
    }

    /// A chain's devices and graph, together.
    pub(crate) fn chain_mut(
        &mut self,
        owner: Option<TrackId>,
    ) -> EditResult<(&mut Vec<Device>, &mut Option<Routing>)> {
        match owner {
            Some(id) => {
                let track = self.track_mut(id).ok_or(EditError::NoSuchTrack(id))?;
                Ok((&mut track.devices, &mut track.routing))
            }
            None => Ok((&mut self.master_devices, &mut self.master_routing)),
        }
    }

    /// A chain's graph, made from its linear order first if it has none, for
    /// edits that make it non-linear. Call [`Self::normalize_routing`] when
    /// done, or a graph left linear stays.
    pub fn ensure_routing(&mut self, owner: Option<TrackId>) -> EditResult<&mut Routing> {
        if self.chain_mut(owner)?.1.is_none() {
            let count = self.chain_mut(owner)?.0.len() + 2;
            let mut ids: Vec<NodeId> = (0..count).map(|_| self.new_node_id()).collect();
            ids.reverse();
            let (devices, routing) = self.chain_mut(owner)?;
            *routing = Some(Routing::linear(devices, || ids.pop().expect("enough ids")));
        }
        let (_, routing) = self.chain_mut(owner)?;
        Ok(routing.as_mut().expect("just made"))
    }

    /// Every graph node's place on the canvas, for carrying them over undo
    /// and redo: they're view settings, like folds.
    pub fn node_positions(&self) -> Vec<(NodeId, [f32; 2])> {
        let mut positions = Vec::new();
        let mut add = |routing: Option<&Routing>| {
            positions.extend(
                routing
                    .iter()
                    .flat_map(|r| r.nodes.iter().map(|n| (n.id, n.pos))),
            );
        };
        add(self.master_routing.as_ref());
        self.visit(|track, _| add(track.routing.as_ref()));
        positions
    }

    /// Moves a graph node on the canvas, wherever it is. A view setting:
    /// not an undo step.
    pub fn set_node_position(&mut self, node: NodeId, pos: [f32; 2]) -> EditResult {
        let mut found = false;
        let mut place = |routing: Option<&mut Routing>| {
            if let Some(n) = routing.and_then(|r| r.nodes.iter_mut().find(|n| n.id == node)) {
                n.pos = pos;
                found = true;
            }
        };
        place(self.master_routing.as_mut());
        self.visit_mut(|track| place(track.routing.as_mut()));
        if found {
            Ok(())
        } else {
            Err(EditError::NoSuchNode(node))
        }
    }

    /// Brings a chain's graph back to canonical form (see [`normalize`]).
    /// Every edit to a chain or its graph ends with this.
    pub fn normalize_routing(&mut self, owner: Option<TrackId>) -> EditResult {
        let (devices, routing) = self.chain_mut(owner)?;
        if routing.is_none() {
            return Ok(());
        }
        let mut devices = std::mem::take(devices);
        let mut graph = routing.take();
        normalize(&mut devices, &mut graph, || self.new_node_id());
        let (pool, routing) = self.chain_mut(owner)?;
        *pool = devices;
        *routing = graph;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use effects::EffectRef;

    use super::*;

    /// A project whose first track has an EQ, a compressor and a reverb.
    fn chain() -> (Project, TrackId, [DeviceId; 3]) {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let ids = ["canopy.eq", "canopy.compressor", "canopy.reverb"].map(|id| {
            p.add_device(Some(track), EffectRef::builtin(id), None)
                .unwrap()
        });
        (p, track, ids)
    }

    fn port(node: NodeId, port: u8) -> PortRef {
        PortRef { node, port }
    }

    fn cable(from: PortRef, to: PortRef) -> Cable {
        Cable {
            from,
            to,
            gain_db: 0.0,
            muted: false,
        }
    }

    #[test]
    fn a_linear_graph_is_valid_and_normalizes_away() {
        let (mut p, track, ids) = chain();
        let graph = p.ensure_routing(Some(track)).unwrap().clone();
        let devices = p.devices(Some(track)).unwrap();
        assert_eq!(graph.validate(devices), Ok(()));
        assert_eq!(graph.linear_chain(), Some(ids.to_vec()));
        assert_eq!(graph.nodes.len(), 5);
        assert_eq!(graph.topological_order().unwrap().len(), 5);
        p.normalize_routing(Some(track)).unwrap();
        assert!(p.routing(Some(track)).is_none());
    }

    #[test]
    fn node_positions_are_view_settings_found_anywhere() {
        let (mut p, track, _) = chain();
        p.ensure_routing(Some(track)).unwrap();
        p.ensure_routing(None).unwrap();
        let positions = p.node_positions();
        assert_eq!(positions.len(), 5 + 2, "the track's and the master's");
        let (node, _) = positions[3];
        p.set_node_position(node, [12.0, 34.0]).unwrap();
        assert!(p.node_positions().contains(&(node, [12.0, 34.0])));
        assert_eq!(
            p.set_node_position(NodeId(99_999), [0.0; 2]),
            Err(EditError::NoSuchNode(NodeId(99_999)))
        );
    }

    #[test]
    fn a_parallel_branch_stays_and_keeps_the_pool_in_order() {
        let (mut p, track, [eq, comp, reverb]) = chain();
        let graph = p.ensure_routing(Some(track)).unwrap();
        let input = graph.input().unwrap();
        let reverb_node = graph.device_node(reverb).unwrap();
        // A dry path around the reverb, at -6 dB.
        let comp_node = graph.device_node(comp).unwrap();
        let output = graph.output().unwrap();
        graph.cables.push(Cable {
            gain_db: -6.0,
            ..cable(port(comp_node, 0), port(output, 0))
        });
        p.normalize_routing(Some(track)).unwrap();
        let graph = p.routing(Some(track)).unwrap();
        assert!(graph.linear_chain().is_none());
        assert_eq!(graph.validate(p.devices(Some(track)).unwrap()), Ok(()));

        // Put the reverb first in the pool: normalizing restores sound order.
        let track_mut = p.track_mut(track).unwrap();
        track_mut.devices.rotate_right(1);
        p.normalize_routing(Some(track)).unwrap();
        let order: Vec<DeviceId> = p
            .devices(Some(track))
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(order, [eq, comp, reverb]);

        // A cycle is caught.
        let mut looped = p.routing(Some(track)).unwrap().clone();
        looped
            .cables
            .push(cable(port(reverb_node, 0), port(comp_node, 0)));
        assert_eq!(
            looped.validate(p.devices(Some(track)).unwrap()),
            Err(RoutingError::Cycle)
        );
        assert!(looped.topological_order().is_none());
        assert_eq!(looped.order_breaking_loops().len(), looped.nodes.len());
        assert_eq!(looped.order_breaking_loops()[0], input);
    }

    #[test]
    fn ports_and_membership_are_checked() {
        let (mut p, track, [eq, comp, _]) = chain();
        let graph = p.ensure_routing(Some(track)).unwrap().clone();
        let devices = p.devices(Some(track)).unwrap().to_vec();
        let eq_node = graph.device_node(eq).unwrap();
        let comp_node = graph.device_node(comp).unwrap();
        let output = graph.output().unwrap();

        // The compressor listens to a sidechain, so it has a second input.
        let mut sidechained = graph.clone();
        sidechained
            .cables
            .push(cable(port(eq_node, 0), port(comp_node, 1)));
        assert_eq!(sidechained.validate(&devices), Ok(()));
        let mut bad = graph.clone();
        bad.cables.push(cable(port(eq_node, 0), port(comp_node, 2)));
        assert_eq!(
            bad.validate(&devices),
            Err(RoutingError::NoSuchPort(port(comp_node, 2)))
        );
        let mut bad = graph.clone();
        bad.cables.push(cable(port(output, 0), port(comp_node, 1)));
        assert_eq!(
            bad.validate(&devices),
            Err(RoutingError::NoSuchPort(port(output, 0))),
            "Output has no outputs"
        );
        let mut bad = graph.clone();
        bad.cables.push(graph.cables[0].clone());
        assert!(matches!(
            bad.validate(&devices),
            Err(RoutingError::DuplicateCable(_))
        ));
        let mut bad = graph.clone();
        bad.nodes[1].id = bad.nodes[2].id;
        assert!(matches!(
            bad.validate(&devices),
            Err(RoutingError::DuplicateNode(_))
        ));
        let mut bad = graph.clone();
        bad.nodes.retain(|n| n.id != eq_node);
        assert_eq!(
            bad.validate(&devices),
            Err(RoutingError::DeviceWithoutNode(eq))
        );
        assert_eq!(
            graph.validate(&devices[1..]),
            Err(RoutingError::UnknownDevice(eq))
        );
        let mut bad = graph.clone();
        bad.nodes.push(GraphNode {
            id: NodeId(9999),
            kind: NodeKind::Split(Split::Bands {
                crossovers: vec![],
                slope: Slope::Db24,
            }),
            pos: [0.0; 2],
        });
        assert_eq!(
            bad.validate(&devices),
            Err(RoutingError::BadSplit(NodeId(9999)))
        );
        let mut bad = graph.clone();
        bad.nodes.retain(|n| n.kind != NodeKind::Output);
        assert_eq!(bad.validate(&devices), Err(RoutingError::OutputCount(0)));
    }

    #[test]
    fn splicing_keeps_the_sound_flowing() {
        let (mut p, track, [eq, comp, reverb]) = chain();
        let devices = p.devices(Some(track)).unwrap().to_vec();
        let graph = p.ensure_routing(Some(track)).unwrap();
        let comp_node = graph.device_node(comp).unwrap();
        graph.cables[1].gain_db = -3.0; // EQ → compressor
        graph.cables[2].gain_db = -2.0; // compressor → reverb
        graph.splice_out(comp_node);
        let eq_node = graph.device_node(eq).unwrap();
        let reverb_node = graph.device_node(reverb).unwrap();
        let joined = graph
            .cables
            .iter()
            .find(|c| c.from.node == eq_node)
            .unwrap();
        assert_eq!(joined.to, port(reverb_node, 0));
        assert_eq!(joined.gain_db, -5.0, "gains add");
        assert_eq!(graph.cables.len(), 3);

        graph.splice_before_output(NodeId(5000), NodeKind::Device(comp));
        assert_eq!(
            graph.validate(&devices),
            Ok(()),
            "the compressor is back, last"
        );
        let output = p.routing(Some(track)).unwrap().output().unwrap();
        let into_output: Vec<_> = p
            .routing(Some(track))
            .unwrap()
            .cables
            .iter()
            .filter(|c| c.to.node == output)
            .collect();
        assert_eq!(into_output.len(), 1);
        assert_eq!(into_output[0].from, port(NodeId(5000), 0));
    }
}
