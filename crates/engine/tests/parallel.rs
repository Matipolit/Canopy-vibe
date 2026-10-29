//! Rendering tracks on several threads (`engine/src/pool.rs`) must change
//! nothing but the time it takes.

use std::sync::Arc;

use engine::{Command, Compiler, Engine, EngineHandle, create_with_workers};
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use project::{
    AutomationPoint, AutomationTarget, EffectRef, Mixer, NewTrack, Note, Project, SourceId,
    TrackId, TrackPosition,
};

const RATE: u32 = 48_000;

struct Rig {
    handle: EngineHandle,
    engine: Engine,
    compiler: Compiler,
}

impl Rig {
    fn new(workers: usize, project: &Project, audio: &[(SourceId, Arc<AudioBuffer>)]) -> Rig {
        let (mut handle, engine) = create_with_workers(RATE as f32, workers);
        let mut compiler = Compiler::new(RATE);
        let lookup = |id: SourceId| {
            audio
                .iter()
                .find(|(s, _)| *s == id)
                .map(|(_, a)| Arc::clone(a))
        };
        handle
            .send(Command::Arrangement(compiler.compile(project, lookup)))
            .unwrap();
        handle.send(Command::SetPlaying(true)).unwrap();
        Rig {
            handle,
            engine,
            compiler,
        }
    }

    /// Renders `frames` in device-sized callbacks: the interleaved output.
    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for chunk in out.chunks_mut(256 * 2) {
            self.engine.process_interleaved(chunk, 2);
        }
        self.handle.collect_garbage();
        out
    }
}

fn sine(hz: f32, seconds: f32, level: f32) -> Arc<AudioBuffer> {
    let samples: Vec<f32> = (0..(seconds * RATE as f32) as usize)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * level)
        .collect();
    Arc::new(AudioBuffer::new(RATE, samples.clone(), samples))
}

/// A burst at `at` samples, loud, then silence.
fn burst(at: usize, seconds: f32) -> Arc<AudioBuffer> {
    let mut samples = vec![0.0; (seconds * RATE as f32) as usize];
    for (i, s) in samples[at..at + 2400].iter_mut().enumerate() {
        *s = if i % 2 == 0 { 0.9 } else { -0.9 };
    }
    Arc::new(AudioBuffer::new(RATE, samples.clone(), samples))
}

fn add_device(project: &mut Project, track: Option<TrackId>, id: &str, settings: &[(&str, f32)]) {
    let device = project
        .add_device(track, EffectRef::builtin(id), None)
        .unwrap();
    for &(param, value) in settings {
        project.set_device_param(device, param, value).unwrap();
    }
}

fn notes(project: &mut Project, track: TrackId, count: i64, base: u8) {
    let bar = project.tempo.bar_len();
    let clip = project.add_midi_clip(track, Tick::ZERO, bar * 8).unwrap();
    let notes: Vec<_> = (0..count)
        .map(|n| Note {
            id: project.new_note_id(),
            start: Tick::new(n * 240),
            len: Tick::new(300),
            pitch: Pitch::new(base + (n % 12) as u8).unwrap(),
            velocity: Velocity::from_midi1(100),
            release_velocity: Velocity::CENTER,
            muted: false,
            // Every third note bent down a fifth and back.
            bend: if n % 3 == 0 {
                [(0, 0.0), (150, -7.0), (300, 0.0)]
                    .into_iter()
                    .map(|(at, semitones)| project::BendPoint {
                        at: Tick::new(at),
                        semitones,
                        curve: 0.0,
                    })
                    .collect()
            } else {
                Vec::new()
            },
        })
        .collect();
    project.set_notes(clip, notes).unwrap();
}

