//! A track whose chain has been silent for ten seconds, with nothing
//! playing into it, rests: its instrument and effects aren't run until
//! something plays again, when they start afresh. Watched through a fake
//! plugin effect that counts its blocks.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use effects::{Category, Context, Effect, EffectDescriptor, Format, Pace};
use engine::{AudioLookup, Command, Compiler, Engine, EngineHandle};
use instruments::{ParamDescriptor, ParameterKind};
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use project::{Device, DeviceId, EffectRef, NewTrack, Note, Project, SourceId, TrackPosition};

const RATE: u32 = 48_000;
const SECOND: usize = RATE as usize;

/// What the fake has been through.
#[derive(Default)]
struct Record {
    processed: AtomicUsize,
    resets: AtomicUsize,
    restless: AtomicBool,
}

impl Record {
    fn processed(&self) -> usize {
        self.processed.load(Ordering::Relaxed)
    }

    fn resets(&self) -> usize {
        self.resets.load(Ordering::Relaxed)
    }
}

/// Halves the sound and counts its blocks.
struct Probe(Arc<Record>);

impl Effect for Probe {
    fn prepare(&mut self, _: f32, _: usize) {}
    fn reset(&mut self) {
        self.0.resets.fetch_add(1, Ordering::Relaxed);
    }
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
    fn restless(&self) -> bool {
        self.0.restless.load(Ordering::Relaxed)
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

fn probe() -> EffectRef {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        effects::register_external(
            Format::Clap,
            "test.rest-probe",
            EffectDescriptor {
                id: "test.rest-probe",
                name: "Rest Probe",
                description: "",
                version: "1",
                category: Category::Dynamics,
                params: &PARAMS,
                settings: &[],
                meters: &[],
                levels: effects::meter::Levels::NONE,
                tap: false,
                sidechain: false,
                latency: effects::no_latency,
                create: || unreachable!("plugins come from the lookup"),
            },
        );
    });
    EffectRef {
        format: Format::Clap,
        id: "test.rest-probe".into(),
        version: "1".into(),
    }
}

/// The audio, and the one probe's record.
struct Host {
    source: Option<(SourceId, Arc<AudioBuffer>)>,
    record: Arc<Record>,
}

impl AudioLookup for &Host {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self.source
            .as_ref()
            .filter(|(source, _)| *source == id)
            .map(|(_, audio)| audio.clone())
    }

    fn plugin_effect(&self, _: &Device, _: u32, _: usize) -> Option<Box<dyn Effect>> {
        Some(Box::new(Probe(self.record.clone())))
    }
}

struct Rig {
    handle: EngineHandle,
    engine: Engine,
    host: Host,
}

impl Rig {
    fn playing(project: &Project, host: Host) -> Rig {
        let (mut handle, engine) = engine::create(RATE as f32);
        let arrangement = Compiler::new(RATE).compile(project, &host);
        handle.send(Command::Arrangement(arrangement)).unwrap();
        handle.send(Command::SetPlaying(true)).unwrap();
        Rig {
            handle,
            engine,
            host,
        }
    }

    /// Plays `frames` and returns the left channel.
    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for callback in out.chunks_mut(2 * 512) {
            self.engine.process_interleaved(callback, 2);
        }
        out.chunks(2).map(|frame| frame[0]).collect()
    }

    fn record(&self) -> &Record {
        &self.host.record
    }
}

fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// A second of constant 0.5 at the song's start and again at 25 seconds,
/// on an audio track in a group, with the probe on the track or on the
/// group.
fn two_clips(on_group: bool) -> (Project, Host, DeviceId) {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let group = project.group_tracks(&[track]).unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, u64::from(RATE), 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    // 120 BPM: 50 beats in.
    project
        .add_audio_clip(track, Tick::QUARTER * 50, source)
        .unwrap();
    let owner = if on_group { group } else { track };
    let device = project.add_device(Some(owner), probe(), None).unwrap();
    let samples = vec![0.5; SECOND];
    let host = Host {
        source: Some((
            source,
            Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)),
        )),
        record: Arc::default(),
    };
    (project, host, device)
}

