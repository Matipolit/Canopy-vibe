//! The compiled, read-only snapshot of a project that the audio thread plays.
//!
//! The UI edits a [`Project`]; after each edit a [`Compiler`] turns it into an
//! [`Arrangement`] with every position already in samples and sorted, and
//! sends it over. The audio thread swaps it in and never sees the project.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use effects::{Effect, MAX_METERS};
use instruments::{DataKind, Instrument, InstrumentData, InstrumentRef, ParamDescriptor};
use media::{AudioBuffer, ClipProcess};
use midi::{NoteEvent, NoteEventKind, Tick};
use project::{
    AudioClip, AutomationTarget, ChannelMode, ClipId, ClipRef, Device, DeviceId, MidiClip,
    ModulatorId, Project, SourceId, StretchMode, Track, TrackId, TrackKind,
};

use crate::MAX_BLOCK;
use crate::audio::render_regions;
use crate::chain::{self, EffectSlot, LiveDevice, Meters, TapFrame};
use crate::compensation::{Compensation, MAX_DELAY};
use crate::fader::{Fader, FaderValues};
use crate::graph::{ChainInput, ChainProgram, GraphState};
use crate::modulation::{self, ModInput, ModProgram, ModState};
use crate::pool::Schedule;
use crate::shared::TrackMeters;

/// Fade at each end of an audio clip, like Ableton's default clip fades, so
/// clips that start or end mid-waveform don't click.
const CLIP_FADE_MS: f64 = 4.0;

/// The most note events one track can receive in one block. Collected into a
/// buffer allocated up front; anything past this in a single block is dropped.
pub(crate) const MAX_EVENTS_PER_BLOCK: usize = 512;

/// Notes the piano roll can audition at once on one track.
const MAX_PREVIEW_EVENTS: usize = 64;

pub struct Arrangement {
    /// Children before their group. Each renders into its own `out`; a
    /// group sums its `inputs`, and the master sums `master_inputs`.
    pub(crate) nodes: Vec<Node>,
    /// The top-level nodes, which play into the master.
    pub(crate) master_inputs: Box<[u32]>,
    /// The mix: the master's inputs summed, then its effects and fader.
    pub(crate) master_bus: Bus,
    /// Which nodes wait for which when a block is rendered on several
    /// threads (see `pool.rs`).
    pub(crate) schedule: Schedule,
    /// Sidechain sources: each holds its track's latest block, after its
    /// effects and before its fader, for the effects listening to it. A
    /// listener waits for its source where that doesn't close a loop, and
    /// otherwise reads the block before (`EffectSlot::late`).
    pub(crate) sends: Box<[Send]>,
    /// Which of each send's two buffers this block writes; the other holds
    /// the block before. Flipped every block.
    pub(crate) parity: usize,
    pub(crate) master: FaderValues,
    /// The master's effects, before its fader.
    pub(crate) master_effects: Box<[EffectSlot]>,
    /// How the master's effects run.
    pub(crate) master_program: ChainProgram,
    pub(crate) meters: Arc<TrackMeters>,
    /// For the metronome: samples per beat, and beats per bar.
    pub(crate) beat_len: f64,
    pub(crate) beats_per_bar: u32,
    /// For tempo-synced effects.
    pub(crate) bpm: f64,
    /// Where playback goes round, `start..end` in samples, when looping.
    pub(crate) loop_range: Option<(u64, u64)>,
}

impl fmt::Debug for Arrangement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arrangement")
            .field("tracks", &self.nodes.len())
            .finish()
    }
}

/// One track in the flattened graph.
pub(crate) struct Node {
    pub id: TrackId,
    /// The bus this track's output is added into: 0 is the master, each
    /// group has its own.
    pub output: usize,
    /// For a group, the nodes that play into its bus: what it sums.
    pub inputs: Box<[u32]>,
    /// This block of the track's sound, after its fader and compensation:
    /// what its group (or the master) sums.
    pub out: Bus,
    pub source: NodeSource,
    pub fader: FaderValues,
    /// Instrument parameter values, by index. Empty for non-MIDI tracks.
    pub params: Box<[f32]>,
    /// Effects, after the source and before the fader: one slot per
    /// device in `Track::devices` order, whatever the routing, so slot
    /// indices are automation's.
    pub effects: Box<[EffectSlot]>,
    /// How the effects run: in order, or as the track's graph says.
    pub program: ChainProgram,
    /// The track's modulation; empty if it has none.
    pub mods: ModProgram,
    /// The send bus this track's sound is copied to, if an effect listens
    /// to it.
    pub send: Option<usize>,
    /// The instrument's meters, for the UI.
    pub meters: Arc<Meters>,
    /// How many samples late this track's own effects make its sound.
    pub latency: usize,
    /// How much later to play it, so it arrives at its bus with the slowest
    /// of the tracks there (see `compensation.rs`).
    pub compensation: usize,
    pub automation: Box<[Automation]>,
    /// `None` means "keep the processors this track already has": the engine
    /// moves them over from the previous snapshot when this one arrives.
    pub processors: Option<Box<Processors>>,
}

