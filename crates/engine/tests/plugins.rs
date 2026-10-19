//! Plugins through the compiler's hooks (`AudioLookup::plugin_effect` and
//! friends), with a fake plugin effect: each instance is used until the
//! engine is done with it, then retired exactly once, on the audio thread,
//! without allocating, and dropped back on the UI thread. A restarted
//! plugin (a new generation) gets a fresh instance; one that isn't ready is
//! silent; a plugin's latency counts.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use effects::{Category, Context, Effect, EffectDescriptor, Format, Pace};
use engine::{AudioLookup, Command, Compiler, Engine, EngineHandle};
use instruments::{ParamDescriptor, ParameterKind};
use media::AudioBuffer;
use midi::Tick;
use project::{Device, DeviceId, EffectRef, NewTrack, Project, SourceId, TrackPosition};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: u32 = 48_000;
const BLOCK: usize = 512;

thread_local! {
    /// Set on the thread playing the engine.
    static AUDIO: Cell<bool> = const { Cell::new(false) };
}

/// What happened to one instance.
#[derive(Default)]
struct Record {
    processed: AtomicUsize,
    retired: AtomicUsize,
    retired_elsewhere: AtomicUsize,
    dropped: AtomicUsize,
    dropped_on_audio: AtomicUsize,
}

/// Halves the sound, and records its life.
struct Fake(Arc<Record>);

impl Effect for Fake {
    fn prepare(&mut self, _: f32, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, left: &mut [f32], right: &mut [f32], _: &Context) {
        for s in left.iter_mut().chain(right.iter_mut()) {
            *s *= 0.5;
        }
        self.0.processed.fetch_add(1, Ordering::Relaxed);
    }
    fn params(&self) -> &'static [ParamDescriptor] {
        &PARAMS
    }
    fn receive(&mut self, _: usize, _: f32, _: Pace) {}
    fn retire(&mut self) {
        let count = if AUDIO.get() {
            &self.0.retired
        } else {
            &self.0.retired_elsewhere
        };
        count.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::Relaxed);
        if AUDIO.get() {
            self.0.dropped_on_audio.fetch_add(1, Ordering::Relaxed);
        }
    }
}

static PARAMS: [ParamDescriptor; 1] = [ParamDescriptor {
    id: "1",
    name: "Drive",
    kind: ParameterKind::Float {
        min: 0.0,
        max: 1.0,
        default: 0.0,
        unit: "",
        logarithmic: false,
    },
}];

/// The plugin host's side, as far as the compiler sees it.
struct Host {
    source: (SourceId, Arc<AudioBuffer>),
    /// One record per instance handed out, in order.
    instances: std::cell::RefCell<Vec<Arc<Record>>>,
    generation: Cell<u64>,
    ready: Cell<bool>,
    latency: usize,
}

impl AudioLookup for &Host {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        (id == self.source.0).then(|| self.source.1.clone())
    }

    fn plugin_effect(
        &self,
        device: &Device,
        _sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Effect>> {
        assert!(!AUDIO.get(), "asked on the UI thread");
        assert_eq!(device.effect.format, Format::Clap);
        assert_eq!(max_block, engine::MAX_BLOCK);
        if !self.ready.get() {
            return None;
        }
        let record = Arc::new(Record::default());
        self.instances.borrow_mut().push(record.clone());
        Some(Box::new(Fake(record)))
    }

    fn plugin_latency(&self, _device: DeviceId) -> Option<usize> {
        Some(self.latency)
    }

    fn plugin_generation(&self, _device: DeviceId) -> u64 {
        self.generation.get()
    }
}

/// Plays `engine` for a while on a thread of its own, as the audio thread,
/// where nothing may allocate. Returns it, and the last sample it played.
fn play(engine: Engine) -> (Engine, f32) {
    std::thread::spawn(move || {
        AUDIO.set(true);
        let mut engine = engine;
        let mut out = vec![0.0; BLOCK * 2];
        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..20 {
                engine.process_interleaved(&mut out, 2);
            }
        });
        let last = out[out.len() - 2];
        (engine, last)
    })
    .join()
    .unwrap()
}

fn send(handle: &mut EngineHandle, compiler: &mut Compiler, project: &Project, host: &Host) {
    let arrangement = compiler.compile(project, host);
    handle.send(Command::Arrangement(arrangement)).unwrap();
}

fn count(record: &Record, of: impl Fn(&Record) -> &AtomicUsize) -> usize {
    of(record).load(Ordering::Relaxed)
}

