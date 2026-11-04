//! Chains whose sound isn't a straight line: a `project::Routing` compiled
//! on the UI thread into a [`ChainProgram`] the audio thread walks.
//!
//! A chain without a graph compiles to [`ChainProgram::Linear`], the fast
//! path: every device in pool order ([`chain::run`]). A graph compiles to a
//! [`GraphProgram`]: a flat list of [`Step`]s over numbered stereo buffers,
//! worked out once so the audio thread only follows it.
//!
//! - **Buffers.** Buffer 0 is the track's own block: the chain's input
//!   arrives in it and its output must leave in it. The others are scratch,
//!   sized here. Buffers are handed out like registers: a port takes over
//!   the buffer of the cable feeding it when nothing else will read that
//!   buffer any more (so a device processes in place, and a straight line
//!   needs no scratch and no copies at all), and otherwise gets a fresh one
//!   the cables are mixed into. A buffer is free again after its last
//!   reader. Of a buffer two ports read, the first to run copies it and the
//!   last takes it over.
//! - **Cables** carry a gain that glides (~5 ms, so level edits and mutes
//!   never click; new cables fade in from silence) and, when the sound
//!   arrives at a node at different times along different paths, a delay
//!   that lines it up (see [`ChainInput::latencies`]): every cable into a
//!   node, sidechain included, is delayed to arrive with the latest.
//! - **Nodes** that aren't reachable from Input, or don't reach Output, get
//!   no steps; their devices keep their slots, instances and tails.
//!
//! Effect instances aren't kept here but in the node's slots (one per pool
//! device, in pool order, so automation's slot indices hold), and graph
//! steps address them by index. What the graph itself holds across swaps
//! (crossover filters by node, delay lines and gain ramps by cable) lives in
//! [`StateSlot`]s, adopted from the program of the same chain in the
//! snapshot before: node ids and cables are unique within a chain only.

use std::collections::{BTreeSet, HashMap, HashSet};

use dsp::Smoothed;
use dsp::crossover::{Crossover, CrossoverSlope, MAX_BANDS};
use effects::Context;
use project::{CableKey, Device, Join, NodeId, NodeKind, Routing, Slope, Split, TrackId};

use crate::MAX_BLOCK;
use crate::arrangement::Arrangement;
use crate::chain::{self, EffectSlot, StateSlot};
use crate::compensation::Compensation;
use crate::pool::Sends;

/// How long a cable's level takes to glide to a new one.
const CABLE_RAMP_MS: f32 = 5.0;

/// The longest a cable is delayed to line its sound up: about 2.7 s at
/// 48 kHz, past any lookahead or linear-phase plugin.
const MAX_CABLE_DELAY: usize = (1 << 17) - 1;

/// The shortest delay line a cable gets: a little room to grow before a
/// longer one has to replace it.
const MIN_LINE: usize = 1023;

/// No buffer, delay line or port.
const NONE: u16 = u16::MAX;

/// The track's own block.
const TRACK: u16 = 0;

/// How a chain runs.
pub(crate) enum ChainProgram {
    /// Every slot in order: a chain without a graph.
    Linear,
    Graph(Box<GraphProgram>),
}

/// A compiled graph.
pub(crate) struct GraphProgram {
    /// The chain's track (`None` for the master's): whose program in the
    /// previous snapshot holds this one's state.
    owner: Option<TrackId>,
    steps: Box<[Step]>,
    /// Buffer `i + 1`; buffer 0 is the track's block.
    buffers: Box<[Scratch]>,
    /// Each cable's level ramp, and where it should be.
    gains: Box<[StateSlot<CableKey, Smoothed>]>,
    levels: Box<[Level]>,
    /// The cables that need lining up, and by how much.
    delays: Box<[StateSlot<CableKey, Compensation>]>,
    lags: Box<[Lag]>,
    /// Each band split's filters, and its settings.
    splits: Box<[StateSlot<NodeId, Crossover>]>,
    bands: Box<[Bands]>,
}

/// One scratch buffer: a stereo block.
struct Scratch {
    left: [f32; MAX_BLOCK],
    right: [f32; MAX_BLOCK],
}

impl Default for Scratch {
    fn default() -> Self {
        Scratch {
            left: [0.0; MAX_BLOCK],
            right: [0.0; MAX_BLOCK],
        }
    }
}

/// A cable's level as the project has it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Level {
    gain: f32,
    muted: bool,
}

impl Level {
    fn value(self) -> f32 {
        if self.muted { 0.0 } else { self.gain }
    }
}

/// How late a cable plays its sound.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lag {
    delay: usize,
    /// The line is new because the old one was too short for the delay: it
    /// takes the old one's sound over rather than starting empty.
    grown: bool,
}

/// A band split's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bands {
    slope: CrossoverSlope,
    crossovers: [f32; MAX_BANDS - 1],
    count: usize,
}

