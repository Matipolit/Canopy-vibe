//! Effect graphs in the engine: a `Routing` played as a graph, with
//! parallel branches, cable levels, band, left/right and mid/side splits
//! and joins, latency lined up across branches, sidechains from cables,
//! and state that carries on through snapshot swaps. Every block is
//! played under `assert_no_alloc`.
//!
//! Fake plugin effects stand in where a built-in can't say exactly what
//! happened: a pure delay that reports its latency, one that plays its
//! sidechain, and one that counts its blocks.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use effects::{Category, Context, Effect, EffectDescriptor, Format, Pace};
use engine::{AudioLookup, Command, Compiler, Engine, EngineHandle};
use instruments::ParamDescriptor;
use media::AudioBuffer;
use midi::Tick;
use project::{
    Cable, CableKey, Device, DeviceId, EffectRef, GraphNode, Join, NewTrack, NodeId, NodeKind,
    PortRef, Project, Routing, Slope, SourceId, Split, TrackId, TrackPosition,
};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: u32 = 48_000;

/// What a fake plugin effect does.
enum Fake {
    /// Plays its input `n` samples late, and says so.
    Delay { ring: Vec<[f32; 2]>, at: usize },
    /// Plays its sidechain (silence without one) instead of its input.
    Echo,
    /// Leaves the sound alone and counts the blocks it's given.
    Count(Arc<AtomicUsize>),
}

impl Effect for Fake {
    fn prepare(&mut self, _: f32, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: &Context) {
        match self {
            Fake::Delay { ring, at } => {
                for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                    let [old_l, old_r] = std::mem::replace(&mut ring[*at], [*l, *r]);
                    (*l, *r) = (old_l, old_r);
                    *at = (*at + 1) % ring.len();
                }
            }
            Fake::Echo => match context.sidechain {
                Some((sl, sr)) => {
                    left.copy_from_slice(sl);
                    right.copy_from_slice(sr);
                }
                None => {
                    left.fill(0.0);
                    right.fill(0.0);
                }
            },
            Fake::Count(count) => {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    fn params(&self) -> &'static [ParamDescriptor] {
        &[]
    }
    fn receive(&mut self, _: usize, _: f32, _: Pace) {}
}

fn register() {
    for (id, sidechain) in [
        ("test.delay", false),
        ("test.echo", true),
        ("test.count", false),
    ] {
        effects::register_external(
            Format::Clap,
            id,
            EffectDescriptor {
                id,
                name: id,
                version: "1",
                category: Category::Routing,
                sidechain,
                create: || unreachable!("plugins come from the lookup"),
                ..EffectDescriptor::BASE
            },
        );
    }
}

/// The sources, and the fake plugins' side of the compiler.
#[derive(Default)]
struct Host {
    sources: HashMap<SourceId, Arc<AudioBuffer>>,
    /// Each fake delay's latency.
    delays: RefCell<HashMap<DeviceId, usize>>,
    /// Each counting fake's count.
    counts: RefCell<HashMap<DeviceId, Arc<AtomicUsize>>>,
    generation: Cell<u64>,
}

impl AudioLookup for &Host {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self.sources.get(&id).cloned()
    }

    fn plugin_effect(&self, device: &Device, _: u32, _: usize) -> Option<Box<dyn Effect>> {
        Some(Box::new(match device.effect.id.as_str() {
            "test.delay" => {
                let n = self.delays.borrow().get(&device.id).copied().unwrap_or(0);
                Fake::Delay {
                    ring: vec![[0.0; 2]; n.max(1)],
                    at: 0,
                }
            }
            "test.echo" => Fake::Echo,
            _ => {
                let count = Arc::new(AtomicUsize::new(0));
                self.counts.borrow_mut().insert(device.id, count.clone());
                Fake::Count(count)
            }
        }))
    }

    fn plugin_latency(&self, device: DeviceId) -> Option<usize> {
        Some(self.delays.borrow().get(&device).copied().unwrap_or(0))
    }

    fn plugin_generation(&self, _: DeviceId) -> u64 {
        self.generation.get()
    }
}

/// An audio track playing a stereo file from the start, an engine, and its
/// compiler.
struct Rig {
    handle: EngineHandle,
    engine: Engine,
    compiler: Compiler,
    project: Project,
    track: TrackId,
    host: Host,
}

