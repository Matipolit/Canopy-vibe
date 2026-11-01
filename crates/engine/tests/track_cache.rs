//! Tracks played from a render of themselves (`engine::cache`): what the
//! render holds, what changes its key, and how playback moves between a
//! cache and the track's own chain.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use engine::cache::{self, Uncacheable};
use engine::{AudioLookup, Bounce, Command, Compiler, TrackCache};
use instruments::InstrumentRef;
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use project::*;

const RATE: u32 = 48_000;

/// Sources by id, and caches by track.
#[derive(Default, Clone)]
struct Lookup {
    sources: HashMap<SourceId, Arc<AudioBuffer>>,
    caches: HashMap<TrackId, TrackCache>,
}

impl AudioLookup for Lookup {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        self.sources.get(&id).cloned()
    }

    fn track_cache(&self, track: &Track) -> Option<TrackCache> {
        self.caches.get(&track.id).cloned()
    }
}

fn notes(project: &mut Project, track: TrackId, bars: i64, every: i64, len: i64) {
    let bar = project.tempo.bar_len();
    let clip = project
        .add_midi_clip(track, Tick::ZERO, bar * bars)
        .unwrap();
    let count = (bar * bars).get() / every;
    let notes: Vec<Note> = (0..count)
        .map(|n| Note {
            id: project.new_note_id(),
            start: Tick::new(n * every),
            len: Tick::new(len),
            pitch: Pitch::new(40 + (n * 7 % 30) as u8).unwrap(),
            velocity: Velocity::MAX,
            release_velocity: Velocity::CENTER,
            muted: false,
            bend: Vec::new(),
        })
        .collect();
    project.set_notes(clip, notes).unwrap();
}

fn add(project: &mut Project, track: TrackId, effect: &str) -> DeviceId {
    project
        .add_device(Some(track), EffectRef::builtin(effect), None)
        .unwrap()
}

/// A Wavetable pad through a chorus, reverb and a compressor listening to
/// a drum track, the drums, and a Poly Synth through a delay.
fn song() -> (Project, [TrackId; 3]) {
    let mut project = Project::new();
    let new = |p: &mut Project| p.add_track(NewTrack::Midi, TrackPosition::END).unwrap();
    let (pad, drums, lead) = (new(&mut project), new(&mut project), new(&mut project));
    project
        .set_instrument(pad, InstrumentRef::builtin("canopy.wavetable"))
        .unwrap();
    project
        .set_instrument(drums, InstrumentRef::builtin("canopy.drums"))
        .unwrap();
    notes(&mut project, pad, 2, 960 * 2, 1800);
    notes(&mut project, drums, 2, 480, 120);
    notes(&mut project, lead, 2, 360, 300);
    add(&mut project, pad, "canopy.chorus");
    add(&mut project, pad, "canopy.reverb");
    let ducker = add(&mut project, pad, "canopy.compressor");
    project.set_device_sidechain(ducker, Some(drums)).unwrap();
    project
        .set_device_param(ducker, "threshold", -30.0)
        .unwrap();
    add(&mut project, lead, "canopy.delay");
    project
        .set_mixer(
            pad,
            Mixer {
                volume_db: -4.0,
                pan: -0.3,
                ..Mixer::default()
            },
        )
        .unwrap();
    (project, [pad, drums, lead])
}

fn bounce(project: &Project, lookup: Lookup) -> AudioBuffer {
    let arrangement = Compiler::new(RATE).compile(project, lookup);
    let end = project.tempo.tick_to_sample(project.end(), RATE) as u64;
    let mut bounce = Bounce::new(arrangement, RATE, end, true);
    while !bounce.step(4096) {}
    bounce.finish()
}

fn render(project: &Project, track: TrackId, lookup: &Lookup) -> Arc<AudioBuffer> {
    let cancel = AtomicBool::new(false);
    Arc::new(cache::render(project, track, RATE, lookup.clone(), &cancel, |_| {}).unwrap())
}