/// One thing to do, in order. Buffers, ramps, lines and splits are indices
/// into the program's; [`NONE`] is "not there".
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    /// A cable whose port took its buffer over: its delay and level, in
    /// place.
    Cable { buf: u16, gain: u16, delay: u16 },
    /// A cable into a port with a buffer of its own: `from` delayed and
    /// scaled, added to `to` or (the first cable into it) written over it.
    Mix {
        from: u16,
        to: u16,
        gain: u16,
        delay: u16,
        add: bool,
    },
    /// Silence: a port nothing feeds.
    Clear { buf: u16 },
    /// The chain's output into the track's block.
    Copy { from: u16, to: u16 },
    /// An effect on its main input, in place; its sidechain from a buffer,
    /// or ([`NONE`]) from the track it listens to.
    Effect { slot: u16, buf: u16, sidechain: u16 },
    Split {
        kind: SplitKind,
        input: u16,
        outputs: [u16; MAX_BANDS],
    },
    /// Decodes from the first channel of `a` and `b`.
    Join {
        kind: Join,
        a: u16,
        b: u16,
        out: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SplitKind {
    /// The split's filters: an index into [`GraphProgram::splits`].
    Bands(u16),
    LeftRight,
    MidSide,
}

/// What compiling a chain needs.
pub(crate) struct ChainInput<'a> {
    /// The chain's track, or `None` for the master's.
    pub owner: Option<TrackId>,
    /// The pool: slot `i` is device `i`.
    pub devices: &'a [Device],
    pub routing: Option<&'a Routing>,
    /// How late each device makes its sound, by pool index, in samples (0
    /// when bypassed): its descriptor's latency, or the plugin's.
    pub latencies: &'a [usize],
    pub sample_rate: u32,
}

/// What the compiler remembers about the graph state the engine holds, so
/// a snapshot builds state only for what's new, and knows what it can take
/// from the snapshot before.
#[derive(Default)]
pub(crate) struct GraphState {
    /// By chain owner, as of the last snapshot sent.
    current: HashMap<Option<TrackId>, Known>,
    /// The snapshot being compiled.
    next: HashMap<Option<TrackId>, Known>,
}

/// What one chain's program holds.
#[derive(Default)]
struct Known {
    /// It's a graph (rather than a linear chain).
    graph: bool,
    /// Cables with a level ramp.
    gains: HashSet<CableKey>,
    /// Cables with a delay line, and the longest delay each takes.
    delays: HashMap<CableKey, usize>,
    /// Band splits with filters.
    splits: HashSet<NodeId>,
}

impl GraphState {
    /// Before a snapshot's chains are compiled.
    pub fn begin(&mut self) {
        self.next.clear();
    }

    /// Compiles one chain. Returns it and how late it makes its sound, in
    /// samples, which the compiler lines the tracks up by.
    pub fn compile(&mut self, input: ChainInput) -> (ChainProgram, usize) {
        let compiled = input.routing.and_then(|routing| {
            let known = self.current.get(&input.owner);
            build(&input, routing, known)
        });
        match compiled {
            Some((program, latency, known)) => {
                self.next.insert(input.owner, known);
                (ChainProgram::Graph(Box::new(program)), latency)
            }
            None => {
                // No graph, or one that doesn't hold together (edits and
                // loading never leave one; if one got through, the pool
                // plays in order).
                self.next.insert(input.owner, Known::default());
                (ChainProgram::Linear, input.latencies.iter().sum())
            }
        }
    }

    /// After a snapshot's chains are compiled: forget what it didn't use.
    pub fn finish(&mut self) {
        self.current = std::mem::take(&mut self.next);
    }

    /// Forget everything: the engine's state is gone (a snapshot couldn't
    /// be sent).
    pub fn invalidate(&mut self) {
        self.current.clear();
        self.next.clear();
    }
}

/// One cable, with its ends as node indices.
struct Wire<'a> {
    key: CableKey,
    from: usize,
    from_port: u8,
    to: usize,
    to_port: u8,
    cable: &'a project::Cable,
}