impl Rig {
    fn new(left: Vec<f32>, right: Vec<f32>) -> Rig {
        register();
        let (handle, engine) = engine::create(RATE as f32);
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let source = project.add_source("/tmp/graph.wav".into(), RATE, left.len() as u64, 2);
        project.add_audio_clip(track, Tick::ZERO, source).unwrap();
        let mut host = Host::default();
        host.sources
            .insert(source, Arc::new(AudioBuffer::new(RATE, left, right)));
        Rig {
            handle,
            engine,
            compiler: Compiler::new(RATE),
            project,
            track,
            host,
        }
    }

    fn mono(samples: Vec<f32>) -> Rig {
        Rig::new(samples.clone(), samples)
    }

    /// Adds an effect to the track: a built-in, or a fake plugin
    /// (`test.…`).
    fn add(&mut self, effect: &str, params: &[(&str, f32)]) -> DeviceId {
        let reference = if effect.starts_with("test.") {
            EffectRef {
                format: Format::Clap,
                id: effect.into(),
                version: "1".into(),
            }
        } else {
            EffectRef::builtin(effect)
        };
        let id = self
            .project
            .add_device(Some(self.track), reference, None)
            .unwrap();
        for (param, value) in params {
            self.project.set_device_param(id, param, *value).unwrap();
        }
        id
    }

    /// A fake plugin that delays by `latency` samples.
    fn add_delay(&mut self, latency: usize) -> DeviceId {
        let id = self.add("test.delay", &[]);
        self.host.delays.borrow_mut().insert(id, latency);
        id
    }

    /// The track's graph (a straight line through its effects, the first
    /// time), with its cables gone if `unplugged`.
    fn graph(&mut self, unplugged: bool) -> &mut Routing {
        let graph = self.project.ensure_routing(Some(self.track)).unwrap();
        if unplugged {
            graph.cables.clear();
        }
        graph
    }

    fn input(&mut self) -> NodeId {
        self.graph(false).input().unwrap()
    }

    fn output(&mut self) -> NodeId {
        self.graph(false).output().unwrap()
    }

    fn node_of(&mut self, device: DeviceId) -> NodeId {
        self.graph(false).device_node(device).unwrap()
    }

    fn add_node(&mut self, kind: NodeKind) -> NodeId {
        let id = self.project.new_node_id();
        self.graph(false).nodes.push(GraphNode {
            id,
            kind,
            pos: [0.0, 0.0],
        });
        id
    }

    fn connect(&mut self, from: (NodeId, u8), to: (NodeId, u8), gain_db: f32) -> CableKey {
        let cable = Cable {
            from: PortRef {
                node: from.0,
                port: from.1,
            },
            to: PortRef {
                node: to.0,
                port: to.1,
            },
            gain_db,
            muted: false,
        };
        let key = cable.key();
        self.graph(false).cables.push(cable);
        key
    }

    /// Compiles the project and sends it to the engine, playing.
    fn load(&mut self) {
        let arrangement = self.compiler.compile(&self.project, &self.host);
        self.handle.send(Command::Arrangement(arrangement)).unwrap();
        self.handle.send(Command::SetPlaying(true)).unwrap();
    }

    /// The next `frames` frames, left and right, played without allocating.
    fn run(&mut self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut out = vec![0.0; frames * 2];
        let engine = &mut self.engine;
        assert_no_alloc::assert_no_alloc(|| {
            for chunk in out.chunks_mut(256 * 2) {
                engine.process_interleaved(chunk, 2);
            }
        });
        self.handle.collect_garbage();
        (
            out.chunks(2).map(|f| f[0]).collect(),
            out.chunks(2).map(|f| f[1]).collect(),
        )
    }

    fn render(mut self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        self.load();
        self.run(frames)
    }
}

/// Deterministic noise in -1..1.
fn noise(frames: usize, seed: u32) -> Vec<f32> {
    let mut state = seed.max(1);
    (0..frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32 * 2.0 - 1.0
        })
        .collect()
}

fn sine(frames: usize, hz: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 0.5)
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// How much louder `a` is than `b` over their second halves, in dB.
fn gain_db(a: &[f32], b: &[f32]) -> f32 {
    let half = a.len() / 2;
    20.0 * (rms(&a[half..]) / rms(&b[half..])).log10()
}

