//! Effects in the engine: chains on tracks and the master, state that
//! survives edits, bypass, automation and live parameter moves.

use std::sync::Arc;

use engine::{Command, Compiler, Engine, EngineHandle, create};
use media::AudioBuffer;
use midi::Tick;
use project::*;

const RATE: u32 = 48_000;

struct Rig {
    handle: EngineHandle,
    engine: Engine,
    compiler: Compiler,
    project: Project,
    track: TrackId,
    source: SourceId,
    audio: Arc<AudioBuffer>,
}

impl Rig {
    /// An audio track playing `samples` (mono) from the start.
    fn new(samples: Vec<f32>) -> Rig {
        let (handle, engine) = create(RATE as f32);
        let mut project = Project::new();
        project.set_tempo_bpm(120.0);
        let track = project
            .add_track(NewTrack::Audio, TrackPosition::END)
            .unwrap();
        let source = project.add_source("/tmp/fx.wav".into(), RATE, samples.len() as u64, 1);
        project.add_audio_clip(track, Tick::ZERO, source).unwrap();
        Rig {
            handle,
            engine,
            compiler: Compiler::new(RATE),
            project,
            track,
            source,
            audio: Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)),
        }
    }

    fn add(&mut self, owner: Option<TrackId>, effect: &str, params: &[(&str, f32)]) -> DeviceId {
        let id = self
            .project
            .add_device(owner, EffectRef::builtin(effect), None)
            .unwrap();
        for (param, value) in params {
            self.project.set_device_param(id, param, *value).unwrap();
        }
        id
    }

    fn load(&mut self) {
        let (source, audio) = (self.source, self.audio.clone());
        let arrangement = self
            .compiler
            .compile(&self.project, |id| (id == source).then(|| audio.clone()));
        self.handle.send(Command::Arrangement(arrangement)).unwrap();
    }

    fn play(&mut self) {
        self.load();
        self.handle.send(Command::SetPlaying(true)).unwrap();
    }

    /// The left channel of the next `frames` frames.
    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for chunk in out.chunks_mut(256 * 2) {
            self.engine.process_interleaved(chunk, 2);
        }
        self.handle.collect_garbage();
        out.chunks(2).map(|f| f[0]).collect()
    }
}

fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn rms(buffer: &[f32]) -> f32 {
    (buffer.iter().map(|s| s * s).sum::<f32>() / buffer.len().max(1) as f32).sqrt()
}

/// A 10 ms burst, then silence, `seconds` long in all.
fn click(seconds: f32) -> Vec<f32> {
    let mut samples = vec![0.0; (seconds * RATE as f32) as usize];
    for (i, s) in samples.iter_mut().take(480).enumerate() {
        *s = if i % 2 == 0 { 0.8 } else { -0.8 };
    }
    samples
}

fn sine(hz: f32, seconds: f32, level: f32) -> Vec<f32> {
    (0..(seconds * RATE as f32) as usize)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * level)
        .collect()
}

#[test]
fn a_delay_echoes_and_keeps_echoing_through_edits() {
    let mut rig = Rig::new(click(3.0));
    let track = rig.track;
    let delay = rig.add(
        Some(track),
        "canopy.delay",
        &[
            ("sync", 1.0),
            ("time", 250.0),
            ("feedback", 50.0),
            ("mix", 50.0),
            ("mode", 0.0),
        ],
    );
    rig.play();
    let first = rig.run(RATE as usize * 3 / 10);
    let echo = 12_000;
    assert!(
        peak(&first[echo..echo + 600]) > 0.05,
        "an echo 250 ms later"
    );
    assert!(
        peak(&first[2_000..echo - 1000]) < 1e-3,
        "and quiet before it"
    );

    // An unrelated edit mid-tail: the next echo still comes.
    rig.project.rename_track(track, "edited").unwrap();
    rig.load();
    let rest = rig.run(RATE as usize / 2);
    // The second echo is at 500 ms; we're 300 ms in.
    let second = 24_000 - first.len();
    assert!(
        peak(&rest[second..second + 600]) > 0.02,
        "the tail survived the edit"
    );

    // Bypassed: no more echoes.
    rig.project.set_device_bypassed(delay, true).unwrap();
    rig.load();
    rig.handle.send(Command::Seek(0)).unwrap();
    let dry = rig.run(RATE as usize);
    let after = dry.len() / 2;
    assert!(
        peak(&dry[after..]) < 1e-3,
        "bypassed, the click has no echoes"
    );
}