/// Whether `a` and `b` are the same to the bit up to `exact`, and past it
/// differ by no more than a render leaves out after its tail.
fn same_until(a: &AudioBuffer, b: &AudioBuffer, exact: usize) -> bool {
    assert_eq!(a.frames(), b.frames());
    let close = |x: &[f32], y: &[f32]| {
        x[..exact] == y[..exact]
            && x[exact..]
                .iter()
                .zip(&y[exact..])
                .all(|(p, q)| (p - q).abs() < 1e-4)
    };
    close(a.left(), b.left()) && close(a.right(), b.right())
}

#[test]
fn a_song_played_from_its_caches_is_the_song_bit_for_bit() {
    let (project, tracks) = song();
    let live = bounce(&project, Lookup::default());
    let mut lookup = Lookup::default();
    let mut shortest = usize::MAX;
    for track in tracks {
        let audio = render(&project, track, &Lookup::default());
        shortest = shortest.min(audio.frames());
        lookup.caches.insert(track, cache::active(audio));
    }
    let cached = bounce(&project, lookup);
    assert!(live.left().iter().any(|s| s.abs() > 0.01), "it plays");
    let end = project.tempo.tick_to_sample(project.end(), RATE) as usize;
    assert!(shortest > end, "every render has its tail");
    assert!(same_until(&live, &cached, shortest));
}

#[test]
fn one_cached_track_among_live_ones_changes_nothing() {
    let (project, [pad, ..]) = song();
    let live = bounce(&project, Lookup::default());
    let mut lookup = Lookup::default();
    let audio = render(&project, pad, &lookup);
    let exact = audio.frames();
    lookup.caches.insert(pad, cache::active(audio));
    let cached = bounce(&project, lookup);
    assert!(same_until(&live, &cached, exact));
}

#[test]
fn an_inactive_cache_is_not_played() {
    let (project, [pad, ..]) = song();
    let live = bounce(&project, Lookup::default());
    let silence = Arc::new(AudioBuffer::new(RATE, vec![0.0; 1000], vec![0.0; 1000]));
    let mut lookup = Lookup::default();
    lookup.caches.insert(
        pad,
        TrackCache {
            audio: silence,
            active: false,
        },
    );
    let out = bounce(&project, lookup);
    assert!(live.left() == out.left() && live.right() == out.right());
}

fn key(project: &Project, track: TrackId) -> Result<u64, Uncacheable> {
    cache::render_key(
        project,
        track,
        RATE,
        &Lookup::default(),
        &mut cache::track_fingerprint,
    )
    .map(|k| k.hash)
}

#[test]
fn the_key_follows_the_sound_and_not_the_mixer() {
    let (project, [pad, drums, lead]) = song();
    let original = key(&project, pad).unwrap();
    let same = |edit: &dyn Fn(&mut Project)| {
        let mut copy = project.clone();
        edit(&mut copy);
        key(&copy, pad).unwrap() == original
    };
    // The fader and the track's look stay live.
    assert!(same(&|p| p
        .set_mixer(
            pad,
            Mixer {
                volume_db: 3.0,
                mute: true,
                solo: true,
                pan: 0.5
            }
        )
        .unwrap()));
    assert!(same(&|p| p.rename_track(pad, "renamed").unwrap()));
    let point = AutomationPoint {
        tick: Tick::ZERO,
        value: 0.5,
        curve: 0.0,
    };
    assert!(same(&|p| p
        .set_automation(pad, AutomationTarget::Volume, vec![point])
        .unwrap()));
    // So does every other track that isn't listened to.
    assert!(same(&|p| p.rename_track(lead, "x").unwrap()));
    assert!(same(&|p| add(p, lead, "canopy.reverb").ignore()));
    // What it plays, how, and what it listens to.
    assert!(!same(&|p| add(p, pad, "canopy.eq").ignore()));
    assert!(!same(&|p| p
        .set_instrument_param(pad, "volume", -12.0)
        .unwrap()));
    assert!(!same(&|p| {
        let clip = p.track(drums).unwrap().clips()[0].id();
        p.set_notes(clip, Vec::new()).unwrap();
    }));
    assert!(!same(&|p| add(p, drums, "canopy.eq").ignore()));
    assert!(!same(&|p| p.set_tempo_bpm(97.0)));
    assert!(!same(&|p| {
        let clip = p.track(pad).unwrap().clips()[0].id();
        p.set_notes(clip, Vec::new()).unwrap();
    }));
}