pub(crate) enum NodeSource {
    /// Sorted by sample, Note Offs before Note Ons at the same sample.
    Midi(Box<[ScheduledEvent]>),
    /// Sorted by start; they don't overlap unless the project has overlaps.
    Audio(Box<[Region]>),
    /// A group: the sum of its children (`Node::inputs`), who play into
    /// this bus.
    Bus(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScheduledEvent {
    pub sample: u64,
    pub kind: NoteEventKind,
}

/// A stretch of an audio buffer placed on the timeline, all in session samples.
pub(crate) struct Region {
    pub start: u64,
    pub len: u64,
    /// First frame of `audio` that plays.
    pub offset: u64,
    pub audio: Arc<AudioBuffer>,
    pub gain: f32,
    /// Fade lengths in samples (at least the click guard) and their curves.
    pub fade_in: u64,
    pub fade_out: u64,
    pub fade_in_curve: f32,
    pub fade_out_curve: f32,
    pub channels: ChannelMode,
    /// The latest end of this region and every one before it. Unlike the
    /// ends themselves this never decreases, so it can be binary searched
    /// even when regions overlap.
    pub reach: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AutomationKind {
    Volume,
    Pan,
    Param(usize, ParamDescriptor),
    /// Effect `.0` in the track's chain, its parameter `.1`.
    Effect(usize, usize, ParamDescriptor),
    /// Modulator `.0` in the track's modulation program, its parameter
    /// `.1` (see `modulation::automation_kind`).
    #[allow(dead_code)] // compiled once there are modulators
    Modulator(usize, usize, ParamDescriptor),
}

pub(crate) struct Automation {
    pub kind: AutomationKind,
    /// `(sample, normalized value, curve to the next point)`, sorted.
    pub points: Box<[(u64, f32, f32)]>,
    /// The value applied last block, so unchanged values aren't re-sent.
    pub last: f32,
    /// The first point after the position asked for last, where the next
    /// block's usually is too.
    pub cursor: usize,
}

impl Automation {
    pub fn value_at(&self, sample: u64) -> Option<f32> {
        let after = self.points.partition_point(|p| p.0 <= sample);
        self.value_between(sample, after)
    }

    /// [`Automation::value_at`], starting from where the last call left
    /// off: playback asks for one position after another, so the point
    /// after it is nearly always the same or the next one, and a search is
    /// only needed after a jump.
    pub fn value_following(&mut self, sample: u64) -> Option<f32> {
        let points = &self.points;
        let fits = |after: usize| {
            (after == 0 || points[after - 1].0 <= sample)
                && points.get(after).is_none_or(|p| p.0 > sample)
        };
        let after = if fits(self.cursor) {
            self.cursor
        } else if self.cursor < points.len() && fits(self.cursor + 1) {
            self.cursor + 1
        } else {
            points.partition_point(|p| p.0 <= sample)
        };
        self.cursor = after;
        self.value_between(sample, after)
    }

    /// The value at `sample`, given that `after` is the first point past it.
    fn value_between(&self, sample: u64, after: usize) -> Option<f32> {
        let before = after.checked_sub(1).map(|i| self.points[i]);
        match (before, self.points.get(after)) {
            (None, None) => None,
            (Some(p), None) | (None, Some(&p)) => Some(p.1),
            (Some(a), Some(&b)) => {
                let t = (sample - a.0) as f64 / (b.0 - a.0) as f64;
                Some(project::segment_value(a.1, b.1, a.2, t as f32))
            }
        }
    }
}

/// State that must survive snapshot swaps: an instrument's ringing voices, a
/// fader mid-ramp. Built on the UI thread for new tracks and moved, as a
/// pointer, from one snapshot to the next for existing ones.
pub(crate) struct Processors {
    pub instrument: Option<Box<dyn Instrument>>,
    pub fader: Fader,
    /// Set when the track must release every note before its next block:
    /// after a seek, a stop, or an edit to its notes.
    pub all_notes_off: bool,
    /// Auditioned notes waiting for the next block. Never grows past the
    /// capacity it was built with.
    pub preview: Vec<NoteEventKind>,
    /// What each instrument parameter is actually set to, which automation
    /// and live knob moves change without a new snapshot.
    pub params: Box<[f32]>,
    /// Delays the track to line it up with slower ones.
    pub compensation: Compensation,
    /// This block's note events, allocated once and never grown.
    pub events: Vec<NoteEvent>,
}

impl Processors {
    pub fn push_preview(&mut self, event: NoteEventKind) {
        if self.preview.len() < self.preview.capacity() {
            self.preview.push(event);
        }
    }

    /// Sets an instrument parameter and remembers it.
    pub fn set_param(&mut self, index: usize, value: f32) {
        if let Some(instrument) = self.instrument.as_deref_mut() {
            instrument.set_param(index, value);
        }
        if let Some(slot) = self.params.get_mut(index) {
            *slot = value;
        }
    }
}

impl Node {
    /// True if this snapshot automates `kind` (ignoring a parameter's
    /// descriptor), so its static value shouldn't be pushed on a swap.
    pub fn automates(&self, kind: AutomationKind) -> bool {
        self.automation.iter().any(|a| match (a.kind, kind) {
            (AutomationKind::Param(a, _), AutomationKind::Param(b, _)) => a == b,
            (AutomationKind::Effect(a, i, _), AutomationKind::Effect(b, j, _)) => (a, i) == (b, j),
            (AutomationKind::Modulator(a, i, _), AutomationKind::Modulator(b, j, _)) => {
                (a, i) == (b, j)
            }
            (a, b) => a == b,
        })
    }
}

/// One block of stereo audio, sized so a bus never needs reallocating.
#[derive(Clone)]
pub(crate) struct Bus {
    pub left: [f32; MAX_BLOCK],
    pub right: [f32; MAX_BLOCK],
}

impl Default for Bus {
    fn default() -> Self {
        Bus {
            left: [0.0; MAX_BLOCK],
            right: [0.0; MAX_BLOCK],
        }
    }
}

impl Bus {
    /// Zeroes the first `frames`.
    pub fn clear(&mut self, frames: usize) {
        self.left[..frames].fill(0.0);
        self.right[..frames].fill(0.0);
    }

    /// Adds the first `frames` of `other`.
    pub fn add(&mut self, other: &Bus, frames: usize) {
        for (o, s) in self.left[..frames].iter_mut().zip(&other.left[..frames]) {
            *o += s;
        }
        for (o, s) in self.right[..frames].iter_mut().zip(&other.right[..frames]) {
            *o += s;
        }
    }
}

/// A sidechain send: the source track's block, in two buffers, one being
/// written this block while the other still holds the block before, for a
/// listener that can't wait for the source.
pub(crate) struct Send {
    buffers: [UnsafeCell<Bus>; 2],
}

// One buffer is written by one node per block and read only by nodes that
// waited for it; the other is only read. See `pool.rs`.
unsafe impl Sync for Send {}

impl Send {
    pub fn new() -> Send {
        Send {
            buffers: [
                UnsafeCell::new(Bus::default()),
                UnsafeCell::new(Bus::default()),
            ],
        }
    }

    /// Buffer `parity`, to read. Sound while no node writes it, which the
    /// schedule arranges.
    pub fn read(&self, parity: usize) -> &Bus {
        // SAFETY: as above.
        unsafe { &*self.buffers[parity & 1].get() }
    }

    /// Buffer `parity`, to write through. Only the node that owns the
    /// send may, once per block, and only readers that waited for that
    /// node read the same buffer.
    pub fn write(&self, parity: usize) -> *mut Bus {
        self.buffers[parity & 1].get()
    }
}

impl Arrangement {
    /// Plays nothing. What the engine starts with.
    pub fn empty() -> Box<Arrangement> {
        Box::new(Arrangement {
            nodes: Vec::new(),
            master_inputs: Box::new([]),
            master_bus: Bus::default(),
            schedule: Schedule::new(&[]),
            sends: Box::new([]),
            parity: 0,
            master: FaderValues::UNITY,
            master_effects: Box::new([]),
            master_program: ChainProgram::Linear,
            meters: Arc::new(TrackMeters::new(Vec::new())),
            beat_len: 0.0,
            beats_per_bar: 4,
            bpm: 120.0,
            loop_range: None,
        })
    }

    /// Where each track's level is published while this snapshot plays.
    pub fn meters(&self) -> &Arc<TrackMeters> {
        &self.meters
    }

    pub(crate) fn node_mut(&mut self, id: TrackId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// On the audio thread, once the snapshot replacing this one has taken
    /// what it keeps: retires the instruments, effects and modulators left
    /// here, which are done, before this goes back to be dropped. Called
    /// once per snapshot.
    pub(crate) fn retire(&mut self) {
        for node in &mut self.nodes {
            if let Some(instrument) = node
                .processors
                .as_deref_mut()
                .and_then(|p| p.instrument.as_deref_mut())
            {
                instrument.retire();
            }
            chain::retire(&mut node.effects);
            node.mods.retire();
        }
        chain::retire(&mut self.master_effects);
    }
}

/// What kind of processors the engine holds for a track. If this changes,
/// e.g. a different instrument, the track gets fresh ones.
#[derive(Debug, Clone, PartialEq)]
enum ProcessorKind {
    Audio,
    Group,
    /// The instrument, which data it was given (by address), so new data
    /// gets a new instance, and for a plugin its generation
    /// ([`AudioLookup::plugin_instrument_generation`]), so a restarted
    /// plugin does too.
    Midi(InstrumentRef, Option<usize>, u64),
}

/// What the compiler remembers about each track the engine has processors
/// for.
struct LiveTrack {
    kind: ProcessorKind,
    /// The instrument's meters, the same in every snapshot.
    meters: Arc<Meters>,
}

/// Builds [`Arrangement`]s on the UI thread, remembering which tracks the
/// engine already has processors for.
pub struct Compiler {
    sample_rate: u32,
    live: HashMap<TrackId, LiveTrack>,
    /// Effects the engine has an instance of.
    devices: HashMap<DeviceId, LiveDevice>,
    /// Graph state the engine holds.
    graph: GraphState,
    /// Modulators the engine runs, and their readings.
    mods: ModState,
    latency: usize,
}

impl Compiler {
    pub fn new(sample_rate: u32) -> Compiler {
        Compiler {
            sample_rate,
            live: HashMap::new(),
            devices: HashMap::new(),
            graph: GraphState::default(),
            mods: ModState::default(),
            latency: 0,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// How late the whole mix plays, in samples, because of effects that
    /// delay their sound (as of the last snapshot compiled).
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Forget what the engine holds, so the next snapshot carries fresh
    /// processors for every track. Call this if a snapshot couldn't be sent.
    pub fn invalidate(&mut self) {
        self.live.clear();
        self.devices.clear();
        self.graph.invalidate();
        self.mods.invalidate();
    }

    /// Where `target` on `track` is right now, modulation and all, on the
    /// `0..=1` scale automation uses, as the audio thread last published
    /// it: for a knob's live dot. `None` if it isn't modulated (the master,
    /// `None`, has no modulation).
    pub fn modulated_value(
        &self,
        track: Option<TrackId>,
        target: &AutomationTarget,
    ) -> Option<f32> {
        self.mods.modulated_value(track?, target)
    }

    /// A modulator's latest output and, if it has one, its phase (`0..1`).
    pub fn modulator_output(&self, id: ModulatorId) -> Option<(f32, f32)> {
        self.mods.modulator_output(id)
    }

    /// An effect's meters (a compressor's gain reduction and levels), as the
    /// audio thread last published them; what each slot means is in its
    /// descriptor.
    pub fn device_meters(&self, id: DeviceId) -> [f32; MAX_METERS] {
        self.devices
            .get(&id)
            .map_or([0.0; MAX_METERS], |d| d.meters.read())
    }

    /// A track's instrument meters, as the audio thread last published them.
    pub fn instrument_meters(&self, track: TrackId) -> [f32; MAX_METERS] {
        self.live
            .get(&track)
            .map_or([0.0; MAX_METERS], |t| t.meters.read())
    }

    /// Hands every frame an effect has tapped since the last call to
    /// `sink`, oldest first (see [`TapFrame`]). False if the effect has no
    /// tap.
    pub fn drain_tap(&mut self, id: DeviceId, mut sink: impl FnMut(TapFrame)) -> bool {
        let Some(device) = self.devices.get_mut(&id) else {
            return false;
        };
        // What a tap that had stopped holds is from when it stopped.
        let stale = device.meters.watch();
        let Some(tap) = device.tap.as_mut() else {
            return false;
        };
        if let Ok(chunk) = tap.read_chunk(tap.slots()) {
            if stale {
                chunk.commit_all();
            } else {
                for frame in chunk {
                    sink(frame);
                }
            }
        }
        true
    }

    /// `audio` finds the decoded audio for sources and the rendered audio
    /// for clips that need it. Clips whose audio isn't ready yet, or is at
    /// another sample rate, are silent.
    pub fn compile(&mut self, project: &Project, audio: impl AudioLookup) -> Box<Arrangement> {
        let sends = sidechain_sources(project);
        self.graph.begin();
        self.mods.begin();
        let mut build = Build {
            project,
            sample_rate: self.sample_rate,
            audio: &audio,
            any_solo: project.any_solo(),
            nodes: Vec::new(),
            bus_count: 1,
            live: &mut self.live,
            seen: Vec::new(),
            devices: &mut self.devices,
            seen_devices: Vec::new(),
            sends: &sends,
            graph: &mut self.graph,
            mods: &mut self.mods,
        };
        build.tracks(&project.tracks, 0, false);
        let master_effects = chain::build_chain(
            &project.master_devices,
            build.devices,
            &mut build.seen_devices,
            &sends,
            self.sample_rate,
            &audio,
        );
        let latencies = device_latencies(&project.master_devices, self.sample_rate, &audio);
        let (master_program, master_latency) = build.graph.compile(ChainInput {
            owner: None,
            devices: &project.master_devices,
            routing: project.master_routing.as_ref(),
            latencies: &latencies,
            sample_rate: self.sample_rate,
        });
        let seen = std::mem::take(&mut build.seen);
        let seen_devices = std::mem::take(&mut build.seen_devices);
        let (mut nodes, bus_count) = (build.nodes, build.bus_count);
        self.live.retain(|id, _| seen.contains(id));
        self.devices.retain(|id, _| seen_devices.contains(id));
        self.graph.finish();
        self.mods.finish();
        self.latency = compensate(&mut nodes, bus_count) + master_latency;
        let (schedule, master_inputs) = schedule(&mut nodes, bus_count);

        let meters = TrackMeters::new(nodes.iter().map(|n| n.id).collect());
        // Many beats at once, for a precise length.
        let tempo = &project.tempo;
        let thousand_beats = tempo.beat_len() * 1000;
        let beat_len = tempo.tick_to_sample(thousand_beats, self.sample_rate) as f64 / 1000.0;
        let loop_range = project.loop_region.filter(|r| r.on).and_then(|r| {
            let start = u64::try_from(tempo.tick_to_sample(r.start, self.sample_rate)).ok()?;
            let end = u64::try_from(tempo.tick_to_sample(r.end, self.sample_rate)).ok()?;
            (end > start).then_some((start, end))
        });
        Box::new(Arrangement {
            nodes,
            master_inputs,
            master_bus: Bus::default(),
            schedule,
            sends: (0..sends.len()).map(|_| Send::new()).collect(),
            parity: 0,
            master: FaderValues::from_mixer(&project.master, true),
            master_effects,
            master_program,
            meters: Arc::new(meters),
            beat_len,
            beats_per_bar: u32::from(tempo.time_signature.numerator.max(1)),
            bpm: tempo.tempo.to_bpm(),
            loop_range,
        })
    }
}

/// How late each of `devices` makes its sound, in samples: what its
/// descriptor says at its settings, or for a plugin what the plugin
/// reports; 0 when bypassed.
fn device_latencies<L: AudioLookup + ?Sized>(
    devices: &[Device],
    sample_rate: u32,
    lookup: &L,
) -> Vec<usize> {
    devices
        .iter()
        .map(|d| {
            if d.bypassed {
                0
            } else if !d.effect.format.is_builtin() {
                lookup.plugin_latency(d.id).unwrap_or(0)
            } else {
                d.effect
                    .descriptor()
                    .map_or(0, |e| (e.latency)(&d.values(), sample_rate as f32))
            }
        })
        .collect()
}

/// Works out how late each node's sound reaches the bus it plays into (its
/// group's own lateness plus its effects') and how much to delay it so it
/// meets the slowest there. Returns how late everything reaches the master.
/// `nodes` are in post-order, so a group's bus is settled before the group.
fn compensate(nodes: &mut [Node], bus_count: usize) -> usize {
    let mut bus_latency = vec![0usize; bus_count];
    let mut arrival = Vec::with_capacity(nodes.len());
    for node in nodes.iter() {
        let input = match node.source {
            NodeSource::Bus(bus) => bus_latency.get(bus).copied().unwrap_or(0),
            _ => 0,
        };
        let at = input + node.latency;
        arrival.push(at);
        bus_latency[node.output] = bus_latency[node.output].max(at);
    }
    for (node, at) in nodes.iter_mut().zip(arrival) {
        node.compensation = (bus_latency[node.output] - at).min(MAX_DELAY);
        if let Some(processors) = node.processors.as_deref_mut() {
            processors.compensation.jump(node.compensation);
        }
    }
    bus_latency[0]
}

/// Works out which nodes wait for which when a block renders on several
/// threads: a group for its inputs (the nodes playing into its bus, which
/// it sums), and a listener for the track its effect's sidechain reads,
/// unless that would close a loop (a track listening to its own group, two
/// tracks listening to each other), in which case the slot reads the block
/// before (`EffectSlot::late`), as every listener used to. Fills each
/// group's `inputs`; returns the schedule and the master's inputs.
fn schedule(nodes: &mut [Node], bus_count: usize) -> (Schedule, Box<[u32]>) {
    // Which node owns each bus (none for the master), and which node
    // writes each send.
    let mut bus_owner = vec![None; bus_count];
    let mut send_owner = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        if let NodeSource::Bus(bus) = node.source {
            bus_owner[bus] = Some(i);
        }
        if let Some(send) = node.send {
            send_owner.insert(send, i);
        }
    }
    let mut inputs: Vec<Vec<u32>> = vec![Vec::new(); nodes.len()];
    let mut master_inputs = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        match bus_owner.get(node.output).copied().flatten() {
            Some(owner) => inputs[owner].push(i as u32),
            None => master_inputs.push(i as u32),
        }
    }
    let mut deps = inputs.clone();
    // Whether `from` can reach `to` along the edges so far, so adding
    // `to → from` would close a loop.
    fn reaches(deps: &[Vec<u32>], from: usize, to: usize, seen: &mut Vec<bool>) -> bool {
        if from == to {
            return true;
        }
        if std::mem::replace(&mut seen[from], true) {
            return false;
        }
        deps[from]
            .iter()
            .any(|&d| reaches(deps, d as usize, to, seen))
    }
    for i in 0..nodes.len() {
        for slot in 0..nodes[i].effects.len() {
            let Some(send) = nodes[i].effects[slot].sidechain else {
                continue;
            };
            let Some(&source) = send_owner.get(&send) else {
                continue;
            };
            let mut seen = vec![false; nodes.len()];
            let late = source == i || reaches(&deps, source, i, &mut seen);
            nodes[i].effects[slot].late = late;
            if !late && !deps[i].contains(&(source as u32)) {
                deps[i].push(source as u32);
            }
        }
    }
    for (node, inputs) in nodes.iter_mut().zip(inputs) {
        node.inputs = inputs.into_boxed_slice();
    }
    (Schedule::new(&deps), master_inputs.into_boxed_slice())
}

/// The tracks some effect listens to, each with the send bus that carries
/// its sound.
fn sidechain_sources(project: &Project) -> HashMap<TrackId, usize> {
    let mut sends = HashMap::new();
    let mut add = |devices: &[project::Device]| {
        for device in devices {
            let listens = device.effect.descriptor().is_some_and(|d| d.sidechain);
            if let Some(track) = device.sidechain.filter(|_| listens) {
                let next = sends.len();
                sends.entry(track).or_insert(next);
            }
        }
    };
    add(&project.master_devices);
    project.visit(|track, _| add(&track.devices));
    sends
}

/// Where the compiler finds audio. A plain `Fn(SourceId)` will do when no
/// clip needs rendering.
pub trait AudioLookup {
    /// A source's decoded audio, at the session rate.
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>>;
    /// `clip`'s audio as [`clip_process`] renders it, for clips that don't
    /// play their file as is: frame 0 is the clip's start. A render of an
    /// earlier version of the clip will do until the new one is ready.
    fn rendered(&self, _clip: &AudioClip) -> Option<Arc<AudioBuffer>> {
        None
    }
    /// What a track's instrument plays from (a sampler's loaded bank, a
    /// Wavetable's user tables), for instruments that need data. A sampler
    /// is silent until it's there; a Wavetable plays its built-in tables.
    fn instrument_data(&self, _track: &Track) -> Option<InstrumentData> {
        None
    }

    // Plugins. The compiler runs on the UI thread, which is the plugins'
    // main thread; it calls these for effects and instruments whose format
    // isn't built-in (and whose description is registered), instead of
    // their descriptor's `create`.

    /// A running instance of plugin effect `device`, ready for the audio
    /// thread at `sample_rate` with blocks of at most `max_block` (the
    /// compiler doesn't `prepare` it). Asked only for devices the engine
    /// has no instance of, or whose generation changed. `None` while the
    /// plugin isn't ready (restarting): the slot is silent until a snapshot
    /// gets one. The engine calls [`Effect::retire`] on the audio thread
    /// when it's done with an instance, then sends it back to be dropped.
    fn plugin_effect(
        &self,
        _device: &Device,
        _sample_rate: u32,
        _max_block: usize,
    ) -> Option<Box<dyn Effect>> {
        None
    }

    /// Like [`Self::plugin_effect`], for a track's plugin instrument. `None`
    /// leaves the track silent until a snapshot gets one.
    fn plugin_instrument(
        &self,
        _track: &Track,
        _sample_rate: u32,
        _max_block: usize,
    ) -> Option<Box<dyn Instrument>> {
        None
    }

    /// How late plugin effect `device` makes its sound, in samples, as the
    /// plugin last reported; `None` for none.
    fn plugin_latency(&self, _device: DeviceId) -> Option<usize> {
        None
    }

    /// Like [`Self::plugin_latency`], for a track's plugin instrument.
    fn plugin_instrument_latency(&self, _track: TrackId) -> Option<usize> {
        None
    }

    /// Bumped whenever plugin effect `device` must get a fresh instance
    /// (it restarted, say for a new latency): the next snapshot asks for
    /// one, and the old instance is retired.
    fn plugin_generation(&self, _device: DeviceId) -> u64 {
        0
    }

    /// Like [`Self::plugin_generation`], for a track's plugin instrument.
    fn plugin_instrument_generation(&self, _track: TrackId) -> u64 {
        0
    }
}

impl<F: Fn(SourceId) -> Option<Arc<AudioBuffer>>> AudioLookup for F {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self(id)
    }
}

/// How `media::process_clip` must change `clip`'s source, decoded at
/// `sample_rate`, for the clip to play: reversed, looped, stretched. `None`
/// if it plays the file as is.
///
/// The result only changes when the clip's sound does: not when it moves,
/// nor (unless it follows the tempo) when the tempo changes.
pub fn clip_process(project: &Project, clip: &AudioClip, sample_rate: u32) -> Option<ClipProcess> {
    if clip.is_plain(&project.tempo) {
        return None;
    }
    let source = project.source(clip.source)?;
    let ratio = sample_rate as f64 / source.sample_rate.max(1) as f64;
    let frames = |n: u64| (n as f64 * ratio).round() as u64;
    let len = frames(clip.source_len).max(1);
    let play_len = frames(clip.played()).max(1);
    let speed = clip.stretch.effective_speed(&project.tempo);
    Some(ClipProcess {
        offset: frames(clip.source_offset),
        len,
        reversed: clip.reversed,
        looped: clip.looped,
        phase: frames(clip.loop_phase).min(len - 1),
        play_len,
        mode: match clip.stretch.mode {
            StretchMode::Resample => media::StretchMode::Resample,
            StretchMode::Stretch => media::StretchMode::Stretch,
            StretchMode::Beats => media::StretchMode::Beats,
        },
        speed,
        transpose: clip.stretch.transpose,
        out_len: (play_len as f64 / speed).ceil() as u64,
    })
}

/// A MIDI clip rendered on its own, for freezing it to audio.
pub struct RenderedClip {
    pub audio: AudioBuffer,
    /// Where the audio starts on the timeline: the clip's start.
    pub start: Tick,
}

/// After the last note, rendering stops once the instrument has been this
/// quiet (-80 dB) for this long, or after the longest tail allowed.
pub(crate) const TAIL_SILENCE: f32 = 1e-4;
pub(crate) const TAIL_SILENT_SECONDS: f64 = 0.25;
pub(crate) const MAX_TAIL_SECONDS: f64 = 10.0;

impl Compiler {
    /// Renders one MIDI clip through a fresh copy of its track's instrument,
    /// with the track's parameter settings and parameter automation, before
    /// the track's fader. Release tails ring out past the clip's end.
    /// `audio` provides the instrument's data, if it needs any. Runs on the
    /// calling thread; it's not real-time code.
    pub fn render_midi_clip(
        &self,
        project: &Project,
        clip: ClipId,
        audio: impl AudioLookup,
    ) -> Result<RenderedClip, String> {
        let rate = self.sample_rate;
        let (track_id, ClipRef::Midi(midi)) = project.clip(clip).ok_or("that clip is gone")? else {
            return Err("only MIDI clips can be frozen".into());
        };
        let track = project.track(track_id).ok_or("that track is gone")?;
        let TrackKind::Midi {
            instrument: reference,
            ..
        } = &track.kind
        else {
            return Err("only MIDI clips can be frozen".into());
        };
        let unavailable = || format!("{} isn't available", reference.id);
        let mut instrument = if reference.format.is_builtin() {
            let mut instrument = reference.create().ok_or_else(unavailable)?;
            instrument.prepare(rate as f32, MAX_BLOCK);
            if let Some(data) = audio.instrument_data(track) {
                instrument.set_data(data);
            }
            instrument
        } else {
            // A separate instance of the plugin, for rendering here.
            reference.descriptor().ok_or_else(unavailable)?;
            audio
                .plugin_instrument(track, rate, MAX_BLOCK)
                .ok_or_else(unavailable)?
        };
        let descriptors = reference.descriptor().map_or(&[][..], |d| d.params);
        let bpm = project.tempo.tempo.to_bpm();
        instrument.set_tempo(bpm);
        for (index, value) in track.instrument_values().into_iter().enumerate() {
            instrument.set_param(index, value);
        }

        let (mut live, mut devices) = (HashMap::new(), HashMap::new());
        let (mut graph, mut mods) = (GraphState::default(), ModState::default());
        let no_audio = |_: SourceId| None;
        let build = Build {
            project,
            sample_rate: rate,
            audio: &no_audio,
            any_solo: false,
            nodes: Vec::new(),
            bus_count: 1,
            live: &mut live,
            seen: Vec::new(),
            devices: &mut devices,
            seen_devices: Vec::new(),
            sends: &HashMap::new(),
            graph: &mut graph,
            mods: &mut mods,
        };
        // Frozen even if it's muted (as it is once frozen).
        let unmuted = MidiClip {
            muted: false,
            ..midi.clone()
        };
        let events = build.midi_events(std::slice::from_ref(&unmuted));
        let mut automation: Vec<Automation> = build
            .automation(track, descriptors)
            .into_vec()
            .into_iter()
            .filter(|a| matches!(a.kind, AutomationKind::Param(..)))
            .collect();

        let start = build.sample(midi.start).unwrap_or(0);
        let end = build.sample(midi.end()).unwrap_or(start).max(start);
        let limit = end + (MAX_TAIL_SECONDS * rate as f64) as u64;
        let quiet_needed = (TAIL_SILENT_SECONDS * rate as f64) as u64;
        let (mut left, mut right) = (Vec::new(), Vec::new());
        let (mut block_left, mut block_right) = ([0.0f32; MAX_BLOCK], [0.0f32; MAX_BLOCK]);
        let mut block_events = Vec::new();
        let (mut pos, mut next_event, mut quiet) = (start, 0, 0);
        while pos < limit {
            let block_end = pos + MAX_BLOCK as u64;
            block_events.clear();
            while let Some(event) = events.get(next_event).filter(|e| e.sample < block_end) {
                block_events.push(NoteEvent {
                    frame: event.sample.saturating_sub(pos) as u32,
                    kind: event.kind,
                });
                next_event += 1;
            }
            for lane in &mut automation {
                if let (Some(value), AutomationKind::Param(index, descriptor)) =
                    (lane.value_at(pos), lane.kind)
                {
                    instrument.set_param(index, descriptor.from_normalized(value));
                }
            }
            instrument.set_position(pos as f64 * bpm / (60.0 * f64::from(rate)), true);
            instrument.process(&block_events, &mut block_left, &mut block_right);
            left.extend_from_slice(&block_left);
            right.extend_from_slice(&block_right);
            pos = block_end;
            if pos >= end && next_event == events.len() {
                let peak = block_left
                    .iter()
                    .chain(&block_right)
                    .fold(0.0f32, |m, s| m.max(s.abs()));
                quiet = if peak < TAIL_SILENCE {
                    quiet + MAX_BLOCK as u64
                } else {
                    0
                };
                if quiet >= quiet_needed {
                    break;
                }
            }
        }
        // Rendered here, so it stops here.
        instrument.retire();
        // Keep the whole clip, but not the silence after its tail.
        let clip_len = (end - start) as usize;
        let last_sound = left
            .iter()
            .zip(&right)
            .rposition(|(l, r)| l.abs().max(r.abs()) >= TAIL_SILENCE)
            .map_or(0, |i| i + 1);
        let len = last_sound.max(clip_len).max(1).min(left.len().max(1));
        left.resize(len, 0.0);
        right.resize(len, 0.0);
        Ok(RenderedClip {
            audio: AudioBuffer::new(rate, left, right),
            start: midi.start,
        })
    }
}

impl Compiler {
    /// Mixes audio clips from one track into one buffer as they play (gain,
    /// fades, channels; not the track's fader), from the earliest start to
    /// the latest end, gaps and all: for gluing them into a single clip.
    pub fn render_audio_clips(
        &self,
        project: &Project,
        ids: &[ClipId],
        audio: impl AudioLookup,
    ) -> Result<RenderedClip, String> {
        let mut track = None;
        let mut clips = Vec::new();
        for id in ids {
            let (on, ClipRef::Audio(clip)) = project.clip(*id).ok_or("that clip is gone")? else {
                return Err("only audio clips can be consolidated".into());
            };
            if track.is_some_and(|t| t != on) {
                return Err("the clips must be on one track".into());
            }
            track = Some(on);
            clips.push(clip.clone());
        }
        let first = clips
            .iter()
            .map(|c| c.start)
            .min()
            .ok_or("nothing selected")?;
        let last = clips
            .iter()
            .map(|c| project.clip_end(ClipRef::Audio(c)))
            .max()
            .unwrap_or(first);
        let (mut live, mut devices) = (HashMap::new(), HashMap::new());
        let (mut graph, mut mods) = (GraphState::default(), ModState::default());
        let build = Build {
            project,
            sample_rate: self.sample_rate,
            audio: &audio,
            any_solo: false,
            nodes: Vec::new(),
            bus_count: 1,
            live: &mut live,
            seen: Vec::new(),
            devices: &mut devices,
            seen_devices: Vec::new(),
            sends: &HashMap::new(),
            graph: &mut graph,
            mods: &mut mods,
        };
        let regions = build.regions(&clips);
        // Nothing plays before the timeline's start.
        let first = first.max(Tick::ZERO);
        let start = build.sample(first).unwrap_or(0);
        let end = build.sample(last).unwrap_or(start).max(start + 1);
        let len = usize::try_from(end - start).map_err(|_| "too long")?;
        let (mut left, mut right) = (vec![0.0; len], vec![0.0; len]);
        let gate = [1.0f32; MAX_BLOCK];
        let blocks = left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK));
        for (i, (left, right)) in blocks.enumerate() {
            let at = start + (i * MAX_BLOCK) as u64;
            render_regions(&regions, at, &gate[..left.len()], left, right);
        }
        Ok(RenderedClip {
            audio: AudioBuffer::new(self.sample_rate, left, right),
            start: first,
        })
    }
}

struct Build<'a, F: ?Sized> {
    project: &'a Project,
    sample_rate: u32,
    audio: &'a F,
    any_solo: bool,
    nodes: Vec<Node>,
    bus_count: usize,
    live: &'a mut HashMap<TrackId, LiveTrack>,
    seen: Vec<TrackId>,
    devices: &'a mut HashMap<DeviceId, LiveDevice>,
    seen_devices: Vec<DeviceId>,
    sends: &'a HashMap<TrackId, usize>,
    graph: &'a mut GraphState,
    mods: &'a mut ModState,
}