/// Every path a block has: instruments, audio, nested groups with
/// effects, sidechains across the tree (one of them a loop), automation
/// and a master chain.
fn busy_song() -> (Project, Vec<(SourceId, Arc<AudioBuffer>)>) {
    let mut project = Project::new();
    let mut audio = Vec::new();
    let synths: Vec<TrackId> = (0..4)
        .map(|i| {
            let track = project
                .add_track(NewTrack::Midi, TrackPosition::END)
                .unwrap();
            notes(&mut project, track, 64, 48 + i * 5);
            add_device(&mut project, Some(track), "canopy.eq", &[("b1_on", 1.0)]);
            track
        })
        .collect();
    let drums = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    project
        .set_instrument(drums, instruments::InstrumentRef::builtin("canopy.drums"))
        .unwrap();
    notes(&mut project, drums, 32, 36);
    let kick = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let kick_source = project.add_source("/kick.wav".into(), RATE, RATE as u64 * 2, 2);
    audio.push((kick_source, burst(4800, 2.0)));
    project
        .add_audio_clip(kick, Tick::ZERO, kick_source)
        .unwrap();
    let pad = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let pad_source = project.add_source("/pad.wav".into(), RATE, RATE as u64 * 2, 2);
    audio.push((pad_source, sine(220.0, 2.0, 0.3)));
    project.add_audio_clip(pad, Tick::ZERO, pad_source).unwrap();

    // The pad ducks under the kick; a synth ducks under the pad, and the
    // pad's group listens to the synth: a loop the schedule must break.
    let ducker = project
        .add_device(Some(pad), EffectRef::builtin("canopy.compressor"), None)
        .unwrap();
    project.set_device_sidechain(ducker, Some(kick)).unwrap();
    let synth_ducker = project
        .add_device(
            Some(synths[0]),
            EffectRef::builtin("canopy.compressor"),
            None,
        )
        .unwrap();
    project
        .set_device_sidechain(synth_ducker, Some(pad))
        .unwrap();
    let inner = project.group_tracks(&[synths[2], synths[3]]).unwrap();
    add_device(&mut project, Some(inner), "canopy.reverb", &[]);
    let outer = project.group_tracks(&[inner, pad]).unwrap();
    let group_ducker = project
        .add_device(Some(outer), EffectRef::builtin("canopy.compressor"), None)
        .unwrap();
    project
        .set_device_sidechain(group_ducker, Some(synths[0]))
        .unwrap();
    let points: Vec<_> = (0..16)
        .map(|i| AutomationPoint {
            tick: Tick::new(i * 960),
            value: 0.5 + 0.03 * i as f32,
            curve: 0.0,
        })
        .collect();
    project
        .set_automation(synths[1], AutomationTarget::Volume, points)
        .unwrap();
    project
        .set_mixer(
            drums,
            Mixer {
                pan: -0.3,
                ..Mixer::default()
            },
        )
        .unwrap();
    add_device(&mut project, None, "canopy.limiter", &[]);
    add_device(&mut project, None, "canopy.eq", &[("b2_on", 1.0)]);
    (project, audio)
}

#[test]
fn any_number_of_workers_renders_the_same_samples() {
    let (project, audio) = busy_song();
    let alone = Rig::new(0, &project, &audio).run(RATE as usize);
    assert!(alone.iter().any(|s| s.abs() > 0.01), "it plays");
    for workers in [1, 3, 7] {
        let together = Rig::new(workers, &project, &audio).run(RATE as usize);
        assert!(
            alone == together,
            "{workers} workers: differs from a single thread"
        );
    }
}

#[test]
fn a_block_rendered_in_parallel_stays_identical_across_snapshots_and_edits() {
    let (mut project, audio) = busy_song();
    let mut alone = Rig::new(0, &project, &audio);
    let mut together = Rig::new(3, &project, &audio);
    let lookup = |id: SourceId| {
        audio
            .iter()
            .find(|(s, _)| *s == id)
            .map(|(_, a)| Arc::clone(a))
    };
    for step in 0..4 {
        assert_eq!(alone.run(4096), together.run(4096), "step {step}");
        // An edit each step: the swap, with its adoption, on both.
        let ids = project.track_ids();
        project
            .set_mixer(
                ids[step],
                Mixer {
                    volume_db: -3.0 * step as f32,
                    ..Mixer::default()
                },
            )
            .unwrap();
        for rig in [&mut alone, &mut together] {
            rig.handle
                .send(Command::Arrangement(rig.compiler.compile(&project, lookup)))
                .unwrap();
        }
    }
}