#[test]
fn groups_plugins_and_empty_tracks_are_not_cached() {
    let (mut project, [pad, drums, lead]) = song();
    let empty = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    assert_eq!(key(&project, empty), Err(Uncacheable::Empty));
    let group = project.group_tracks(&[lead]).unwrap();
    assert_eq!(key(&project, group), Err(Uncacheable::Group));
    assert!(key(&project, lead).is_ok(), "a track in a group is fine");
    let ducker = project.track(pad).unwrap().devices[2].id;
    project.set_device_sidechain(ducker, Some(group)).unwrap();
    assert_eq!(key(&project, pad), Err(Uncacheable::Sidechain));
    // Listening back to the track is a loop the render can't reproduce.
    project.set_device_sidechain(ducker, Some(drums)).unwrap();
    let back = add(&mut project, drums, "canopy.compressor");
    project.set_device_sidechain(back, Some(pad)).unwrap();
    assert_eq!(key(&project, pad), Err(Uncacheable::Sidechain));
}

/// A second of a constant on an audio track; its cache says something
/// else, so which one plays is plain to see.
fn constant_rig(value: f32) -> (Project, TrackId, Lookup) {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, 4 * RATE as u64, 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let samples = vec![value; 4 * RATE as usize];
    let mut lookup = Lookup::default();
    lookup.sources.insert(
        source,
        Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)),
    );
    (project, track, lookup)
}

#[test]
fn a_cache_dropped_mid_song_fades_back_to_the_track() {
    let (project, track, mut lookup) = constant_rig(0.5);
    let zeros = vec![0.0; 4 * RATE as usize];
    let silent = Arc::new(AudioBuffer::new(RATE, zeros.clone(), zeros));
    lookup.caches.insert(track, cache::active(silent.clone()));
    let (mut handle, mut engine) = engine::create_with_workers(RATE as f32, 0);
    let mut compiler = Compiler::new(RATE);
    handle
        .send(Command::Arrangement(
            compiler.compile(&project, lookup.clone()),
        ))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();
    let mut out = vec![0.0f32; 2 * 1024];
    let mut left = Vec::new();
    for _ in 0..20 {
        engine.process_interleaved(&mut out, 2);
        left.extend(out.iter().step_by(2));
    }
    assert!(
        left[2000..].iter().all(|s| *s == 0.0),
        "the silent cache plays"
    );

    // The project changed, so the cache rides along inactive and fades out.
    lookup.caches.insert(
        track,
        TrackCache {
            audio: silent,
            active: false,
        },
    );
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    let before = left.len();
    for _ in 0..20 {
        engine.process_interleaved(&mut out, 2);
        left.extend(out.iter().step_by(2));
    }
    let after = &left[before..];
    let fade = (RATE as f32 * 0.010) as usize;
    let steepest: f32 = after
        .windows(2)
        .map(|w: &[f32]| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(
        steepest < 0.5 / fade as f32 * 1.5,
        "a fade, not a jump: {steepest}"
    );
    assert!(
        after[2 * fade..].iter().all(|s| (s - 0.5).abs() < 1e-4),
        "then the track's own sound"
    );
}

#[test]
fn a_cache_plays_only_while_the_song_rolls() {
    let (project, track, mut lookup) = constant_rig(0.5);
    let quarter = vec![0.25; 4 * RATE as usize];
    let cache = Arc::new(AudioBuffer::new(RATE, quarter.clone(), quarter));
    lookup.caches.insert(track, cache::active(cache));
    let (mut handle, mut engine) = engine::create_with_workers(RATE as f32, 0);
    let mut compiler = Compiler::new(RATE);
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    let mut out = vec![0.0f32; 2 * 1024];
    engine.process_interleaved(&mut out, 2);
    assert!(out.iter().all(|s| *s == 0.0), "stopped: silent");
    handle.send(Command::SetPlaying(true)).unwrap();
    for _ in 0..4 {
        engine.process_interleaved(&mut out, 2);
    }
    assert!(
        out.iter().all(|s| (s - 0.25).abs() < 1e-6),
        "rolling: the cache, not the clip"
    );
}

trait Ignore {
    fn ignore(self);
}

impl<T> Ignore for T {
    fn ignore(self) {}
}

/// Runs `callbacks` callbacks of 1024 frames; the left channel.
fn run(engine: &mut engine::Engine, callbacks: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; 2 * 1024];
    let mut left = Vec::new();
    for _ in 0..callbacks {
        engine.process_interleaved(&mut out, 2);
        left.extend(out.iter().step_by(2));
    }
    left
}