fn rests_between_its_clips(on_group: bool) {
    let (project, host, _) = two_clips(on_group);
    let mut rig = Rig::playing(&project, host);
    let first = rig.run(SECOND / 2);
    assert_eq!(first.last(), Some(&0.25), "the clip plays through the probe");

    // The clip ends at one second; ten silent seconds later it rests.
    rig.run(11 * SECOND + SECOND / 2);
    let (processed, resets) = (rig.record().processed(), rig.record().resets());
    let quiet = rig.run(10 * SECOND);
    assert_eq!(peak(&quiet), 0.0);
    assert_eq!(rig.record().processed(), processed, "not run while resting");

    // The second clip, at 25 seconds: it runs again, afresh.
    let second = rig.run(3 * SECOND + SECOND / 2);
    assert_eq!(second.last(), Some(&0.25), "and plays again");
    assert!(rig.record().processed() > processed);
    assert_eq!(rig.record().resets(), resets + 1, "reset once as it woke");
}

#[test]
fn a_track_rests_after_ten_silent_seconds_and_plays_its_next_clip() {
    rests_between_its_clips(false);
}

#[test]
fn a_group_rests_while_nothing_plays_into_it() {
    rests_between_its_clips(true);
}

#[test]
fn a_restless_effect_keeps_its_track_running() {
    let (project, host, _) = two_clips(false);
    host.record.restless.store(true, Ordering::Relaxed);
    let mut rig = Rig::playing(&project, host);
    rig.run(12 * SECOND);
    let processed = rig.record().processed();
    rig.run(5 * SECOND);
    assert!(rig.record().processed() > processed, "its window is open");
    // Closed: it rests like any other.
    rig.record().restless.store(false, Ordering::Relaxed);
    rig.run(SECOND);
    let processed = rig.record().processed();
    rig.run(SECOND);
    assert_eq!(rig.record().processed(), processed);
}

#[test]
fn a_knob_turned_wakes_a_resting_track() {
    let (project, host, device) = two_clips(false);
    let mut rig = Rig::playing(&project, host);
    rig.run(12 * SECOND);
    let processed = rig.record().processed();
    rig.run(SECOND);
    assert_eq!(rig.record().processed(), processed, "resting");
    rig.handle
        .send(Command::SetDeviceParam {
            device,
            index: 0,
            value: 0.5,
        })
        .unwrap();
    rig.run(SECOND);
    assert!(rig.record().processed() > processed, "heard at once");
}

#[test]
fn an_instrument_rests_between_its_notes_and_plays_the_next() {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    project.add_device(Some(track), probe(), None).unwrap();
    // A beat-long note at the start and another 40 seconds (80 beats) in.
    let clip = project
        .add_midi_clip(track, Tick::ZERO, Tick::QUARTER * 100)
        .unwrap();
    let notes: Vec<Note> = [0, 80]
        .into_iter()
        .map(|beat| Note {
            id: project.new_note_id(),
            start: Tick::QUARTER * beat,
            len: Tick::QUARTER,
            pitch: Pitch::A4,
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        })
        .collect();
    project.set_notes(clip, notes).unwrap();
    let host = Host {
        source: None,
        record: Arc::default(),
    };
    let mut rig = Rig::playing(&project, host);
    let first = rig.run(SECOND / 4);
    assert!(peak(&first) > 0.01, "the first note sounds");

    rig.run(20 * SECOND);
    let processed = rig.record().processed();
    let quiet = rig.run(19 * SECOND);
    assert_eq!(peak(&quiet), 0.0);
    assert_eq!(rig.record().processed(), processed, "resting between notes");

    // 39.25 seconds have played: the second note starts at 40.
    rig.run(SECOND * 3 / 4);
    let second = rig.run(SECOND / 4);
    // The same note from an instrument started afresh: the same sound.
    assert!(peak(&second) > 0.01, "the second note sounds");
    for (a, b) in first.iter().zip(&second) {
        assert!((a - b).abs() < 1e-4, "{a} against {b}");
    }
}