#[test]
fn a_sidechain_reaches_its_listener_in_the_same_block_whatever_the_track_order() {
    // The listener is a steady tone with a ducker; the source a burst.
    // Rendered with the source after the listener and with it before:
    // the listener must duck at the same sample either way.
    fn song(source_first: bool) -> (Project, Vec<(SourceId, Arc<AudioBuffer>)>) {
        let mut project = Project::new();
        let mut audio = Vec::new();
        let mut make = |project: &mut Project, name: &str, buffer: Arc<AudioBuffer>| {
            let track = project
                .add_track(NewTrack::Audio, TrackPosition::END)
                .unwrap();
            let source = project.add_source(format!("/{name}.wav").into(), RATE, RATE as u64, 2);
            project.add_audio_clip(track, Tick::ZERO, source).unwrap();
            audio.push((source, buffer));
            track
        };
        let (listener, source) = if source_first {
            let source = make(&mut project, "source", burst(9600, 1.0));
            (
                make(&mut project, "listener", sine(440.0, 1.0, 0.5)),
                source,
            )
        } else {
            let listener = make(&mut project, "listener", sine(440.0, 1.0, 0.5));
            (listener, make(&mut project, "source", burst(9600, 1.0)))
        };
        project
            .set_mixer(
                source,
                Mixer {
                    mute: true,
                    ..Mixer::default()
                },
            )
            .unwrap();
        let ducker = project
            .add_device(
                Some(listener),
                EffectRef::builtin("canopy.compressor"),
                None,
            )
            .unwrap();
        for (param, value) in [("threshold", -40.0), ("ratio", 20.0), ("attack", 0.1)] {
            project.set_device_param(ducker, param, value).unwrap();
        }
        project.set_device_sidechain(ducker, Some(source)).unwrap();
        (project, audio)
    }
    let (first, audio) = song(true);
    let (second, audio2) = song(false);
    let source_first = Rig::new(2, &first, &audio).run(RATE as usize / 2);
    let listener_first = Rig::new(2, &second, &audio2).run(RATE as usize / 2);
    // Interleaved: frame 9600 is index 19200.
    let loud = |out: &[f32]| out[..18_000].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let ducked = |out: &[f32]| {
        out[20_000..20_600]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    assert!(
        loud(&source_first) > 0.4 && ducked(&source_first) < 0.1,
        "it ducks"
    );
    assert_eq!(
        source_first, listener_first,
        "the order of the tracks doesn't matter"
    );
}

#[test]
fn sidechains_that_form_a_loop_still_render() {
    let mut project = Project::new();
    let mut audio = Vec::new();
    let mut tracks = Vec::new();
    for name in ["a", "b", "c"] {
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let source = project.add_source(format!("/{name}.wav").into(), RATE, RATE as u64, 2);
        project.add_audio_clip(track, Tick::ZERO, source).unwrap();
        audio.push((source, sine(300.0, 1.0, 0.4)));
        tracks.push(track);
    }
    // a and b listen to each other; c, in a group, listens to its own
    // group, and the group listens to c.
    let listen = |project: &mut Project, on: TrackId, to: TrackId| {
        let ducker = project
            .add_device(Some(on), EffectRef::builtin("canopy.compressor"), None)
            .unwrap();
        project.set_device_sidechain(ducker, Some(to)).unwrap();
    };
    listen(&mut project, tracks[0], tracks[1]);
    listen(&mut project, tracks[1], tracks[0]);
    let group = project.group_tracks(&[tracks[2]]).unwrap();
    listen(&mut project, tracks[2], group);
    listen(&mut project, group, tracks[2]);
    for workers in [0, 3] {
        let out = Rig::new(workers, &project, &audio).run(RATE as usize / 4);
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(
            out[8000..].iter().any(|s| s.abs() > 0.05),
            "{workers} workers: it plays"
        );
    }
}