/// Compiles a graph; `None` if it doesn't hold together. `known` is what
/// the engine holds for this chain.
fn build(
    input: &ChainInput,
    routing: &Routing,
    known: Option<&Known>,
) -> Option<(GraphProgram, usize, Known)> {
    let devices = input.devices;
    routing.validate(devices).ok()?;
    let most = devices
        .len()
        .max(routing.nodes.len())
        .max(routing.cables.len());
    if most >= usize::from(NONE) {
        return None;
    }
    let nodes = &routing.nodes;
    let index: HashMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
    let order: Vec<usize> = routing
        .topological_order()?
        .iter()
        .map(|id| index[id])
        .collect();
    let wires: Vec<Wire> = routing
        .cables
        .iter()
        .map(|cable| Wire {
            key: cable.key(),
            from: index[&cable.from.node],
            from_port: cable.from.port,
            to: index[&cable.to.node],
            to_port: cable.to.port,
            cable,
        })
        .collect();
    let input_node = index[&routing.input()?];
    let output_node = index[&routing.output()?];

    // Only what's on a path from Input to Output plays.
    let mut reached = vec![false; nodes.len()];
    reached[input_node] = true;
    for &n in &order {
        if reached[n] {
            for wire in wires.iter().filter(|w| w.from == n) {
                reached[wire.to] = true;
            }
        }
    }
    let mut reaches = vec![false; nodes.len()];
    reaches[output_node] = true;
    for &n in order.iter().rev() {
        if wires.iter().any(|w| w.from == n && reaches[w.to]) {
            reaches[n] = true;
        }
    }
    let active: Vec<bool> = (0..nodes.len()).map(|n| reached[n] && reaches[n]).collect();
    let live: Vec<&Wire> = wires
        .iter()
        .filter(|w| active[w.from] && active[w.to])
        .collect();

    let slot = |n: usize| match nodes[n].kind {
        NodeKind::Device(id) => devices.iter().position(|d| d.id == id),
        _ => None,
    };

    // When the sound gets to each node, and so how much each cable into it
    // must wait for the latest.
    let mut arrive_in = vec![0usize; nodes.len()];
    let mut arrive_out = vec![0usize; nodes.len()];
    for &n in order.iter().filter(|n| active[**n]) {
        if n != input_node {
            arrive_in[n] = live
                .iter()
                .filter(|w| w.to == n)
                .map(|w| arrive_out[w.from])
                .max()
                .unwrap_or(0);
        }
        let latency = slot(n).and_then(|s| input.latencies.get(s)).copied();
        arrive_out[n] = arrive_in[n].saturating_add(latency.unwrap_or(0));
    }
    let latency = if active[output_node] {
        arrive_in[output_node]
    } else {
        0
    };

    let rate = input.sample_rate as f32;
    let mut next = Known {
        graph: true,
        ..Known::default()
    };

    // A cable's level: where its ramp starts, if the engine hasn't one.
    let on_line = |wire: &Wire| {
        let position = |n: usize| match nodes[n].kind {
            NodeKind::Input => Some(0),
            NodeKind::Output => Some(devices.len() + 1),
            NodeKind::Device(id) => devices.iter().position(|d| d.id == id).map(|p| p + 1),
            _ => None,
        };
        wire.from_port == 0
            && wire.to_port == 0
            && matches!((position(wire.from), position(wire.to)), (Some(a), Some(b)) if b == a + 1)
    };
    let mut gains = Vec::with_capacity(live.len());
    let mut levels = Vec::with_capacity(live.len());
    for wire in &live {
        let level = Level {
            gain: dsp::db_to_gain(wire.cable.gain_db),
            muted: wire.cable.muted,
        };
        levels.push(level);
        next.gains.insert(wire.key);
        if known.is_some_and(|k| k.gains.contains(&wire.key)) {
            gains.push(StateSlot::kept(wire.key));
            continue;
        }
        // New cables fade in, unless nothing was playing (a first
        // snapshot, an export) or the chain was a line until now and this
        // cable is one of its links, which were playing all along.
        let start = match known {
            None => level.value(),
            Some(k) if !k.graph && on_line(wire) => level.value(),
            Some(_) => 0.0,
        };
        let mut ramp = Smoothed::new(start, CABLE_RAMP_MS);
        ramp.prepare(rate);
        if start != level.value() {
            ramp.set_target(level.value());
        }
        gains.push(StateSlot::new(wire.key, ramp));
    }

    let mut delays = Vec::new();
    let mut lags = Vec::new();
    let mut delay_of = vec![NONE; live.len()];
    for (i, wire) in live.iter().enumerate() {
        let delay = (arrive_in[wire.to] - arrive_out[wire.from]).min(MAX_CABLE_DELAY);
        let had = known.and_then(|k| k.delays.get(&wire.key)).copied();
        let (slot, max, grown) = match had {
            Some(max) if delay <= max => (StateSlot::kept(wire.key), max, false),
            None if delay == 0 => continue,
            _ => {
                let max = (delay + 1).next_power_of_two().max(MIN_LINE + 1) - 1;
                let max = max.min(MAX_CABLE_DELAY);
                // A cable that was already playing glides from no delay; a
                // new one fades in at its delay anyway.
                let playing = gains[i].value.is_none();
                let start = if playing { 0 } else { delay };
                let mut line = Compensation::sized(start, max);
                line.set(delay);
                (StateSlot::new(wire.key, line), max, had.is_some())
            }
        };
        next.delays.insert(wire.key, max);
        delay_of[i] = delays.len() as u16;
        delays.push(slot);
        lags.push(Lag { delay, grown });
    }

    let mut splits = Vec::new();
    let mut bands = Vec::new();
    let mut split_of = vec![NONE; nodes.len()];
    for n in (0..nodes.len()).filter(|n| active[*n]) {
        let NodeKind::Split(Split::Bands { crossovers, slope }) = &nodes[n].kind else {
            continue;
        };
        let slope = match slope {
            Slope::Db12 => CrossoverSlope::Db12,
            Slope::Db24 => CrossoverSlope::Db24,
            Slope::Db48 => CrossoverSlope::Db48,
        };
        let mut settings = Bands {
            slope,
            crossovers: [0.0; MAX_BANDS - 1],
            count: crossovers.len().min(MAX_BANDS - 1),
        };
        settings.crossovers[..settings.count].copy_from_slice(&crossovers[..settings.count]);
        let id = nodes[n].id;
        next.splits.insert(id);
        split_of[n] = splits.len() as u16;
        splits.push(if known.is_some_and(|k| k.splits.contains(&id)) {
            StateSlot::kept(id)
        } else {
            StateSlot::new(
                id,
                Crossover::new(rate, slope, &crossovers[..settings.count]),
            )
        });
        bands.push(settings);
    }

    let (steps, buffers) = schedule(&Schedule {
        nodes,
        order: &order,
        live: &live,
        active: &active,
        input: input_node,
        output: output_node,
        delay_of: &delay_of,
        split_of: &split_of,
        slot: &slot,
    });
    let program = GraphProgram {
        owner: input.owner,
        steps: steps.into(),
        buffers: (1..buffers).map(|_| Scratch::default()).collect(),
        gains: gains.into(),
        levels: levels.into(),
        delays: delays.into(),
        lags: lags.into(),
        splits: splits.into(),
        bands: bands.into(),
    };
    Some((program, latency, next))
}