#[test]
fn plugin_instances_are_retired_once_on_the_audio_thread() {
    effects::register_external(
        Format::Clap,
        "test.fake-gain",
        EffectDescriptor {
            id: "test.fake-gain",
            name: "Fake Gain",
            description: "",
            version: "1",
            category: Category::Dynamics,
            params: &PARAMS,
            settings: &[],
            meters: &[],
            tap: false,
            sidechain: false,
            latency: effects::no_latency,
            create: || unreachable!("plugins come from the lookup"),
        },
    );
    // Ten seconds of 0.5 on an audio track, through the fake.
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, 10 * RATE as u64, 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let samples = vec![0.5; 10 * RATE as usize];
    let reference = EffectRef {
        format: Format::Clap,
        id: "test.fake-gain".into(),
        version: "1".into(),
    };
    let device = project.add_device(Some(track), reference, None).unwrap();
    let host = Host {
        source: (
            source,
            Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)),
        ),
        instances: Default::default(),
        generation: Cell::new(0),
        ready: Cell::new(true),
        latency: 100,
    };
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    let record = |i: usize| host.instances.borrow()[i].clone();

    send(&mut handle, &mut compiler, &project, &host);
    assert_eq!(compiler.latency(), 100, "the plugin's latency counts");
    handle.send(Command::SetPlaying(true)).unwrap();
    let (engine, last) = play(engine);
    assert_eq!(last, 0.25, "it plays");
    let first = record(0);
    assert!(count(&first, |r| &r.processed) > 0);
    assert_eq!(count(&first, |r| &r.retired), 0, "still in use");

    // An unrelated edit keeps the instance.
    project.rename_track(track, "renamed").unwrap();
    send(&mut handle, &mut compiler, &project, &host);
    let (engine, _) = play(engine);
    handle.collect_garbage();
    assert_eq!(host.instances.borrow().len(), 1);
    assert_eq!(count(&first, |r| &r.retired), 0);
    assert_eq!(count(&first, |r| &r.dropped), 0);

    // A restart: a new instance, and the old one retired and dropped.
    host.generation.set(1);
    send(&mut handle, &mut compiler, &project, &host);
    let (engine, last) = play(engine);
    assert_eq!(last, 0.25);
    assert_eq!(count(&first, |r| &r.retired), 1);
    handle.collect_garbage();
    assert_eq!(count(&first, |r| &r.dropped), 1);
    let second = record(1);
    assert_eq!(count(&second, |r| &r.retired), 0);

    // Not ready: silent, asked again next time, and the old one retired.
    host.ready.set(false);
    host.generation.set(2);
    send(&mut handle, &mut compiler, &project, &host);
    let (engine, last) = play(engine);
    assert_eq!(last, 0.0, "silent while it restarts");
    assert_eq!(count(&second, |r| &r.retired), 1);
    host.ready.set(true);
    send(&mut handle, &mut compiler, &project, &host);
    let (engine, last) = play(engine);
    assert_eq!(last, 0.25);
    let third = record(2);

    // Removed: retired, and the sound passes again.
    project.remove_device(device).unwrap();
    send(&mut handle, &mut compiler, &project, &host);
    let (engine, last) = play(engine);
    assert_eq!(last, 0.5);
    let (_engine, _) = play(engine);
    handle.collect_garbage();
    for record in host.instances.borrow().iter() {
        assert_eq!(count(record, |r| &r.retired), 1, "exactly once");
        assert_eq!(count(record, |r| &r.retired_elsewhere), 0);
        assert_eq!(count(record, |r| &r.dropped), 1);
        assert_eq!(count(record, |r| &r.dropped_on_audio), 0);
    }
    assert_eq!(host.instances.borrow().len(), 3);
    assert_eq!(count(&third, |r| &r.retired), 1);
}

#[test]
fn a_graph_plays_its_effects_in_sound_order() {
    // A utility at -6 dB, in a graph with a muted path around it, so the
    // graph isn't linear.
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, RATE as u64, 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let down = project
        .add_device(Some(track), EffectRef::builtin("canopy.utility"), None)
        .unwrap();
    project.set_device_param(down, "gain", -6.0).unwrap();
    let graph = project.ensure_routing(Some(track)).unwrap();
    let (input, output) = (graph.input().unwrap(), graph.output().unwrap());
    graph.cables.push(project::Cable {
        from: project::PortRef {
            node: input,
            port: 0,
        },
        to: project::PortRef {
            node: output,
            port: 0,
        },
        gain_db: 0.0,
        muted: true,
    });
    project.normalize_routing(Some(track)).unwrap();
    assert!(project.routing(Some(track)).is_some());
    let samples = vec![0.5; RATE as usize];
    let host = Host {
        source: (
            source,
            Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)),
        ),
        instances: Default::default(),
        generation: Cell::new(0),
        ready: Cell::new(true),
        latency: 0,
    };
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &project, &host);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (_engine, last) = play(engine);
    let expected = 0.5 * dsp::db_to_gain(-6.0);
    assert!((last - expected).abs() < 1e-3, "{last} vs {expected}");
}
