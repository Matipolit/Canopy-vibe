//! Patchers, as the engine plays them: a chain holding Patchers becomes
//! one flat pool of effects and one graph, which the graph program
//! (`graph.rs`) runs like any other.
//!
//! The pool is every effect in the chain and inside its Patchers, the
//! Patchers themselves left out: a Patcher only passes its sound through
//! the effects it holds, so its node in the chain's graph is replaced by
//! its own graph (or, for a linear inside, by its effects in a line). Its
//! inner Input and Output go too: whatever fed the Patcher feeds what its
//! Input fed, and what fed its Output feeds what the Patcher fed, cable
//! levels adding up along the way. A bypassed Patcher becomes a plain wire,
//! and the effects inside keep their slots (so their instances, and their
//! tails, survive) without being in the sound's path.
//!
//! Node ids the project doesn't give (a linear chain has no graph) are
//! made from the ids of what they stand for, so they're the same from one
//! snapshot to the next and the graph program's state follows them: a
//! device's node is `NodeId(device id)`, which no real node can have, ids
//! coming from one counter; Inputs and Outputs take ids from the top of
//! the range, which the counter never reaches.
//!
//! A chain without Patchers is passed on as it is, so the linear fast path
//! stays.

use std::borrow::Cow;
use std::collections::HashMap;

use project::{Cable, CableKey, Device, DeviceId, GraphNode, NodeId, NodeKind, PortRef, Routing};

/// A chain as the engine plays it.
pub(crate) struct Flat<'a> {
    pub devices: Cow<'a, [Device]>,
    pub routing: Option<Cow<'a, Routing>>,
}

impl Flat<'_> {
    pub fn routing(&self) -> Option<&Routing> {
        self.routing.as_deref()
    }
}

/// `devices` and their `routing`, with any Patchers among them (or inside
/// those) opened up (see the module's header).
pub(crate) fn flatten<'a>(devices: &'a [Device], routing: Option<&'a Routing>) -> Flat<'a> {
    if !devices.iter().any(|d| d.patch.is_some()) {
        return Flat {
            devices: Cow::Borrowed(devices),
            routing: routing.map(Cow::Borrowed),
        };
    }
    let mut ids = SyntheticIds::default();
    let mut pool = Vec::new();
    let graph = open(devices, routing, &mut pool, &mut ids);
    Flat {
        devices: Cow::Owned(pool),
        routing: Some(Cow::Owned(graph)),
    }
}

/// Hands out Input and Output ids from the top of the range.
#[derive(Default)]
struct SyntheticIds {
    taken: u32,
}

impl SyntheticIds {
    fn next(&mut self) -> NodeId {
        self.taken += 1;
        NodeId(u32::MAX - self.taken)
    }
}

/// The graph of one chain with its Patchers opened, adding its effects (and
/// theirs) to `pool`.
fn open(
    devices: &[Device],
    routing: Option<&Routing>,
    pool: &mut Vec<Device>,
    ids: &mut SyntheticIds,
) -> Routing {
    let mut graph = match routing {
        Some(graph) => graph.clone(),
        None => linear(devices, ids),
    };
    for device in devices {
        let Some(patch) = &device.patch else {
            pool.push(device.clone());
            continue;
        };
        let Some(node) = graph.device_node(device.id) else {
            continue;
        };
        let start = pool.len();
        let inner = open(&patch.devices, patch.routing.as_ref(), pool, ids);
        if device.bypassed {
            // Its effects stay in the pool, unconnected, so they keep their
            // instances; the sound goes around them.
            splice(&mut graph, node);
            for inside in &pool[start..] {
                graph.nodes.push(GraphNode {
                    id: NodeId(inside.id.0),
                    kind: NodeKind::Device(inside.id),
                    pos: [0.0; 2],
                });
            }
            continue;
        }
        inline(&mut graph, node, inner);
    }
    graph
}

/// The graph of a linear chain, with ids made from what the nodes are.
fn linear(devices: &[Device], ids: &mut SyntheticIds) -> Routing {
    let (input, output) = (ids.next(), ids.next());
    let mut nodes: Vec<NodeId> = vec![input];
    nodes.extend(devices.iter().map(|d| NodeId(d.id.0)));
    nodes.push(output);
    let mut graph = Routing::linear(devices, {
        let mut nodes = nodes.into_iter();
        move || nodes.next().expect("one id per node")
    });
    for node in &mut graph.nodes {
        node.pos = [0.0; 2];
    }
    graph
}