/// What [`schedule`] works from.
struct Schedule<'a> {
    nodes: &'a [project::GraphNode],
    /// Node indices, in topological order.
    order: &'a [usize],
    /// The cables that play; a cable's index here is its gain ramp's.
    live: &'a [&'a Wire<'a>],
    active: &'a [bool],
    input: usize,
    output: usize,
    /// Each live cable's delay line, if it has one.
    delay_of: &'a [u16],
    /// Each node's band split filters, if it's a band split.
    split_of: &'a [u16],
    /// Each node's device slot, if it's a device.
    slot: &'a dyn Fn(usize) -> Option<usize>,
}

/// Buffers handed out like registers: the lowest free one first, so the
/// schedule is the same every time.
struct Registers {
    free: BTreeSet<u16>,
    count: u16,
}

impl Registers {
    fn take(&mut self, prefer_track: bool) -> u16 {
        if prefer_track && self.free.remove(&TRACK) {
            return TRACK;
        }
        self.free.pop_first().unwrap_or_else(|| {
            self.count += 1;
            self.count - 1
        })
    }

    fn give_back(&mut self, buf: u16) {
        if buf != NONE {
            self.free.insert(buf);
        }
    }
}

/// The steps that play a graph, and how many buffers they use (the
/// track's block included): each active node in topological order, its
/// input ports gathered from their cables, then the node itself.
fn schedule(s: &Schedule) -> (Vec<Step>, u16) {
    let mut steps = Vec::new();
    if !s.active[s.output] {
        // Nothing reaches the output.
        steps.push(Step::Clear { buf: TRACK });
        return (steps, 1);
    }
    // Each node's output buffers, and how many cables still have to read
    // each.
    let mut out = vec![[NONE; MAX_BANDS]; s.nodes.len()];
    let mut readers = vec![[0usize; MAX_BANDS]; s.nodes.len()];
    for wire in s.live {
        readers[wire.from][usize::from(wire.from_port)] += 1;
    }
    let mut registers = Registers {
        free: BTreeSet::new(),
        count: 1,
    };
    out[s.input][0] = TRACK;

    for &n in s.order.iter().filter(|n| s.active[**n] && **n != s.input) {
        let kind = &s.nodes[n].kind;
        let mut inputs = [NONE; 2];
        for (port, buf) in inputs.iter_mut().enumerate() {
            let cables: Vec<usize> = (0..s.live.len())
                .filter(|c| s.live[*c].to == n && usize::from(s.live[*c].to_port) == port)
                .collect();
            if cables.is_empty() {
                continue;
            }
            let source = |c: usize| {
                let wire = s.live[c];
                (wire.from, usize::from(wire.from_port))
            };
            // A cable that's the last to read its buffer hands it over; the
            // output would rather have the track's block, saving a copy.
            let last = |c: &usize| {
                let (from, port) = source(*c);
                readers[from][port] == 1
            };
            let taken = cables
                .iter()
                .copied()
                .filter(last)
                .min_by_key(|c| n != s.output || out[source(*c).0][source(*c).1] != TRACK);
            let mut add = taken.is_some();
            *buf = match taken {
                Some(c) => {
                    let (from, port) = source(c);
                    readers[from][port] = 0;
                    let buf = out[from][port];
                    steps.push(Step::Cable {
                        buf,
                        gain: c as u16,
                        delay: s.delay_of[c],
                    });
                    buf
                }
                None => registers.take(n == s.output),
            };
            for c in cables.into_iter().filter(|c| Some(*c) != taken) {
                let (from, port) = source(c);
                steps.push(Step::Mix {
                    from: out[from][port],
                    to: *buf,
                    gain: c as u16,
                    delay: s.delay_of[c],
                    add,
                });
                add = true;
                readers[from][port] -= 1;
                if readers[from][port] == 0 {
                    registers.give_back(out[from][port]);
                }
            }
        }

        match kind {
            NodeKind::Input => {}
            NodeKind::Output => match inputs[0] {
                TRACK => {}
                NONE => steps.push(Step::Clear { buf: TRACK }),
                from => steps.push(Step::Copy { from, to: TRACK }),
            },
            NodeKind::Device(_) => {
                let main = match inputs[0] {
                    // Only a sidechain feeds it: its main input is silent.
                    NONE => {
                        let buf = registers.take(false);
                        steps.push(Step::Clear { buf });
                        buf
                    }
                    buf => buf,
                };
                // Validation keeps cables off a sidechain port a device
                // doesn't have.
                let sidechain = inputs[1];
                if let Some(slot) = (s.slot)(n) {
                    steps.push(Step::Effect {
                        slot: slot as u16,
                        buf: main,
                        sidechain,
                    });
                }
                registers.give_back(sidechain);
                out[n][0] = main;
            }
            NodeKind::Split(split) => {
                // The split reads its input whole before writing, so a
                // band may land back in the input's buffer.
                registers.give_back(inputs[0]);
                let mut outputs = [NONE; MAX_BANDS];
                for (port, buf) in outputs
                    .iter_mut()
                    .enumerate()
                    .take(usize::from(kind.outputs()))
                {
                    if readers[n][port] > 0 {
                        *buf = registers.take(false);
                    }
                }
                out[n] = outputs;
                let kind = match split {
                    Split::Bands { .. } => SplitKind::Bands(s.split_of[n]),
                    Split::LeftRight => SplitKind::LeftRight,
                    Split::MidSide => SplitKind::MidSide,
                };
                steps.push(Step::Split {
                    kind,
                    input: inputs[0],
                    outputs,
                });
            }
            NodeKind::Join(join) => {
                registers.give_back(inputs[0]);
                registers.give_back(inputs[1]);
                let buf = registers.take(false);
                steps.push(Step::Join {
                    kind: *join,
                    a: inputs[0],
                    b: inputs[1],
                    out: buf,
                });
                out[n][0] = buf;
            }
        }
        // An output nothing reads (only a band split's can be) is free.
        for port in 0..MAX_BANDS {
            if out[n][port] != NONE && readers[n][port] == 0 && n != s.output {
                registers.give_back(out[n][port]);
            }
        }
    }
    (steps, registers.count)
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
    /// state the same chain's program in the old snapshot holds into this
    /// one's slots that keep theirs. Pointer moves and small copies only.
    pub fn adopt(&mut self, old: &mut Arrangement) {
        let ChainProgram::Graph(graph) = self else {
            return;
        };
        let previous = match graph.owner {
            Some(track) => old.node_mut(track).map(|n| &mut n.program),
            None => Some(&mut old.master_program),
        };
        if let Some(ChainProgram::Graph(previous)) = previous {
            graph.adopt(previous);
        }
    }

    /// A live cable level change ([`crate::Command::SetCableGain`]).
    pub fn set_cable_gain(&mut self, cable: CableKey, gain_db: f32) {
        let ChainProgram::Graph(graph) = self else {
            return;
        };
        let Some(i) = graph.gains.iter().position(|g| g.key == cable) else {
            return;
        };
        let level = &mut graph.levels[i];
        level.gain = dsp::db_to_gain(gain_db);
        if let Some(ramp) = graph.gains[i].value.as_deref_mut() {
            ramp.set_target(level.value());
        }
    }
}

