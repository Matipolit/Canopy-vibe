//! Regressions: values left behind by automation, and clicks on seek.

use std::sync::Arc;

use engine::{Command, Compiler, Engine, EngineHandle, create};
use media::AudioBuffer;
use midi::{NoteEventKind, Pitch, Tick, Velocity};
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
        let a = self.compiler.compile(project, |id| {
            audio
                .as_ref()
                .filter(|(s, _)| *s == id)
                .map(|(_, a)| a.clone())
        });
        self.handle.send(Command::Arrangement(a)).unwrap();
    }
    fn send(&mut self, c: Command) {
        self.handle.send(c).unwrap();
    }
    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        self.engine.process_interleaved(&mut out, 2);
        self.handle.collect_garbage();
        out.chunks(2).map(|f| f[0]).collect()
    }
}

fn peak(b: &[f32]) -> f32 {
    b.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn on() -> NoteEventKind {
    NoteEventKind::On {
        pitch: Pitch::A4,
        velocity: Velocity::MAX,
    }
}

/// Plays a previewed note on a fresh MIDI track and returns its peak.
fn preview_peak(remove_automation_after_play: bool) -> f32 {
    let mut rig = Rig::new();
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    // Automate the synth's output gain all the way down (-60 dB).
    let gain = AutomationTarget::InstrumentParam("gain".into());
    if remove_automation_after_play {
        project
            .set_automation(
                track,
                gain.clone(),
                vec![AutomationPoint {
                    tick: Tick::ZERO,
                    value: 0.0,
                    curve: 0.0,
                }],
            )
            .unwrap();
    }
    rig.load(&project);
    rig.send(Command::SetPlaying(true));
    rig.run(4800);
    rig.send(Command::SetPlaying(false));
    rig.run(4800);

    // The user deletes the automation lane: the knob shows the static value
    // (the default, -6 dB) again.
    project.set_automation(track, gain, Vec::new()).unwrap();
    rig.load(&project);
    rig.run(64);

    rig.send(Command::PreviewNote { track, event: on() });
    peak(&rig.run(9600))
}

#[test]
fn removing_param_automation_restores_static_value() {
    let reference = preview_peak(false);
    let after = preview_peak(true);
    assert!(reference > 0.05);
    assert!(
        (after - reference).abs() < reference * 0.1,
        "instrument still at the automated value: {after} vs {reference}"
    );
}

#[test]
fn seeking_while_playing_does_not_click() {
    let mut rig = Rig::new();
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, RATE as u64 * 4, 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let dc = vec![0.5; RATE as usize * 4];
    rig.audio = Some((source, Arc::new(AudioBuffer::new(RATE, dc.clone(), dc))));
    rig.load(&project);
    rig.send(Command::SetPlaying(true));
    let before = rig.run(4800);
    rig.send(Command::Seek(RATE as u64 * 2));
    let after = rig.run(64);
    let (last, first) = (before[before.len() - 1], after[0]);
    assert!(
        (last - first).abs() < 0.05,
        "jump of {} at the seek",
        (last - first).abs()
    );
}