/// Long enough for a new device's meters to stop counting as watched.
const UNWATCHED: usize = 70;

#[test]
fn a_note_from_a_keyboard_is_heard_on_a_cached_track() {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let silent = Arc::new(AudioBuffer::new(
        RATE,
        vec![0.0; 8 * RATE as usize],
        vec![0.0; 8 * RATE as usize],
    ));
    let mut lookup = Lookup::default();
    lookup.caches.insert(track, cache::active(silent));
    let (mut handle, mut engine) = engine::create_with_workers(RATE as f32, 0);
    let mut compiler = Compiler::new(RATE);
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();
    run(&mut engine, UNWATCHED);
    handle
        .send(Command::PreviewNote {
            track,
            event: midi::NoteEventKind::On {
                pitch: Pitch::A4,
                velocity: Velocity::MAX,
            },
        })
        .unwrap();
    let played = run(&mut engine, 2);
    assert!(played.iter().any(|s| s.abs() > 0.01), "the note sounds");
}

#[test]
fn a_track_whose_meters_are_on_screen_plays_its_own_chain() {
    let (mut project, track, mut lookup) = constant_rig(0.5);
    let utility = add(&mut project, track, "canopy.utility");
    let quarter = vec![0.25; 4 * RATE as usize];
    let cache = Arc::new(AudioBuffer::new(RATE, quarter.clone(), quarter));
    lookup.caches.insert(track, cache::active(cache));
    let (mut handle, mut engine) = engine::create_with_workers(RATE as f32, 0);
    let mut compiler = Compiler::new(RATE);
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();
    let before = run(&mut engine, UNWATCHED);
    assert!(
        before[before.len() - 100..]
            .iter()
            .all(|s| (s - 0.25).abs() < 1e-6),
        "nobody looking: the cache"
    );
    compiler.device_meters(utility);
    let after = run(&mut engine, 2);
    assert!(
        after[after.len() - 100..]
            .iter()
            .all(|s| (s - 0.5).abs() < 1e-4),
        "its panel open: the track itself"
    );
}

#[test]
fn turning_a_knob_on_a_cached_track_is_heard() {
    let (mut project, track, mut lookup) = constant_rig(0.5);
    let utility = add(&mut project, track, "canopy.utility");
    let quarter = vec![0.25; 4 * RATE as usize];
    let cache = Arc::new(AudioBuffer::new(RATE, quarter.clone(), quarter));
    lookup.caches.insert(track, cache::active(cache));
    let (mut handle, mut engine) = engine::create_with_workers(RATE as f32, 0);
    let mut compiler = Compiler::new(RATE);
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();
    run(&mut engine, UNWATCHED);
    let gain = effects::find_builtin("canopy.utility")
        .unwrap()
        .params
        .iter()
        .position(|p| p.id == "gain")
        .unwrap();
    handle
        .send(Command::SetDeviceParam {
            device: utility,
            index: gain,
            value: -6.0,
        })
        .unwrap();
    let after = run(&mut engine, 2);
    let half = 0.5 * dsp::db_to_gain(-6.0);
    assert!(
        after[after.len() - 100..]
            .iter()
            .all(|s| (s - half).abs() < 1e-3),
        "the track itself, 6 dB down: {}",
        after[after.len() - 1]
    );
}