impl GraphProgram {
    fn adopt(&mut self, old: &mut GraphProgram) {
        StateSlot::adopt(&mut self.gains, |key| StateSlot::take(&mut old.gains, key));
        for (slot, level) in self.gains.iter_mut().zip(self.levels.iter()) {
            if let Some(ramp) = slot.value.as_deref_mut()
                && ramp.target() != level.value()
            {
                ramp.set_target(level.value());
            }
        }
        for (slot, lag) in self.delays.iter_mut().zip(self.lags.iter()) {
            match slot.value.as_deref_mut() {
                None => slot.value = StateSlot::take(&mut old.delays, &slot.key),
                Some(line) if lag.grown => {
                    let outgrown = old.delays.iter().find(|o| o.key == slot.key);
                    if let Some(outgrown) = outgrown.and_then(|o| o.value.as_deref()) {
                        line.take_over(outgrown);
                    }
                }
                Some(_) => {}
            }
            if let Some(line) = slot.value.as_deref_mut() {
                line.set(lag.delay);
            }
        }
        for (slot, bands) in self.splits.iter_mut().zip(self.bands.iter()) {
            if slot.value.is_none() {
                slot.value = StateSlot::take(&mut old.splits, &slot.key);
                if let Some(crossover) = slot.value.as_deref_mut() {
                    crossover.configure(bands.slope, &bands.crossovers[..bands.count]);
                }
            }
        }
    }