/// Puts `inner` (a Patcher's graph, already opened) in place of the
/// Patcher's node in `graph`.
fn inline(graph: &mut Routing, patcher: NodeId, inner: Routing) {
    let (Some(input), Some(output)) = (inner.input(), inner.output()) else {
        splice(graph, patcher);
        return;
    };
    // What went into the Patcher goes into its Input, what came out of it
    // comes out of its Output; then both are spliced out.
    for cable in &mut graph.cables {
        if cable.to.node == patcher {
            cable.to = PortRef {
                node: input,
                port: 0,
            };
        }
        if cable.from.node == patcher {
            cable.from = PortRef {
                node: output,
                port: 0,
            };
        }
    }
    graph.nodes.retain(|n| n.id != patcher);
    // Inputs and Outputs are spliced like any node: as pass-throughs.
    graph
        .nodes
        .extend(inner.nodes.into_iter().map(|n| match n.kind {
            NodeKind::Input | NodeKind::Output => GraphNode {
                kind: NodeKind::Split(project::Split::LeftRight),
                ..n
            },
            _ => n,
        }));
    graph.cables.extend(inner.cables);
    splice(graph, input);
    splice(graph, output);
}

/// Takes a node out, joining what fed its first input to what it fed. A
/// cable that then runs between two ports a cable already joins is summed
/// into it, levels added as amplitudes, so a dry path inside a Patcher and
/// one around it both still sound.
fn splice(graph: &mut Routing, node: NodeId) {
    let into: Vec<Cable> = graph
        .cables
        .iter()
        .filter(|c| c.to.node == node && c.to.port == 0)
        .cloned()
        .collect();
    let out_of: Vec<Cable> = graph
        .cables
        .iter()
        .filter(|c| c.from.node == node)
        .cloned()
        .collect();
    graph
        .cables
        .retain(|c| c.from.node != node && c.to.node != node);
    graph.nodes.retain(|n| n.id != node);
    for a in &into {
        for b in &out_of {
            add_cable(
                graph,
                Cable {
                    from: a.from,
                    to: b.to,
                    gain_db: a.gain_db + b.gain_db,
                    muted: a.muted || b.muted,
                },
            );
        }
    }
}

fn add_cable(graph: &mut Routing, cable: Cable) {
    let key: CableKey = cable.key();
    let Some(existing) = graph.cables.iter_mut().find(|c| c.key() == key) else {
        graph.cables.push(cable);
        return;
    };
    let level = |c: &Cable| {
        if c.muted {
            0.0
        } else {
            10f32.powf(c.gain_db / 20.0)
        }
    };
    let sum = level(existing) + level(&cable);
    if sum <= 0.0 {
        existing.muted = true;
    } else {
        existing.gain_db = 20.0 * sum.log10();
        existing.muted = false;
    }
}

/// Every automatable knob a Patcher's macro lane moves, through Patchers
/// inside too: the device, its parameter, and how the knob's place (as a
/// fraction of its range) follows the macro's, `place = a + b * macro`.
pub(crate) fn macro_targets(patcher: &Device, param: &str) -> Vec<(DeviceId, String, f32, f32)> {
    let mut out = Vec::new();
    follow(patcher, param, 0.0, 1.0, &mut out);
    out
}