#[test]
fn eq_boost_bypass_and_automation() {
    let mut rig = Rig::new(sine(1000.0, 4.0, 0.25));
    let track = rig.track;
    let eq = rig.add(
        Some(track),
        "canopy.eq",
        &[("b3_freq", 1000.0), ("b3_gain", 12.0), ("b3_q", 1.0)],
    );
    rig.play();
    let boosted = rms(&rig.run(RATE as usize / 2)[RATE as usize / 4..]);
    let dry = 0.25 / 2f32.sqrt();
    assert!(
        (boosted / dry - 3.98).abs() < 0.3,
        "+12 dB: {}",
        boosted / dry
    );

    rig.project.set_device_bypassed(eq, true).unwrap();
    rig.load();
    let bypassed = rms(&rig.run(RATE as usize / 2)[RATE as usize / 4..]);
    assert!(
        (bypassed / dry - 1.0).abs() < 0.02,
        "bypassed: {}",
        bypassed / dry
    );

    // Automating the gain from 0 dB to +12 dB over a bar.
    rig.project.set_device_bypassed(eq, false).unwrap();
    use effects::eq;
    let gain = &eq::DESCRIPTOR.params[eq::param(2, eq::GAIN)];
    let points = [(0, 0.0f32), (3840, 12.0)]
        .map(|(tick, db)| AutomationPoint {
            tick: Tick::new(tick),
            value: gain.to_normalized(db),
            curve: 0.0,
        })
        .to_vec();
    let target = AutomationTarget::DeviceParam {
        device: eq,
        param: "b3_gain".into(),
    };
    rig.project.set_automation(track, target, points).unwrap();
    rig.load();
    rig.handle.send(Command::Seek(0)).unwrap();
    let ramp = rig.run(RATE as usize * 2);
    let early = rms(&ramp[2_400..7_200]);
    let late = rms(&ramp[RATE as usize * 2 - 7_200..]);
    assert!(early / dry < 1.3, "starts near 0 dB: {}", early / dry);
    assert!(late / dry > 3.5, "ends near +12 dB: {}", late / dry);
}

#[test]
fn live_moves_and_the_master_chain() {
    let mut rig = Rig::new(sine(500.0, 4.0, 0.5));
    let compressor = rig.add(
        None,
        "canopy.compressor",
        &[
            ("threshold", -30.0),
            ("ratio", 20.0),
            ("attack", 1.0),
            ("mix", 100.0),
        ],
    );
    rig.play();
    let squashed = rms(&rig.run(RATE as usize / 2)[RATE as usize / 4..]);
    let dry = 0.5 / 2f32.sqrt();
    assert!(
        squashed < dry * 0.3,
        "the master compressor squashes it: {}",
        squashed / dry
    );
    assert!(
        rig.compiler.device_meters(compressor)[0] > 10.0,
        "and says so"
    );

    // A live knob move, no new snapshot: mix to 0% is dry.
    let mix = param_index("canopy.compressor", "mix");
    rig.handle
        .send(Command::SetDeviceParam {
            device: compressor,
            index: mix,
            value: 0.0,
        })
        .unwrap();
    let back = rms(&rig.run(RATE as usize / 2)[RATE as usize / 4..]);
    assert!((back / dry - 1.0).abs() < 0.05, "dry again: {}", back / dry);
}

#[test]
fn meters_read_silence_before_sound_and_when_bypassed() {
    let mut rig = Rig::new(sine(500.0, 2.0, 0.5));
    let track = rig.track;
    let compressor = rig.add(Some(track), "canopy.compressor", &[]);
    rig.load();
    // Not a block played yet: the input level is at its floor, not 0 dB.
    let input_peak = 1;
    assert!(rig.compiler.device_meters(compressor)[input_peak] < -90.0);

    rig.handle.send(Command::SetPlaying(true)).unwrap();
    rig.run(RATE as usize / 4);
    assert!(rig.compiler.device_meters(compressor)[input_peak] > -12.0);

    rig.project.set_device_bypassed(compressor, true).unwrap();
    rig.load();
    rig.run(1024);
    let meters = rig.compiler.device_meters(compressor);
    assert!(
        meters[input_peak] < -90.0,
        "bypassed: {}",
        meters[input_peak]
    );
    assert_eq!(meters[0], 0.0, "no reduction");
}