fn max_difference(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max)
}

const SECOND: usize = RATE as usize;

#[test]
fn a_straight_line_graph_sounds_exactly_like_the_chain() {
    let input = noise(SECOND, 7);
    let build = |graph: bool| {
        let mut rig = Rig::mono(input.clone());
        rig.add("canopy.eq", &[]);
        let compressor = rig.add("canopy.compressor", &[]);
        rig.add("canopy.reverb", &[]);
        rig.add("canopy.utility", &[("gain", -4.0)]);
        rig.add("canopy.limiter", &[]);
        rig.project.set_device_bypassed(compressor, true).unwrap();
        if graph {
            rig.graph(false);
        }
        assert_eq!(rig.project.routing(Some(rig.track)).is_some(), graph);
        rig.render(SECOND / 2)
    };
    let (line, graph) = (build(false), build(true));
    assert!(rms(&line.0) > 0.01, "it plays");
    assert!(line == graph, "bit for bit");
}

#[test]
fn parallel_branches_add_up() {
    let input = noise(SECOND / 4, 3);
    let dry = Rig::mono(input.clone()).render(input.len());
    let mut rig = Rig::mono(input.clone());
    let a = rig.add("canopy.utility", &[("gain", -6.0)]);
    let b = rig.add("canopy.utility", &[("gain", -6.0)]);
    rig.graph(true);
    let (i, o, a, b) = (rig.input(), rig.output(), rig.node_of(a), rig.node_of(b));
    rig.connect((i, 0), (a, 0), 0.0);
    rig.connect((i, 0), (b, 0), 0.0);
    rig.connect((a, 0), (o, 0), 0.0);
    rig.connect((b, 0), (o, 0), 0.0);
    let wet = rig.render(input.len());
    let expected = 20.0 * (2.0 * dsp::db_to_gain(-6.0)).log10();
    let measured = gain_db(&wet.0, &dry.0);
    assert!((measured - expected).abs() < 0.01, "{measured} dB");
}

#[test]
fn a_muted_cable_is_silent_and_a_quieter_one_quieter() {
    let input = noise(SECOND / 4, 5);
    let dry = Rig::mono(input.clone()).render(input.len());
    let mut rig = Rig::mono(input.clone());
    let utility = rig.add("canopy.utility", &[]);
    rig.graph(true);
    let (i, o, u) = (rig.input(), rig.output(), rig.node_of(utility));
    rig.connect((i, 0), (o, 0), -6.0);
    rig.connect((i, 0), (u, 0), 0.0);
    let muted = rig.connect((u, 0), (o, 0), 0.0);
    let graph = rig.graph(false);
    graph
        .cables
        .iter_mut()
        .find(|c| c.key() == muted)
        .unwrap()
        .muted = true;
    let wet = rig.render(input.len());
    let measured = gain_db(&wet.0, &dry.0);
    assert!((measured + 6.0).abs() < 0.01, "{measured} dB");
}

/// A DC input through a cable straight to the output, and a muted path
/// beside it so the chain is a graph, and a second utility with no cables.
/// Returns the rig, the cable and the second utility's node.
fn dc_rig() -> (Rig, CableKey, NodeId) {
    let mut rig = Rig::mono(vec![0.5; SECOND]);
    let utility = rig.add("canopy.utility", &[]);
    let spare = rig.add("canopy.utility", &[]);
    rig.graph(true);
    let spare = rig.node_of(spare);
    let (i, o, u) = (rig.input(), rig.output(), rig.node_of(utility));
    let direct = rig.connect((i, 0), (o, 0), 0.0);
    rig.connect((i, 0), (u, 0), 0.0);
    let muted = rig.connect((u, 0), (o, 0), 0.0);
    let graph = rig.graph(false);
    graph
        .cables
        .iter_mut()
        .find(|c| c.key() == muted)
        .unwrap()
        .muted = true;
    (rig, direct, spare)
}

