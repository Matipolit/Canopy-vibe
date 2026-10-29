//! Rendering a whole project offline, for exporting.

use std::sync::Arc;

use engine::{Bounce, Compiler};
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use project::*;

const RATE: u32 = 48_000;

fn bounce(
    project: &Project,
    audio: Option<(SourceId, Arc<AudioBuffer>)>,
    tail: bool,
) -> AudioBuffer {
    let arrangement = Compiler::new(RATE).compile(project, |id| {
        audio
            .as_ref()
            .filter(|(s, _)| *s == id)
            .map(|(_, a)| a.clone())
    });
    let end = project.tempo.tick_to_sample(project.end(), RATE) as u64;
    let mut bounce = Bounce::new(arrangement, RATE, end, tail);
    let mut steps = 0;
    while !bounce.step(10_000) {
        steps += 1;
        assert!(bounce.progress() < 1.0);
    }
    assert!(steps > 0);
    assert_eq!(bounce.progress(), 1.0);
    bounce.finish()
}

#[test]
fn bounces_start_to_end_at_full_level_through_the_faders() {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, RATE as u64, 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let mixer = |volume_db| Mixer {
        volume_db,
        ..Mixer::default()
    };
    project.set_mixer(track, mixer(-6.0)).unwrap();
    project.master = mixer(-6.0);
    let ones = vec![1.0; RATE as usize];
    let audio = Arc::new(AudioBuffer::new(RATE, ones.clone(), ones));

    let out = bounce(&project, Some((source, audio)), true);
    assert_eq!(out.frames(), RATE as usize, "a second, and no silent tail");
    // Both faders at -6 dB, already there at the first sample (only the
    // clip's 4 ms click guard fades in). Pan is a balance: center is full.
    let level = dsp::db_to_gain(-12.0);
    let left = out.left();
    for i in [400, 1000, RATE as usize / 2] {
        assert!(
            (left[i] - level).abs() < 1e-4,
            "{i}: {} vs {level}",
            left[i]
        );
    }
}

#[test]
fn the_tail_rings_out_past_the_end() {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let bar = project.tempo.bar_len();
    let clip = project.add_midi_clip(track, Tick::ZERO, bar).unwrap();
    let note = Note {
        id: project.new_note_id(),
        start: bar - Tick::new(240),
        len: Tick::new(240),
        pitch: Pitch::A4,
        velocity: Velocity::MAX,
        release_velocity: Velocity::CENTER,
        muted: false,
        bend: Vec::new(),
    };
    project.set_notes(clip, vec![note]).unwrap();
    let bar_samples = RATE as usize * 2;

    let cut = bounce(&project, None, false);
    assert_eq!(cut.frames(), bar_samples);
    let with_tail = bounce(&project, None, true);
    assert!(
        with_tail.frames() > bar_samples + RATE as usize / 20,
        "{}",
        with_tail.frames()
    );
    assert!(with_tail.frames() < bar_samples + RATE as usize * 2);
}