#[test]
fn tracks_stay_in_time_through_effects_that_delay_their_sound() {
    // The same impulse on two tracks.
    let mut impulse = vec![0.0; RATE as usize];
    impulse[4800] = 0.25;
    let mut rig = Rig::new(impulse);
    let limited = rig
        .project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    rig.project
        .add_audio_clip(limited, Tick::ZERO, rig.source)
        .unwrap();
    rig.add(Some(limited), "canopy.limiter", &[]);
    let lookahead = effects::limiter::latency_samples(RATE as f32);
    rig.play();
    let out = rig.run(RATE as usize / 2);
    let peaks: Vec<usize> = (0..out.len()).filter(|i| out[*i].abs() > 0.1).collect();
    assert_eq!(
        peaks,
        [4800 + lookahead],
        "one impulse of both, the plain track delayed to meet the limited one"
    );
    assert!((out[4800 + lookahead] - 0.5).abs() < 0.01);
    assert_eq!(rig.compiler.latency(), lookahead);

    // Through a group with its own lookahead: still together.
    let group = rig.project.group_tracks(&[limited]).unwrap();
    let ducker = rig.add(Some(group), "canopy.ducker", &[("lookahead", 5.0)]);
    rig.handle.send(Command::SetPlaying(false)).unwrap();
    rig.run(4800);
    rig.handle.send(Command::SetPlaying(false)).unwrap();
    rig.run(64);
    rig.play();
    let out = rig.run(RATE as usize / 2);
    let total = lookahead + 240;
    let peaks: Vec<usize> = (0..out.len()).filter(|i| out[*i].abs() > 0.1).collect();
    assert_eq!(peaks, [4800 + total]);
    assert_eq!(rig.compiler.latency(), total);

    // Bypassed, it adds nothing, and the plain track follows.
    rig.project.set_device_bypassed(ducker, true).unwrap();
    rig.load();
    assert_eq!(rig.compiler.latency(), lookahead);
}

fn param_index(effect: &str, param: &str) -> usize {
    EffectRef::builtin(effect)
        .descriptor()
        .unwrap()
        .params
        .iter()
        .position(|p| p.id == param)
        .unwrap()
}

#[test]
fn taps_hand_the_ui_what_goes_in_and_out() {
    let mut rig = Rig::new(sine(1000.0, 1.0, 0.25));
    let track = rig.track;
    let eq = rig.add(
        Some(track),
        "canopy.eq",
        &[("b3_freq", 1000.0), ("b3_gain", 12.0)],
    );
    rig.play();
    let out = rig.run(4096);
    let mut frames = Vec::new();
    assert!(rig.compiler.drain_tap(eq, |f| frames.push(f)));
    assert_eq!(frames.len(), 4096);
    let input: Vec<f32> = frames.iter().map(|f| f[0]).collect();
    let tapped: Vec<f32> = frames.iter().map(|f| f[2]).collect();
    assert_eq!(tapped, out, "the tap is what the EQ put out");
    let boost = rms(&tapped[2048..]) / rms(&input[2048..]);
    assert!((boost - 4.0).abs() < 0.3, "+12 dB at the band: {boost}");

    // Nothing new, nothing to read; effects without a tap have none.
    assert!(rig.compiler.drain_tap(eq, |_| panic!("stale frame")));
    let delay = rig.add(Some(track), "canopy.delay", &[]);
    rig.load();
    rig.run(256);
    assert!(!rig.compiler.drain_tap(delay, |_| {}));
}

#[test]
fn a_sidechain_ducks_one_track_under_another() {
    // The "kick" is loud and muted, like a ghost track; the "bass" is quiet.
    let mut rig = Rig::new(sine(200.0, 2.0, 0.5));
    let kick = rig.track;
    let bass = rig
        .project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let clip = rig
        .project
        .add_audio_clip(bass, Tick::ZERO, rig.source)
        .unwrap();
    rig.project.set_clip_gain(clip, -30.0).unwrap();
    let muted = Mixer {
        mute: true,
        ..Mixer::default()
    };
    rig.project.set_mixer(kick, muted).unwrap();
    let ducker = rig.add(
        Some(bass),
        "canopy.compressor",
        &[("threshold", -30.0), ("ratio", 20.0), ("attack", 1.0)],
    );
    rig.play();
    let alone = rms(&rig.run(RATE as usize / 2)[RATE as usize / 4..]);
    assert!(alone > 0.005, "the bass on its own is under the threshold");
    assert!(rig.compiler.device_meters(ducker)[0] < 0.5);

    rig.project
        .set_device_sidechain(ducker, Some(kick))
        .unwrap();
    rig.load();
    let ducked = rms(&rig.run(RATE as usize / 2)[RATE as usize / 4..]);
    assert!(
        ducked < alone * 0.2,
        "the muted kick still ducks it: {} of {alone}",
        ducked
    );
    assert!(rig.compiler.device_meters(ducker)[0] > 10.0);

    // The sidechain follows the source to the master chain too.
    rig.project.set_device_sidechain(ducker, None).unwrap();
    let master = rig.add(
        None,
        "canopy.compressor",
        &[("threshold", -30.0), ("ratio", 20.0), ("attack", 1.0)],
    );
    rig.project
        .set_device_sidechain(master, Some(kick))
        .unwrap();
    rig.load();
    let ducked = rms(&rig.run(RATE as usize / 2)[RATE as usize / 4..]);
    assert!(ducked < alone * 0.2, "{ducked} of {alone}");
}