/// The largest step between neighbouring samples.
fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn a_cable_level_change_glides_rather_than_jumps() {
    let (mut rig, direct, _) = dc_rig();
    rig.load();
    let (before, _) = rig.run(4096);
    let level = *before.last().unwrap();
    assert!(level > 0.1);
    rig.handle
        .send(Command::SetCableGain {
            owner: Some(rig.track),
            cable: direct,
            gain_db: -12.0,
        })
        .unwrap();
    let (after, _) = rig.run(4096);
    let target = level * dsp::db_to_gain(-12.0);
    assert!((after.last().unwrap() - target).abs() < 1e-5, "it arrives");
    let moving = after.iter().filter(|s| (**s - target).abs() > 1e-5).count();
    assert!(
        moving > 100,
        "over a few milliseconds, not at once: {moving}"
    );
    assert!(largest_step(&after) < 0.01, "{}", largest_step(&after));
}

#[test]
fn a_new_cable_fades_in() {
    let (mut rig, _, s) = dc_rig();
    rig.load();
    let (before, _) = rig.run(4096);
    let level = *before.last().unwrap();
    // A second path, through the spare utility.
    let (i, o) = (rig.input(), rig.output());
    rig.connect((i, 0), (s, 0), 0.0);
    rig.connect((s, 0), (o, 0), 0.0);
    rig.load();
    let (after, _) = rig.run(4096);
    assert!((after.last().unwrap() - 2.0 * level).abs() < 1e-4);
    assert!(largest_step(&after) < 0.01, "{}", largest_step(&after));
}

/// A track split at `crossovers` with every band played straight to the
/// output (or only `band`, if given), playing the first half of `input`
/// (well inside the clip, whose end the filters would smear).
fn banded(
    input: &[f32],
    crossovers: &[f32],
    slope: Slope,
    band: Option<u8>,
) -> (Vec<f32>, Vec<f32>) {
    let mut rig = Rig::mono(input.to_vec());
    rig.graph(true);
    let (i, o) = (rig.input(), rig.output());
    let split = rig.add_node(NodeKind::Split(Split::Bands {
        crossovers: crossovers.to_vec(),
        slope,
    }));
    rig.connect((i, 0), (split, 0), 0.0);
    for port in 0..=crossovers.len() as u8 {
        if band.is_none_or(|b| b == port) {
            rig.connect((split, port), (o, 0), 0.0);
        }
    }
    rig.render(input.len() / 2)
}

#[test]
fn band_splits_add_back_up_to_the_input() {
    for hz in [80.0, 700.0, 2500.0, 9000.0] {
        let input = sine(SECOND / 2, hz);
        let dry = Rig::mono(input.clone()).render(input.len() / 2);
        for (crossovers, slope) in [
            (&[1000.0][..], Slope::Db24),
            (&[300.0, 3000.0][..], Slope::Db24),
            (&[200.0, 1500.0, 6000.0][..], Slope::Db48),
            (&[500.0][..], Slope::Db12),
        ] {
            let (left, right) = banded(&input, crossovers, slope, None);
            for (wet, dry) in [(&left, &dry.0), (&right, &dry.1)] {
                let measured = gain_db(wet, dry);
                assert!(
                    measured.abs() < 0.05,
                    "{crossovers:?} {slope:?} at {hz} Hz: {measured} dB"
                );
            }
        }
    }
    // And each band is its own part of the sound.
    let high = sine(SECOND / 2, 9000.0);
    let dry = Rig::mono(high.clone()).render(high.len() / 2);
    let low_band = banded(&high, &[500.0], Slope::Db24, Some(0));
    assert!(gain_db(&low_band.0, &dry.0) < -40.0);
    let high_band = banded(&high, &[500.0], Slope::Db24, Some(1));
    assert!(gain_db(&high_band.0, &dry.0).abs() < 0.05);
}

/// A track with a `split` and its join, its parts crossed if `crossed`.
fn split_and_join(
    left: &[f32],
    right: &[f32],
    split: Split,
    join: Join,
    crossed: bool,
) -> (Vec<f32>, Vec<f32>) {
    let mut rig = Rig::new(left.to_vec(), right.to_vec());
    rig.graph(true);
    let (i, o) = (rig.input(), rig.output());
    let split = rig.add_node(NodeKind::Split(split));
    let join = rig.add_node(NodeKind::Join(join));
    rig.connect((i, 0), (split, 0), 0.0);
    let (a, b) = if crossed { (1, 0) } else { (0, 1) };
    rig.connect((split, 0), (join, a), 0.0);
    rig.connect((split, 1), (join, b), 0.0);
    rig.connect((join, 0), (o, 0), 0.0);
    rig.render(left.len())
}