    fn run(
        &mut self,
        effects: &mut [EffectSlot],
        left: &mut [f32],
        right: &mut [f32],
        context: &Context,
        sends: Sends<'_>,
    ) {
        let frames = left.len().min(right.len()).min(MAX_BLOCK);
        let GraphProgram {
            steps,
            buffers,
            gains,
            levels,
            delays,
            splits,
            ..
        } = self;
        let mut bufs = Buffers {
            left,
            right,
            scratch: buffers,
            frames,
        };
        for step in steps.iter() {
            match *step {
                Step::Cable {
                    buf,
                    gain: g,
                    delay,
                } => {
                    let (l, r) = bufs.get_mut(buf);
                    if let Some(line) = line(delays, delay) {
                        line.process(l, r);
                    }
                    Gain::of(gains, levels, g).apply(l, r);
                }
                Step::Mix {
                    from,
                    to,
                    gain: g,
                    delay,
                    add,
                } => match line(delays, delay) {
                    Some(line) => {
                        let (mut l, mut r) = ([0.0; MAX_BLOCK], [0.0; MAX_BLOCK]);
                        let (l, r) = (&mut l[..frames], &mut r[..frames]);
                        let (from_l, from_r) = bufs.get(from);
                        l.copy_from_slice(from_l);
                        r.copy_from_slice(from_r);
                        line.process(l, r);
                        Gain::of(gains, levels, g).mix((l, r), bufs.get_mut(to), add);
                    }
                    None => {
                        if let Some((from, to)) = bufs.pair(from, to) {
                            Gain::of(gains, levels, g).mix(from, to, add);
                        }
                    }
                },
                Step::Clear { buf } => {
                    let (l, r) = bufs.get_mut(buf);
                    l.fill(0.0);
                    r.fill(0.0);
                }
                Step::Copy { from, to } => {
                    if let Some(((from_l, from_r), (l, r))) = bufs.pair(from, to) {
                        l.copy_from_slice(from_l);
                        r.copy_from_slice(from_r);
                    }
                }
                Step::Effect {
                    slot,
                    buf,
                    sidechain,
                } => {
                    let Some(slot) = effects.get_mut(usize::from(slot)) else {
                        continue;
                    };
                    if sidechain == NONE {
                        let (l, r) = bufs.get_mut(buf);
                        let context = Context {
                            sidechain: chain::sidechain(slot, sends, frames),
                            ..*context
                        };
                        chain::run_slot(slot, l, r, &context);
                    } else if let Some((sidechain, (l, r))) = bufs.pair(sidechain, buf) {
                        let context = Context {
                            sidechain: Some(sidechain),
                            ..*context
                        };
                        chain::run_slot(slot, l, r, &context);
                    }
                }
                Step::Split {
                    kind,
                    input,
                    outputs,
                } => {
                    let mut bands = [[[0.0f32; MAX_BLOCK]; 2]; MAX_BANDS];
                    let (l, r) = bufs.get(input);
                    match kind {
                        SplitKind::LeftRight => {
                            bands[0] = [pad(l), pad(l)];
                            bands[1] = [pad(r), pad(r)];
                        }
                        SplitKind::MidSide => {
                            for (i, (l, r)) in l.iter().zip(r).enumerate() {
                                let (mid, side) = ((l + r) * 0.5, (l - r) * 0.5);
                                bands[0][0][i] = mid;
                                bands[0][1][i] = mid;
                                bands[1][0][i] = side;
                                bands[1][1][i] = side;
                            }
                        }
                        SplitKind::Bands(index) => {
                            let crossover = splits
                                .get_mut(usize::from(index))
                                .and_then(|s| s.value.as_deref_mut());
                            if let Some(crossover) = crossover {
                                crossover.begin_block(frames);
                                for (channel, input) in [l, r].into_iter().enumerate() {
                                    for (i, x) in input.iter().enumerate() {
                                        let split = crossover.tick(channel, *x);
                                        for (band, value) in bands.iter_mut().zip(split) {
                                            band[channel][i] = value;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for (band, out) in bands.iter().zip(outputs) {
                        if out != NONE {
                            let (l, r) = bufs.get_mut(out);
                            l.copy_from_slice(&band[0][..frames]);
                            r.copy_from_slice(&band[1][..frames]);
                        }
                    }
                }
                Step::Join { kind, a, b, out } => {
                    let first = |buf: u16| match buf {
                        NONE => [0.0; MAX_BLOCK],
                        buf => pad(bufs.get(buf).0),
                    };
                    let (a, b) = (first(a), first(b));
                    let (l, r) = bufs.get_mut(out);
                    for (i, (l, r)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
                        (*l, *r) = match kind {
                            Join::LeftRight => (a[i], b[i]),
                            Join::MidSide => (a[i] + b[i], a[i] - b[i]),
                        };
                    }
                }
            }
        }
    }
}

/// Delay line `index`, if there is one.
fn line(delays: &mut [StateSlot<CableKey, Compensation>], index: u16) -> Option<&mut Compensation> {
    delays
        .get_mut(usize::from(index))
        .and_then(|d| d.value.as_deref_mut())
}

/// A block copied into a whole one, zeros past its end.
fn pad(block: &[f32]) -> [f32; MAX_BLOCK] {
    let mut whole = [0.0; MAX_BLOCK];
    whole[..block.len()].copy_from_slice(block);
    whole
}

/// A cable's level for one block: its ramp, or (if the engine somehow has
/// none) where it should be.
struct Gain<'a> {
    ramp: Option<&'a mut Smoothed>,
    level: f32,
}

impl<'a> Gain<'a> {
    fn of(gains: &'a mut [StateSlot<CableKey, Smoothed>], levels: &[Level], index: u16) -> Self {
        let index = usize::from(index);
        Gain {
            ramp: gains.get_mut(index).and_then(|g| g.value.as_deref_mut()),
            level: levels.get(index).map_or(1.0, |l| l.value()),
        }
    }

    /// Where it sits, if it isn't moving.
    fn still(&self) -> Option<f32> {
        match &self.ramp {
            Some(ramp) if ramp.is_smoothing() => None,
            Some(ramp) => Some(ramp.current()),
            None => Some(self.level),
        }
    }

    /// Scales a block in place. At unity it leaves it untouched.
    fn apply(&mut self, left: &mut [f32], right: &mut [f32]) {
        match self.still() {
            Some(1.0) => {}
            Some(0.0) => {
                left.fill(0.0);
                right.fill(0.0);
            }
            Some(g) => {
                for s in left.iter_mut().chain(right.iter_mut()) {
                    *s *= g;
                }
            }
            None => {
                let ramp = self.ramp.as_deref_mut().expect("moving");
                for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                    let g = ramp.tick();
                    *l *= g;
                    *r *= g;
                }
            }
        }
    }

    /// Adds `from`, scaled, to `to`, or writes it over `to`.
    fn mix(&mut self, from: (&[f32], &[f32]), to: (&mut [f32], &mut [f32]), add: bool) {
        let ((from_l, from_r), (to_l, to_r)) = (from, to);
        match (self.still(), add) {
            (Some(0.0), true) => {}
            (Some(0.0), false) => {
                to_l.fill(0.0);
                to_r.fill(0.0);
            }
            (Some(1.0), false) => {
                to_l.copy_from_slice(from_l);
                to_r.copy_from_slice(from_r);
            }
            (Some(1.0), true) => {
                for (to, from) in to_l
                    .iter_mut()
                    .zip(from_l)
                    .chain(to_r.iter_mut().zip(from_r))
                {
                    *to += from;
                }
            }
            (Some(g), _) => {
                for (to, from) in to_l
                    .iter_mut()
                    .zip(from_l)
                    .chain(to_r.iter_mut().zip(from_r))
                {
                    *to = if add { *to + from * g } else { from * g };
                }
            }
            (None, _) => {
                let ramp = self.ramp.as_deref_mut().expect("moving");
                let samples = to_l.iter_mut().zip(to_r.iter_mut());
                for ((to_l, to_r), (from_l, from_r)) in samples.zip(from_l.iter().zip(from_r)) {
                    let g = ramp.tick();
                    if add {
                        *to_l += from_l * g;
                        *to_r += from_r * g;
                    } else {
                        *to_l = from_l * g;
                        *to_r = from_r * g;
                    }
                }
            }
        }
    }
}

/// The buffers a program's steps address: the track's block as 0, the
/// scratch after it, each `frames` long.
struct Buffers<'a> {
    left: &'a mut [f32],
    right: &'a mut [f32],
    scratch: &'a mut [Scratch],
    frames: usize,
}

type Block<'a> = (&'a [f32], &'a [f32]);
type BlockMut<'a> = (&'a mut [f32], &'a mut [f32]);

impl Buffers<'_> {
    fn get(&self, buf: u16) -> Block<'_> {
        let n = self.frames;
        match buf {
            TRACK => (&self.left[..n], &self.right[..n]),
            buf => {
                let s = &self.scratch[usize::from(buf) - 1];
                (&s.left[..n], &s.right[..n])
            }
        }
    }

    fn get_mut(&mut self, buf: u16) -> BlockMut<'_> {
        let n = self.frames;
        match buf {
            TRACK => (&mut self.left[..n], &mut self.right[..n]),
            buf => {
                let s = &mut self.scratch[usize::from(buf) - 1];
                (&mut s.left[..n], &mut s.right[..n])
            }
        }
    }