impl<F: AudioLookup + ?Sized> Build<'_, F> {
    /// Flattens `tracks` in post-order, sending their output to `output`.
    fn tracks(&mut self, tracks: &[Track], output: usize, ancestor_soloed: bool) {
        for track in tracks {
            let soloed = ancestor_soloed || track.mixer.solo;
            // A soloed track is heard, and so is everything it plays through.
            let audible = !self.any_solo || soloed || contains_solo(track);
            let source = match &track.kind {
                TrackKind::Group { children, .. } => {
                    let bus = self.bus_count;
                    self.bus_count += 1;
                    self.tracks(children, bus, soloed);
                    NodeSource::Bus(bus)
                }
                TrackKind::Midi { clips, .. } => NodeSource::Midi(self.midi_events(clips)),
                TrackKind::Audio { clips } => NodeSource::Audio(self.regions(clips)),
            };
            let node = self.node(track, output, source, audible);
            self.nodes.push(node);
        }
    }

    fn node(&mut self, track: &Track, output: usize, source: NodeSource, audible: bool) -> Node {
        self.seen.push(track.id);
        let fader = FaderValues::from_mixer(&track.mixer, audible);
        let mut data = None;
        let mut plugin = None;
        let (kind, descriptor) = match &track.kind {
            TrackKind::Audio { .. } => (ProcessorKind::Audio, None),
            TrackKind::Group { .. } => (ProcessorKind::Group, None),
            TrackKind::Midi { instrument, .. } => {
                let descriptor = instrument.descriptor();
                let needs_data = descriptor.is_some_and(|d| d.data != DataKind::None);
                data = needs_data
                    .then(|| self.audio.instrument_data(track))
                    .flatten();
                // The address tells loads apart; the instance holds the Arc,
                // so it can't be reused while this kind is live.
                let key = data.as_ref().map(|d| Arc::as_ptr(d).cast::<()>() as usize);
                let generation = if instrument.format.is_builtin() {
                    0
                } else {
                    plugin = Some(descriptor.is_some());
                    self.audio.plugin_instrument_generation(track.id)
                };
                (
                    ProcessorKind::Midi(instrument.clone(), key, generation),
                    descriptor,
                )
            }
        };
        let descriptors = descriptor.map_or(&[][..], |d| d.params);
        let params: Box<[f32]> = track.instrument_values().into();

        let existing = self.live.get(&track.id).filter(|t| t.kind == kind);
        let meters = existing.map_or_else(Arc::default, |t| t.meters.clone());
        let processors = if existing.is_some() {
            None
        } else {
            let bpm = self.project.tempo.tempo.to_bpm();
            let instrument = match (&kind, plugin) {
                (_, Some(true)) => self
                    .audio
                    .plugin_instrument(track, self.sample_rate, MAX_BLOCK)
                    .map(|mut instrument| {
                        // Plugins come ready to run; they get their settings
                        // and the tempo, and reset on the audio thread.
                        instrument.set_tempo(bpm);
                        for (index, value) in params.iter().enumerate() {
                            instrument.set_param(index, *value);
                        }
                        instrument
                    }),
                (ProcessorKind::Midi(reference, ..), None) => {
                    reference.create().map(|mut instrument| {
                        instrument.prepare(self.sample_rate as f32, MAX_BLOCK);
                        instrument.set_tempo(bpm);
                        if let Some(data) = data {
                            instrument.set_data(data);
                        }
                        for (index, value) in params.iter().enumerate() {
                            instrument.set_param(index, *value);
                        }
                        // Starts at its settings rather than gliding there,
                        // and shows them before the audio thread first runs
                        // it.
                        instrument.reset();
                        instrument
                    })
                }
                _ => None,
            };
            if let Some(instrument) = &instrument {
                let mut idle = [0.0; MAX_METERS];
                instrument.meters(&mut idle);
                meters.store(&idle);
            }
            if plugin == Some(true) && instrument.is_none() {
                // A plugin that isn't ready: nothing remembered, so the
                // next snapshot asks again.
                self.live.remove(&track.id);
            } else {
                self.live.insert(
                    track.id,
                    LiveTrack {
                        kind,
                        meters: meters.clone(),
                    },
                );
            }
            Some(Box::new(Processors {
                instrument,
                fader: Fader::new(fader, self.sample_rate as f32),
                all_notes_off: false,
                preview: Vec::with_capacity(MAX_PREVIEW_EVENTS),
                params: params.clone(),
                compensation: Compensation::new(0),
                events: Vec::with_capacity(MAX_EVENTS_PER_BLOCK * 2),
            }))
        };

        let effects = chain::build_chain(
            &track.devices,
            self.devices,
            &mut self.seen_devices,
            self.sends,
            self.sample_rate,
            self.audio,
        );
        let latencies = device_latencies(&track.devices, self.sample_rate, self.audio);
        let (program, chain_latency) = self.graph.compile(ChainInput {
            owner: Some(track.id),
            devices: &track.devices,
            routing: track.routing.as_ref(),
            latencies: &latencies,
            sample_rate: self.sample_rate,
        });
        let instrument_latency = match plugin {
            Some(_) => self.audio.plugin_instrument_latency(track.id).unwrap_or(0),
            None => 0,
        };
        let mods = self.mods.compile(ModInput {
            track,
            instrument: descriptor,
            sample_rate: self.sample_rate,
        });
        Node {
            id: track.id,
            output,
            inputs: Box::new([]),
            out: Bus::default(),
            source,
            fader,
            automation: self.automation(track, descriptors),
            params,
            effects,
            program,
            mods,
            send: self.sends.get(&track.id).copied(),
            meters,
            latency: instrument_latency + chain_latency,
            compensation: 0,
            processors,
        }
    }

    fn sample(&self, tick: Tick) -> Option<u64> {
        u64::try_from(self.project.tempo.tick_to_sample(tick, self.sample_rate)).ok()
    }

    fn midi_events(&self, clips: &[project::MidiClip]) -> Box<[ScheduledEvent]> {
        let notes = clips.iter().map(|c| c.notes.len()).sum::<usize>();
        let mut events = Vec::with_capacity(2 * notes);
        for clip in clips.iter().filter(|c| !c.muted) {
            for note in clip.audible_notes() {
                let start = clip.start + (note.start - clip.offset);
                let end = (start + note.len).min(clip.end());
                let (Some(on), Some(off)) = (self.sample(start), self.sample(end)) else {
                    continue;
                };
                if off <= on {
                    continue;
                }
                events.push(ScheduledEvent {
                    sample: on,
                    kind: NoteEventKind::On {
                        pitch: note.pitch,
                        velocity: note.velocity,
                    },
                });
                events.push(ScheduledEvent {
                    sample: off,
                    kind: NoteEventKind::Off {
                        pitch: note.pitch,
                        velocity: note.release_velocity,
                    },
                });
                if !note.bend.is_empty() {
                    let (tempo, rate) = (&self.project.tempo, self.sample_rate);
                    let tick_at = |sample: u64| tempo.sample_to_tick(sample as i64, rate) - start;
                    push_bends(note, on, off, tick_at, &mut events);
                }
            }
        }
        // Events with equal keys are equal, so their order can't matter.
        events.sort_unstable_by_key(|e| (e.sample, e.kind));
        events.into_boxed_slice()
    }

    fn regions(&self, clips: &[AudioClip]) -> Box<[Region]> {
        let declick = (CLIP_FADE_MS / 1000.0 * self.sample_rate as f64) as u64;
        let tempo = &self.project.tempo;
        let mut regions = Vec::new();
        for clip in clips.iter().filter(|c| !c.muted) {
            let Some(source) = self.project.source(clip.source) else {
                continue;
            };
            // Clip offsets and fades count frames of material at the file's
            // own rate; the audio is at the session rate, and stretched.
            let ratio = self.sample_rate as f64 / source.sample_rate.max(1) as f64;
            let speed = clip.stretch.effective_speed(tempo);
            let frames = |n: u64| (n as f64 * ratio).round() as i64;
            let timeline = |n: u64| (n as f64 * ratio / speed).round() as i64;
            let mut start = tempo.tick_to_sample(clip.start, self.sample_rate);
            let (audio, mut offset, mut len) = if clip.is_plain(tempo) {
                let Some(audio) = self.audio.source(clip.source) else {
                    continue;
                };
                (audio, frames(clip.source_offset), frames(clip.source_len))
            } else {
                let Some(audio) = self.audio.rendered(clip) else {
                    continue;
                };
                let end = tempo.tick_to_sample(
                    self.project.clip_end(ClipRef::Audio(clip)),
                    self.sample_rate,
                );
                (audio, 0, end - start)
            };
            if audio.sample_rate() != self.sample_rate {
                continue;
            }
            let mut fade_in = timeline(clip.fade_in) as u64;
            if start < 0 {
                // Whatever lies before the timeline's start can't play,
                // including the fade-in.
                offset -= start;
                len += start;
                start = 0;
                fade_in = 0;
            }
            let len = len.min(audio.frames() as i64 - offset);
            if len <= 0 {
                continue;
            }
            let len = len as u64;
            // The click guard always applies; fades can't outgrow the clip.
            let guard = declick.min(len / 2);
            let mut fade_in = fade_in.max(guard);
            let mut fade_out = (timeline(clip.fade_out) as u64).max(guard);
            if fade_in + fade_out > len {
                let scale = len as f64 / (fade_in + fade_out) as f64;
                fade_in = (fade_in as f64 * scale) as u64;
                fade_out = len - fade_in;
            }
            let polarity = if clip.polarity_inverted { -1.0 } else { 1.0 };
            regions.push(Region {
                start: start as u64,
                len,
                offset: offset as u64,
                gain: dsp::db_to_gain(clip.gain_db) * polarity,
                channels: clip.channels,
                fade_in,
                fade_out,
                fade_in_curve: clip.fade_in_curve,
                fade_out_curve: clip.fade_out_curve,
                audio,
                reach: 0,
            });
        }
        regions.sort_by_key(|r| r.start);
        let mut reach = 0;
        for region in &mut regions {
            reach = reach.max(region.start + region.len);
            region.reach = reach;
        }
        regions.into_boxed_slice()
    }

    fn automation(&self, track: &Track, params: &[ParamDescriptor]) -> Box<[Automation]> {
        track
            .automation
            .iter()
            .filter_map(|lane| {
                let kind = match &lane.target {
                    AutomationTarget::Volume => AutomationKind::Volume,
                    AutomationTarget::Pan => AutomationKind::Pan,
                    AutomationTarget::InstrumentParam(id) => {
                        let index = params.iter().position(|p| p.id == id)?;
                        AutomationKind::Param(index, params[index])
                    }
                    AutomationTarget::DeviceParam { device, param } => {
                        let slot = track.devices.iter().position(|d| d.id == *device)?;
                        let params = track.devices[slot].effect.descriptor()?.params;
                        let index = params.iter().position(|p| p.id == param)?;
                        AutomationKind::Effect(slot, index, params[index])
                    }
                    AutomationTarget::ModulatorParam { modulator, param } => {
                        modulation::automation_kind(track, *modulator, param)?
                    }
                };
                let points = lane
                    .points
                    .iter()
                    .map(|p| {
                        (
                            self.sample(p.tick.max(Tick::ZERO)).unwrap_or(0),
                            p.value,
                            p.curve,
                        )
                    })
                    .collect();
                Some(Automation {
                    kind,
                    points,
                    last: f32::NAN,
                    cursor: 0,
                })
            })
            .filter(|a| !a.points.is_empty())
            .collect()
    }
}

