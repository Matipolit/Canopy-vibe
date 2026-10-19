//! Clip mute, clip fades and freezing a MIDI clip to audio.

use std::sync::Arc;

use engine::{Command, Compiler, Engine, EngineHandle, create};
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use project::*;

const RATE: u32 = 48_000;

struct Rig {
    handle: EngineHandle,
    engine: Engine,
    compiler: Compiler,
    audio: Option<(SourceId, Arc<AudioBuffer>)>,
}

impl Rig {
    fn new() -> Rig {
        let (handle, engine) = create(RATE as f32);
        Rig {
            handle,
            engine,
            compiler: Compiler::new(RATE),
            audio: None,
        }
    }

    fn load(&mut self, project: &Project) {
        let audio = self.audio.clone();
        let arrangement = self.compiler.compile(project, |id| {
            audio
                .as_ref()
                .filter(|(s, _)| *s == id)
                .map(|(_, a)| a.clone())
        });
        self.handle.send(Command::Arrangement(arrangement)).unwrap();
    }

    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        self.engine.process_interleaved(&mut out, 2);
        self.handle.collect_garbage();
        out.chunks(2).map(|f| f[0]).collect()
    }
}

fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// One second of constant 1.0 at the session rate, as an audio clip at 0.
fn dc_project(rig: &mut Rig) -> (Project, ClipId) {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, RATE as u64, 1);
    let clip = project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let ones = vec![1.0; RATE as usize];
    rig.audio = Some((source, Arc::new(AudioBuffer::new(RATE, ones.clone(), ones))));
    (project, clip)
}

#[test]
fn muted_clips_are_silent() {
    let mut rig = Rig::new();
    let (mut project, clip) = dc_project(&mut rig);
    project.set_clips_muted(&[clip], true).unwrap();
    rig.load(&project);
    rig.handle.send(Command::SetPlaying(true)).unwrap();
    assert_eq!(peak(&rig.run(4800)), 0.0);
}

#[test]
fn fades_follow_their_curves() {
    let mut rig = Rig::new();
    let (mut project, clip) = dc_project(&mut rig);
    // A 100 ms linear fade-in, and a 100 ms fast-first fade-out.
    project.set_clip_fades(clip, 4800, 4800, 0.0, -1.0).unwrap();
    rig.load(&project);
    rig.handle.send(Command::SetPlaying(true)).unwrap();
    let out = rig.run(RATE as usize);
    // The transport's own 5 ms fade-in is over by sample 2400.
    assert!(
        (out[2399] - 0.5).abs() < 0.01,
        "halfway up the linear fade: {}",
        out[2399]
    );
    assert_eq!(out[10_000], 1.0, "full level between the fades");
    let mid_fade_out = out[RATE as usize - 2400];
    assert!(
        mid_fade_out > 0.9,
        "fast-first fade-out is still high halfway: {mid_fade_out}"
    );
    assert!(out[RATE as usize - 1] < 0.01);
}

#[test]
fn freezing_renders_the_notes_and_their_tail() {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let bar = project.tempo.bar_len();
    let clip = project.add_midi_clip(track, bar, bar).unwrap();
    let note = Note {
        id: project.new_note_id(),
        start: Tick::ZERO,
        len: Tick::new(960),
        pitch: Pitch::A4,
        velocity: Velocity::MAX,
        release_velocity: Velocity::CENTER,
        muted: false,
    };
    project.set_notes(clip, vec![note]).unwrap();
    project.set_clips_muted(&[clip], true).unwrap();

    let no_audio = |_: SourceId| None;
    let rendered = Compiler::new(RATE)
        .render_midi_clip(&project, clip, no_audio)
        .unwrap();
    assert_eq!(rendered.start, bar);
    let audio = rendered.audio;
    assert_eq!(audio.sample_rate(), RATE);
    let bar_samples = RATE as usize * 2;
    assert!(audio.frames() >= bar_samples, "at least the clip's length");
    assert!(
        peak(&audio.left()[..4800]) > 0.05,
        "the note plays from the start"
    );
    // The note ends after a beat; the default 300 ms release rings past that.
    let beat = RATE as usize / 2;
    assert!(
        peak(&audio.left()[beat + 2400..beat + 4800]) > 0.01,
        "release tail"
    );
    assert!(
        peak(&audio.left()[bar_samples - 4800..bar_samples]) < 0.001,
        "silent by the clip's end"
    );

    let error = Compiler::new(RATE).render_midi_clip(&project, ClipId(9999), no_audio);
    assert!(error.is_err());
}

#[test]
fn volume_automation_follows_its_curve() {
    // Volume from silence to 0 dB over a bar, bent to start fast or slow.
    let level_halfway = |curve: f32| {
        let mut rig = Rig::new();
        let (mut project, clip) = dc_project(&mut rig);
        let (track, _) = project.clip(clip).unwrap();
        let bar = project.tempo.bar_len();
        // Long enough to cover the whole bar.
        project.set_clip_bounds(clip, Tick::ZERO, bar).ok();
        let points = vec![
            AutomationPoint {
                tick: Tick::ZERO,
                value: 0.0,
                curve,
            },
            AutomationPoint {
                tick: bar,
                value: volume_to_normalized(0.0),
                curve: 0.0,
            },
        ];
        project
            .set_automation(track, AutomationTarget::Volume, points)
            .unwrap();
        rig.load(&project);
        rig.handle.send(Command::SetPlaying(true)).unwrap();
        let out = rig.run(24_000);
        out[23_000]
    };
    let fast = level_halfway(-1.0);
    let straight = level_halfway(0.0);
    let slow = level_halfway(1.0);
    assert!(
        fast > straight && straight > slow,
        "{fast} > {straight} > {slow}"
    );
}
