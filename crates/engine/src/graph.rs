//! Chains whose sound isn't a straight line: a `project::Routing` compiled
//! on the UI thread into a [`ChainProgram`] the audio thread walks.
//!
//! A chain without a graph compiles to [`ChainProgram::Linear`], today's
//! fast path: every device in pool order ([`chain::run`]). A graph compiles
//! to a [`GraphProgram`]. For now that's its devices in the order sound
//! reaches them (topological order), one after another; splits, joins,
//! parallel branches, cable levels and per-cable latency compensation
//! aren't scheduled yet, so a graph plays its effects in a sensible order
//! but in series.
//!
//! Effect instances aren't kept here but in the node's slots (one per pool
//! device, in pool order, so automation's slot indices hold), and graph
//! steps address them by index. State a graph itself needs across swaps
//! (filters, delay lines, gain ramps) is meant to live in
//! [`crate::chain::StateSlot`]s adopted in [`ChainProgram::adopt`].

use effects::Context;
use project::{CableKey, Device, NodeKind, Routing, TrackId};

use crate::arrangement::Arrangement;
use crate::chain::{self, EffectSlot};
use crate::pool::Sends;

/// How a chain runs.
pub(crate) enum ChainProgram {
    /// Every slot in order: a chain without a graph.
    Linear,
    Graph(Box<GraphProgram>),
}

/// A compiled graph.
pub(crate) struct GraphProgram {
    /// Slots in the order they run, one after another.
    order: Box<[u16]>,
}

/// What compiling a chain needs.
pub(crate) struct ChainInput<'a> {
    /// The chain's track, or `None` for the master's.
    #[allow(dead_code)] // for keying graph state
    pub owner: Option<TrackId>,
    /// The pool: slot `i` is device `i`.
    pub devices: &'a [Device],
    pub routing: Option<&'a Routing>,
    /// How late each device makes its sound, by pool index, in samples (0
    /// when bypassed): its descriptor's latency, or the plugin's.
    pub latencies: &'a [usize],
    #[allow(dead_code)] // for crossover filters
    pub sample_rate: u32,
}

/// What the compiler remembers about the graph state the engine holds, so
/// a snapshot builds state only for what's new. Nothing yet.
#[derive(Default)]
pub(crate) struct GraphState {}

impl GraphState {
    /// Before a snapshot's chains are compiled.
    pub fn begin(&mut self) {}

    /// Compiles one chain. Returns it and how late it makes its sound, in
    /// samples, which the compiler lines the tracks up by.
    pub fn compile(&mut self, input: ChainInput) -> (ChainProgram, usize) {
        let latency = input.latencies.iter().sum();
        let Some(routing) = input.routing else {
            return (ChainProgram::Linear, latency);
        };
        let Some(order) = routing.topological_order() else {
            // Edits and loading never leave a loop; if one got through,
            // play the pool in order.
            return (ChainProgram::Linear, latency);
        };
        let order = order
            .iter()
            .filter_map(|id| match routing.node(*id)?.kind {
                NodeKind::Device(device) => input.devices.iter().position(|d| d.id == device),
                _ => None,
            })
            .filter_map(|slot| u16::try_from(slot).ok())
            .collect();
        (
            ChainProgram::Graph(Box::new(GraphProgram { order })),
            latency,
        )
    }

    /// After a snapshot's chains are compiled: forget what it didn't use.
    pub fn finish(&mut self) {}

    /// Forget everything: the engine's state is gone (a snapshot couldn't
    /// be sent).
    pub fn invalidate(&mut self) {}
}

impl ChainProgram {
    /// Runs the chain over one block, in place. `effects` are its slots,
    /// `sends` the sidechain sources' latest blocks.
    pub fn run(
        &mut self,
        effects: &mut [EffectSlot],
        left: &mut [f32],
        right: &mut [f32],
        context: &Context,
        sends: Sends<'_>,
    ) {
        match self {
            ChainProgram::Linear => chain::run(effects, left, right, context, sends),
            ChainProgram::Graph(graph) => graph.run(effects, left, right, context, sends),
        }
    }

    /// When this program's snapshot arrives, on the audio thread: moves the
    /// state the old snapshot's programs hold into this one's slots that
    /// keep theirs. Pointer moves only.
    pub fn adopt(&mut self, _old: &mut Arrangement) {}

    /// A live cable level change ([`crate::Command::SetCableGain`]).
    pub fn set_cable_gain(&mut self, _cable: CableKey, _gain_db: f32) {}
}

impl GraphProgram {
    fn run(
        &mut self,
        effects: &mut [EffectSlot],
        left: &mut [f32],
        right: &mut [f32],
        context: &Context,
        sends: Sends<'_>,
    ) {
        let frames = left.len();
        for slot in &self.order {
            let Some(slot) = effects.get_mut(usize::from(*slot)) else {
                continue;
            };
            let context = Context {
                sidechain: chain::sidechain(slot, sends, frames),
                ..*context
            };
            chain::run_slot(slot, left, right, &context);
        }
    }
}

#[cfg(test)]
mod tests {
    use effects::EffectRef;
    use project::{Cable, PortRef, Project};

    use super::*;

    #[test]
    fn a_graph_runs_its_devices_in_sound_order() {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        let ids = ["canopy.eq", "canopy.utility", "canopy.reverb"].map(|id| {
            p.add_device(Some(track), EffectRef::builtin(id), None)
                .unwrap()
        });
        let graph = p.ensure_routing(Some(track)).unwrap();
        let (input, output) = (graph.input().unwrap(), graph.output().unwrap());
        graph.cables.push(Cable {
            from: PortRef {
                node: input,
                port: 0,
            },
            to: PortRef {
                node: output,
                port: 0,
            },
            gain_db: -6.0,
            muted: false,
        });
        let routing = graph.clone();
        // The pool in another order than the sound's.
        let mut devices = p.devices(Some(track)).unwrap().to_vec();
        devices.reverse();
        let (program, latency) = GraphState::default().compile(ChainInput {
            owner: Some(track),
            devices: &devices,
            routing: Some(&routing),
            latencies: &[1, 2, 3],
            sample_rate: 48_000,
        });
        assert_eq!(latency, 6);
        let ChainProgram::Graph(graph) = program else {
            panic!("a graph compiles to a graph program");
        };
        let order: Vec<_> = graph
            .order
            .iter()
            .map(|slot| devices[usize::from(*slot)].id)
            .collect();
        assert_eq!(order, ids);

        let (linear, _) = GraphState::default().compile(ChainInput {
            owner: Some(track),
            devices: &devices,
            routing: None,
            latencies: &[],
            sample_rate: 48_000,
        });
        assert!(matches!(linear, ChainProgram::Linear));
    }
}