    /// `from` to read and `to` to write, which must differ.
    fn pair(&mut self, from: u16, to: u16) -> Option<(Block<'_>, BlockMut<'_>)> {
        let n = self.frames;
        match (from, to) {
            _ if from == to => None,
            (TRACK, to) => {
                let s = &mut self.scratch[usize::from(to) - 1];
                Some((
                    (&self.left[..n], &self.right[..n]),
                    (&mut s.left[..n], &mut s.right[..n]),
                ))
            }
            (from, TRACK) => {
                let s = &self.scratch[usize::from(from) - 1];
                Some((
                    (&s.left[..n], &s.right[..n]),
                    (&mut self.left[..n], &mut self.right[..n]),
                ))
            }
            (from, to) => {
                let [a, b] = self
                    .scratch
                    .get_disjoint_mut([usize::from(from) - 1, usize::from(to) - 1])
                    .ok()?;
                Some((
                    (&a.left[..n], &a.right[..n]),
                    (&mut b.left[..n], &mut b.right[..n]),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use effects::EffectRef;
    use project::{Cable, GraphNode, PortRef, Project};

    use super::*;

    fn cable(from: NodeId, from_port: u8, to: NodeId, to_port: u8) -> Cable {
        Cable {
            from: PortRef {
                node: from,
                port: from_port,
            },
            to: PortRef {
                node: to,
                port: to_port,
            },
            gain_db: 0.0,
            muted: false,
        }
    }

    /// A track with `effects` and the linear graph over them.
    fn chain(effects: &[&str]) -> (Project, TrackId, Routing) {
        let mut p = Project::with_default_tracks();
        let track = p.track_ids()[0];
        for id in effects {
            p.add_device(Some(track), EffectRef::builtin(id), None)
                .unwrap();
        }
        let routing = p.ensure_routing(Some(track)).unwrap().clone();
        (p, track, routing)
    }

    fn compile(
        p: &Project,
        track: TrackId,
        routing: &Routing,
        latencies: &[usize],
    ) -> (GraphProgram, usize) {
        let (program, latency) = GraphState::default().compile(ChainInput {
            owner: Some(track),
            devices: p.devices(Some(track)).unwrap(),
            routing: Some(routing),
            latencies,
            sample_rate: 48_000,
        });
        let ChainProgram::Graph(graph) = program else {
            panic!("a graph compiles to a graph program");
        };
        (*graph, latency)
    }

    fn node(routing: &Routing, kind: &NodeKind) -> NodeId {
        routing.nodes.iter().find(|n| n.kind == *kind).unwrap().id
    }

    #[test]
    fn a_straight_line_runs_in_place_in_the_track_block() {
        let (p, track, routing) = chain(&["canopy.eq", "canopy.utility", "canopy.reverb"]);
        let (graph, latency) = compile(&p, track, &routing, &[1, 2, 3]);
        assert_eq!(latency, 6);
        assert!(graph.buffers.is_empty(), "no scratch");
        assert!(graph.delays.is_empty());
        let effects: Vec<_> = graph
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Effect { slot, buf, .. } => Some((*slot, *buf)),
                Step::Cable { buf: TRACK, .. } => None,
                other => panic!("a copy or a mix: {other:?}"),
            })
            .collect();
        assert_eq!(effects, [(0, TRACK), (1, TRACK), (2, TRACK)]);
    }

    #[test]
    fn devices_run_in_sound_order_whatever_the_pool_order() {
        let (p, track, mut routing) = chain(&["canopy.eq", "canopy.utility"]);
        let (input, output) = (routing.input().unwrap(), routing.output().unwrap());
        let devices = p.devices(Some(track)).unwrap();
        let (eq, utility) = (
            node(&routing, &NodeKind::Device(devices[0].id)),
            node(&routing, &NodeKind::Device(devices[1].id)),
        );
        routing.cables = vec![
            cable(input, 0, utility, 0),
            cable(utility, 0, eq, 0),
            cable(eq, 0, output, 0),
            cable(input, 0, output, 0),
        ];
        let (graph, _) = compile(&p, track, &routing, &[0, 0]);
        let slots: Vec<_> = graph
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Effect { slot, .. } => Some(*slot),
                _ => None,
            })
            .collect();
        assert_eq!(slots, [1, 0]);
    }

    #[test]
    fn a_parallel_branch_is_delayed_to_meet_a_late_one() {
        let (p, track, mut routing) = chain(&["canopy.limiter"]);
        let (input, output) = (routing.input().unwrap(), routing.output().unwrap());
        routing.cables.push(cable(input, 0, output, 0));
        let (graph, latency) = compile(&p, track, &routing, &[100]);
        assert_eq!(latency, 100);
        assert_eq!(graph.lags.len(), 1, "only the dry cable waits");
        assert_eq!(graph.lags[0].delay, 100);
        assert_eq!(graph.delays[0].key, cable(input, 0, output, 0).key());
        // The dry path copies the input once; the limiter takes the track's
        // block over, and the output stays in it.
        assert_eq!(graph.buffers.len(), 1);
        assert!(!graph.steps.iter().any(|s| matches!(s, Step::Copy { .. })));
    }

    #[test]
    fn nodes_off_the_path_get_no_steps() {
        let (p, track, mut routing) = chain(&["canopy.eq", "canopy.utility", "canopy.reverb"]);
        let (input, output) = (routing.input().unwrap(), routing.output().unwrap());
        let devices = p.devices(Some(track)).unwrap();
        let [eq, utility, reverb] =
            [0, 1, 2].map(|i| node(&routing, &NodeKind::Device(devices[i].id)));
        // The utility hangs off the input and goes nowhere; the reverb
        // plays into the output but nothing feeds it.
        routing.cables = vec![
            cable(input, 0, eq, 0),
            cable(eq, 0, output, 0),
            cable(input, 0, utility, 0),
            cable(reverb, 0, output, 0),
        ];
        let (graph, _) = compile(&p, track, &routing, &[0, 0, 0]);
        let slots: Vec<_> = graph
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Effect { slot, .. } => Some(*slot),
                _ => None,
            })
            .collect();
        assert_eq!(slots, [0]);
        assert_eq!(graph.gains.len(), 2, "only the cables that play");
        let _ = (utility, reverb);
    }

    #[test]
    fn a_graph_that_does_not_hold_together_plays_as_a_line() {
        let (p, track, mut routing) = chain(&["canopy.eq"]);
        routing.nodes.push(GraphNode {
            id: NodeId(9999),
            kind: NodeKind::Output,
            pos: [0.0, 0.0],
        });
        let (program, latency) = GraphState::default().compile(ChainInput {
            owner: Some(track),
            devices: p.devices(Some(track)).unwrap(),
            routing: Some(&routing),
            latencies: &[7],
            sample_rate: 48_000,
        });
        assert!(matches!(program, ChainProgram::Linear));
        assert_eq!(latency, 7);
    }

    #[test]
    fn kept_state_is_remembered_by_chain() {
        let (p, track, mut routing) = chain(&["canopy.eq"]);
        let (input, output) = (routing.input().unwrap(), routing.output().unwrap());
        routing.cables.push(cable(input, 0, output, 0));
        let mut state = GraphState::default();
        let devices = p.devices(Some(track)).unwrap();
        let compile = |state: &mut GraphState, owner| {
            state.begin();
            let (program, _) = state.compile(ChainInput {
                owner,
                devices,
                routing: Some(&routing),
                latencies: &[10],
                sample_rate: 48_000,
            });
            state.finish();
            let ChainProgram::Graph(graph) = program else {
                panic!()
            };
            graph
        };
        let first = compile(&mut state, Some(track));
        assert!(first.gains.iter().all(|g| g.value.is_some()));
        let second = compile(&mut state, Some(track));
        assert!(second.gains.iter().all(|g| g.value.is_none()), "kept");
        assert!(second.delays.iter().all(|g| g.value.is_none()), "kept");
        // The same graph on another chain has its own state.
        let other = compile(&mut state, None);
        assert!(other.gains.iter().all(|g| g.value.is_some()));
        state.invalidate();
        let fresh = compile(&mut state, None);
        assert!(fresh.gains.iter().all(|g| g.value.is_some()));
    }
}