fn follow(
    patcher: &Device,
    param: &str,
    offset: f32,
    scale: f32,
    out: &mut Vec<(DeviceId, String, f32, f32)>,
) {
    let (Some(patch), Some(index)) = (&patcher.patch, project::macro_index(param)) else {
        return;
    };
    let inside: HashMap<DeviceId, &Device> = patch.devices.iter().map(|d| (d.id, d)).collect();
    for link in patch.links.iter().filter(|l| usize::from(l.index) == index) {
        // place = min + (max - min) * macro, macro = offset + scale * x.
        let span = link.max - link.min;
        let (a, b) = (link.min + span * offset, span * scale);
        match inside.get(&link.device) {
            Some(device)
                if device.patch.is_some() && project::macro_index(&link.param).is_some() =>
            {
                follow(device, &link.param, a, b, out);
            }
            Some(_) => out.push((link.device, link.param.clone(), a, b)),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use effects::EffectRef;
    use project::{Chain, PATCHER, Project, TrackId};

    use super::*;

    fn patched() -> (Project, TrackId, DeviceId, [DeviceId; 3]) {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let eq = p
            .add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let patcher = p
            .add_device(Some(track), EffectRef::builtin(PATCHER), None)
            .unwrap();
        let [a, b] = ["canopy.utility", "canopy.reverb"].map(|id| {
            p.add_device(Chain::Patch(patcher), EffectRef::builtin(id), None)
                .unwrap()
        });
        (p, track, patcher, [eq, a, b])
    }

    /// The pool's ids, and the graph, checked against the pool.
    fn flat(p: &Project, track: TrackId) -> (Vec<DeviceId>, Routing) {
        let t = p.track(track).unwrap();
        let flat = flatten(&t.devices, t.routing.as_ref());
        let graph = flat.routing.unwrap().into_owned();
        assert_eq!(graph.validate(&flat.devices), Ok(()));
        (flat.devices.iter().map(|d| d.id).collect(), graph)
    }

    #[test]
    fn a_chain_without_patchers_is_left_alone() {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        p.add_device(Some(track), EffectRef::builtin("canopy.eq"), None)
            .unwrap();
        let t = p.track(track).unwrap();
        let flat = flatten(&t.devices, None);
        assert!(matches!(flat.devices, Cow::Borrowed(_)));
        assert!(flat.routing.is_none());
    }

    #[test]
    fn a_patcher_opens_into_its_effects_in_line() {
        let (p, track, patcher, [eq, a, b]) = patched();
        let (pool, graph) = flat(&p, track);
        assert_eq!(pool, [eq, a, b]);
        assert!(!pool.contains(&patcher));
        assert_eq!(graph.linear_chain(), Some(vec![eq, a, b]));
    }

    #[test]
    fn a_bypassed_patcher_is_a_wire_and_keeps_its_effects() {
        let (mut p, track, patcher, [eq, a, b]) = patched();
        p.set_device_bypassed(patcher, true).unwrap();
        let (pool, graph) = flat(&p, track);
        assert_eq!(pool, [eq, a, b]);
        let input = graph.input().unwrap();
        let eq_node = graph.device_node(eq).unwrap();
        let out = graph.output().unwrap();
        let mut cables: Vec<_> = graph
            .cables
            .iter()
            .map(|c| (c.from.node, c.to.node))
            .collect();
        cables.sort();
        let mut expected = vec![(input, eq_node), (eq_node, out)];
        expected.sort();
        assert_eq!(cables, expected);
        assert!(graph.device_node(a).is_some());
    }

    #[test]
    fn a_patchers_graph_takes_its_place_with_levels_adding_up() {
        let (mut p, track, patcher, [eq, a, b]) = patched();
        let chain = Chain::Patch(patcher);
        let inner = p.ensure_routing(chain).unwrap().clone();
        let (input, b_node) = (inner.input().unwrap(), inner.device_node(b).unwrap());
        // A second path, Input straight to the reverb at -6 dB.
        let key = p
            .connect(
                chain,
                PortRef {
                    node: input,
                    port: 0,
                },
                PortRef {
                    node: b_node,
                    port: 0,
                },
            )
            .unwrap();
        p.set_cable(chain, key, -6.0, false).unwrap();
        // And the Patcher fed at -3 dB from the EQ.
        let outer = p.ensure_routing(Some(track)).unwrap().clone();
        let (eq_node, patch_node) = (
            outer.device_node(eq).unwrap(),
            outer.device_node(patcher).unwrap(),
        );
        let feed = outer
            .cables
            .iter()
            .find(|c| c.from.node == eq_node && c.to.node == patch_node)
            .unwrap()
            .key();
        p.set_cable(Some(track), feed, -3.0, false).unwrap();

        let (pool, graph) = flat(&p, track);
        assert_eq!(pool.len(), 3);
        let eq_to_b = graph
            .cables
            .iter()
            .find(|c| c.from.node == eq_node && c.to.node == b_node)
            .unwrap();
        assert_eq!(eq_to_b.gain_db, -9.0);
        let a_node = graph.device_node(a).unwrap();
        assert!(
            graph
                .cables
                .iter()
                .any(|c| c.from.node == eq_node && c.to.node == a_node)
        );
    }

    #[test]
    fn a_dry_path_inside_and_one_around_sum() {
        let mut graph = Routing::default();
        let (x, y) = (NodeId(1), NodeId(2));
        let cable = |gain_db| Cable {
            from: PortRef { node: x, port: 0 },
            to: PortRef { node: y, port: 0 },
            gain_db,
            muted: false,
        };
        graph.cables.push(cable(0.0));
        add_cable(&mut graph, cable(0.0));
        assert_eq!(graph.cables.len(), 1);
        assert!((graph.cables[0].gain_db - 6.0206).abs() < 1e-3);
    }

    #[test]
    fn macro_lanes_follow_links_through_patchers_inside() {
        let (mut p, _, outer, [_, a, b]) = patched();
        let inner = p
            .add_device(Chain::Patch(outer), EffectRef::builtin(PATCHER), None)
            .unwrap();
        p.move_devices(&[b], Chain::Patch(inner), 0).unwrap();
        p.link_macro(inner, 0, b, "wet", 0.2, 0.6).unwrap();
        p.link_macro(outer, 1, inner, "macro_1", 0.5, 1.0).unwrap();
        p.link_macro(outer, 1, a, "gain", 1.0, 0.0).unwrap();
        let targets = macro_targets(p.device(outer).unwrap(), "macro_2");
        // The utility: down from the top.
        assert!(targets.contains(&(a, "gain".to_owned(), 1.0, -1.0)));
        // The reverb: the inner macro runs 0.5..1, so its knob 0.4..0.6.
        let (_, _, base, scale) = targets.iter().find(|t| t.0 == b).unwrap();
        assert!((base - 0.4).abs() < 1e-6 && (scale - 0.2).abs() < 1e-6);
    }
}