#[test]
fn left_right_and_mid_side_splits_join_back_into_the_input() {
    let (left, right) = (sine(SECOND / 8, 440.0), noise(SECOND / 8, 11));
    let dry = Rig::new(left.clone(), right.clone()).render(left.len());
    assert!(rms(&dry.0) > 0.1 && rms(&dry.1) > 0.1);

    let lr = split_and_join(&left, &right, Split::LeftRight, Join::LeftRight, false);
    assert!(lr == dry, "left/right: exactly");
    let ms = split_and_join(&left, &right, Split::MidSide, Join::MidSide, false);
    assert!(max_difference(&ms.0, &dry.0) < 1e-6);
    assert!(max_difference(&ms.1, &dry.1) < 1e-6);

    // Crossed, left and right trade places, and mid/side becomes side/mid:
    // the right channel turned upside down.
    let swapped = split_and_join(&left, &right, Split::LeftRight, Join::LeftRight, true);
    assert!(swapped.0 == dry.1 && swapped.1 == dry.0);
    let flipped = split_and_join(&left, &right, Split::MidSide, Join::MidSide, true);
    assert!(max_difference(&flipped.0, &dry.0) < 1e-6);
    let inverted: Vec<f32> = dry.1.iter().map(|s| -s).collect();
    assert!(max_difference(&flipped.1, &inverted) < 1e-6);
}

/// A click every quarter second.
fn clicks(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| if i % (SECOND / 4) == 1000 { 0.5 } else { 0.0 })
        .collect()
}

/// Where `samples` aren't silent, and what they are there.
fn sounds(samples: &[f32]) -> Vec<(usize, f32)> {
    samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.abs() > 1e-6)
        .map(|(i, s)| (i, *s))
        .collect()
}

/// A late branch (a delay of `latency`) beside a dry one.
fn late_and_dry(rig: &mut Rig, latency: usize) -> DeviceId {
    let delay = rig.add_delay(latency);
    rig.graph(true);
    let (i, o, d) = (rig.input(), rig.output(), rig.node_of(delay));
    rig.connect((i, 0), (d, 0), 0.0);
    rig.connect((d, 0), (o, 0), 0.0);
    rig.connect((i, 0), (o, 0), 0.0);
    delay
}

#[test]
fn a_late_branch_is_met_in_time_by_the_others() {
    let input = clicks(SECOND / 2);
    let dry = Rig::mono(input.clone()).render(input.len());
    let clicks_dry = sounds(&dry.0);
    assert_eq!(clicks_dry.len(), 2);

    let mut rig = Rig::mono(input.clone());
    late_and_dry(&mut rig, 300);
    rig.load();
    assert_eq!(rig.compiler.latency(), 300, "the graph's latency counts");
    let (left, _) = rig.run(input.len());
    // One click each time, twice as loud, 300 samples late: both branches
    // meet at once.
    let heard = sounds(&left);
    let expected: Vec<_> = clicks_dry.iter().map(|(i, s)| (i + 300, 2.0 * s)).collect();
    assert_eq!(heard.len(), expected.len(), "{heard:?}");
    for ((at, level), (want_at, want)) in heard.iter().zip(&expected) {
        assert_eq!(at, want_at);
        assert!((level - want).abs() < 1e-6);
    }
}

#[test]
fn a_longer_latency_grows_the_other_branch_delay_without_allocating() {
    let input = clicks(2 * SECOND);
    let mut rig = Rig::mono(input.clone());
    let delay = late_and_dry(&mut rig, 300);
    rig.load();
    rig.run(SECOND / 2);
    // Far past the line the dry cable had: it gets a longer one, taken
    // over on the audio thread.
    rig.host.delays.borrow_mut().insert(delay, 5000);
    rig.host.generation.set(1);
    rig.load();
    assert_eq!(rig.compiler.latency(), 5000);
    let (settling, _) = rig.run(SECOND / 2);
    let (left, _) = rig.run(SECOND / 2);
    let heard = sounds(&left);
    assert_eq!(
        heard.len(),
        2,
        "{heard:?} ({} while settling)",
        sounds(&settling).len()
    );
    assert_eq!(heard[1].0 - heard[0].0, SECOND / 4);
    assert!((heard[0].1 - heard[1].1).abs() < 1e-6);
}