fn contains_solo(track: &Track) -> bool {
    track
        .children()
        .iter()
        .any(|c| c.mixer.solo || contains_solo(c))
}

/// Appends `note`'s bend, the note sounding from sample `on` to `off`, as
/// [`NoteEventKind::NoteBend`]s: one every [`midi::BEND_STEP_FRAMES`] where
/// its value, to the cent, has changed (so at `on` only if it starts bent).
/// `tick_at` turns a timeline sample into ticks from the note's start;
/// points past the note's end are never reached.
fn push_bends(
    note: &project::Note,
    on: u64,
    off: u64,
    tick_at: impl Fn(u64) -> Tick,
    events: &mut Vec<ScheduledEvent>,
) {
    let mut sent = 0;
    let mut sample = on;
    while sample < off {
        // Within the note: samples and ticks round differently, so a note's
        // first sample can map to the tick before it.
        let at = tick_at(sample).clamp(Tick::ZERO, note.len);
        let cents = (note.bend_at(at) * 100.0).round() as i16;
        if cents != sent {
            events.push(ScheduledEvent {
                sample,
                kind: NoteEventKind::NoteBend {
                    pitch: note.pitch,
                    cents,
                },
            });
            sent = cents;
        }
        sample += u64::from(midi::BEND_STEP_FRAMES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A note of pitch 60 from sample `on` to `off`, bent along `points`
    /// (tick, semitones), at one tick per 10 samples.
    fn bends(on: u64, off: u64, len: i64, points: &[(i64, f32)]) -> Vec<(u64, i16)> {
        let note = project::Note {
            id: project::NoteId(1),
            start: Tick::ZERO,
            len: Tick::new(len),
            pitch: midi::Pitch::new(60).unwrap(),
            velocity: midi::Velocity::MAX,
            release_velocity: midi::Velocity::CENTER,
            muted: false,
            bend: points
                .iter()
                .map(|&(at, semitones)| project::BendPoint {
                    at: Tick::new(at),
                    semitones,
                    curve: 0.0,
                })
                .collect(),
        };
        let mut events = Vec::new();
        push_bends(
            &note,
            on,
            off,
            |sample| Tick::new(((sample - on) / 10) as i64),
            &mut events,
        );
        events
            .iter()
            .map(|e| match e.kind {
                NoteEventKind::NoteBend { pitch, cents } => {
                    assert_eq!(pitch.get(), 60);
                    (e.sample, cents)
                }
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_bent_note_compiles_to_bend_events_inside_it() {
        // An octave up over the first 960 ticks (9600 samples), then held.
        let events = bends(1000, 20_000, 2000, &[(0, 0.0), (960, 12.0)]);
        assert!(events.iter().all(|&(s, _)| (1000..20_000).contains(&s)));
        assert!(
            events
                .windows(2)
                .all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1)
        );
        // One a block (64 frames) at most, and it ends at the octave.
        assert!(events.windows(2).all(|w| w[1].0 - w[0].0 >= 64));
        assert_eq!(events.last().unwrap().1, 1200);
        // Nothing at the note's start: it starts unbent.
        assert!(events[0].0 > 1000);
        // Starting bent, it says so at once.
        let late = bends(1000, 5000, 400, &[(0, -2.0)]);
        assert_eq!(late, [(1000, -200)]);
        assert!(bends(0, 5000, 400, &[]).is_empty());
    }

    #[test]
    fn a_note_that_starts_bent_says_so_at_its_note_on_whatever_the_rounding() {
        // At 140 BPM and 48 kHz beat 2's first sample rounds down to just
        // before the note's tick: its bend must still be sent at once.
        let tempo = project::TempoMap::new(140.0);
        let rate = 48_000;
        let start = Tick::new(960);
        let mut note = project::Note {
            id: project::NoteId(1),
            start,
            len: Tick::new(480),
            pitch: midi::Pitch::new(60).unwrap(),
            velocity: midi::Velocity::MAX,
            release_velocity: midi::Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        };
        note.bend = vec![project::BendPoint {
            at: Tick::ZERO,
            semitones: -2.0,
            curve: 0.0,
        }];
        let on = tempo.tick_to_sample(start, rate) as u64;
        let off = tempo.tick_to_sample(start + note.len, rate) as u64;
        let mut events = Vec::new();
        let tick_at = |sample: u64| tempo.sample_to_tick(sample as i64, rate) - start;
        push_bends(&note, on, off, tick_at, &mut events);
        assert_eq!(events.first().map(|e| e.sample), Some(on));
        assert!(matches!(
            events[0].kind,
            NoteEventKind::NoteBend { cents: -200, .. }
        ));
    }

    #[test]
    fn bend_points_past_the_end_are_not_played() {
        // The note was shortened to 100 ticks; its curve went on to 960.
        let events = bends(0, 1000, 100, &[(0, 0.0), (960, 12.0)]);
        assert!(events.iter().all(|&(s, _)| s < 1000));
        let last = events.last().unwrap().1;
        assert!((115..=125).contains(&last), "{last}");
    }

    #[test]
    fn following_a_lane_reads_what_searching_it_does() {
        let lane = || Automation {
            kind: AutomationKind::Volume,
            points: vec![
                (100, 0.0, 0.0),
                (200, 1.0, 0.3),
                (200, 0.5, 0.0),
                (900, 0.25, -0.5),
            ]
            .into_boxed_slice(),
            last: f32::NAN,
            cursor: 0,
        };
        let mut followed = lane();
        let searched = lane();
        // Forwards a block at a time, then jumps both ways.
        let positions = (0..1100)
            .step_by(64)
            .chain([150, 50, 1000, 199, 200, 201, 0]);
        for sample in positions {
            assert_eq!(
                followed.value_following(sample).map(f32::to_bits),
                searched.value_at(sample).map(f32::to_bits),
                "at {sample}"
            );
        }
    }
}