#[test]
fn a_cable_into_the_sidechain_port_feeds_the_sidechain() {
    let (left, right) = (sine(SECOND / 8, 440.0), noise(SECOND / 8, 13));
    let dry = Rig::new(left.clone(), right.clone()).render(left.len());
    // The echo hears the right channel on its sidechain and plays it, in
    // place of the left on its main input.
    let echoed = |sidechain: bool| {
        let mut rig = Rig::new(left.clone(), right.clone());
        let echo = rig.add("test.echo", &[]);
        rig.graph(true);
        let (i, o, e) = (rig.input(), rig.output(), rig.node_of(echo));
        let split = rig.add_node(NodeKind::Split(Split::LeftRight));
        rig.connect((i, 0), (split, 0), 0.0);
        rig.connect((split, 0), (e, 0), 0.0);
        if sidechain {
            rig.connect((split, 1), (e, 1), 0.0);
        }
        rig.connect((e, 0), (o, 0), 0.0);
        rig.render(left.len())
    };
    let (l, r) = echoed(true);
    assert!(l == dry.1 && r == dry.1);
    // Without the cable it listens to its send, which has nothing.
    let (l, r) = echoed(false);
    assert!(rms(&l) == 0.0 && rms(&r) == 0.0);
}

#[test]
fn nodes_off_the_path_are_not_run() {
    let input = noise(SECOND / 8, 17);
    let dry = Rig::mono(input.clone()).render(input.len());
    let mut rig = Rig::mono(input.clone());
    let played = rig.add("test.count", &[]);
    let dead_end = rig.add("test.count", &[]);
    let unfed = rig.add("test.count", &[]);
    rig.graph(true);
    let (i, o) = (rig.input(), rig.output());
    let [p, d, u] = [played, dead_end, unfed].map(|id| rig.node_of(id));
    rig.connect((i, 0), (p, 0), 0.0);
    rig.connect((p, 0), (o, 0), 0.0);
    rig.connect((i, 0), (d, 0), 0.0);
    rig.connect((u, 0), (o, 0), 0.0);
    rig.load();
    let (left, _) = rig.run(input.len());
    assert!(left == dry.0);
    let count = |id| rig.host.counts.borrow()[&id].load(Ordering::Relaxed);
    assert!(count(played) > 0);
    assert_eq!(count(dead_end), 0);
    assert_eq!(count(unfed), 0);
}

#[test]
fn a_graph_carries_its_state_through_an_equivalent_snapshot() {
    // A burst of noise then silence, through a band split (one band to a
    // reverb, the other through a delay) beside a quieter dry path that
    // waits for the delay: filters, a delay line, ramps and a tail.
    let mut input = noise(SECOND / 10, 19);
    input.resize(SECOND, 0.0);
    let build = || {
        let mut rig = Rig::mono(input.clone());
        let reverb = rig.add("canopy.reverb", &[]);
        let delay = rig.add_delay(100);
        rig.graph(true);
        let (i, o, r, d) = (
            rig.input(),
            rig.output(),
            rig.node_of(reverb),
            rig.node_of(delay),
        );
        let split = rig.add_node(NodeKind::Split(Split::Bands {
            crossovers: vec![500.0],
            slope: Slope::Db24,
        }));
        rig.connect((i, 0), (split, 0), 0.0);
        rig.connect((split, 0), (r, 0), 0.0);
        rig.connect((split, 1), (d, 0), 0.0);
        rig.connect((r, 0), (o, 0), 0.0);
        rig.connect((d, 0), (o, 0), 0.0);
        rig.connect((i, 0), (o, 0), -6.0);
        rig.load();
        rig
    };
    let mut steady = build();
    let whole = steady.run(SECOND);

    let mut swapped = build();
    let (mut left, mut right) = swapped.run(SECOND / 4);
    swapped
        .project
        .rename_track(swapped.track, "renamed")
        .unwrap();
    swapped.load();
    let (l, r) = swapped.run(SECOND - SECOND / 4);
    left.extend(l);
    right.extend(r);
    assert!(rms(&whole.0[SECOND / 2..]) > 1e-4, "a tail rings on");
    assert!((left, right) == whole, "nothing restarted");
}
