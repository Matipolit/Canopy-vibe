//! The loop region: playback goes round it seamlessly while it's on.

use std::sync::Arc;

use engine::{Command, Compiler, create};
use media::AudioBuffer;
use midi::Tick;
use project::*;

const RATE: u32 = 48_000;
/// At the default 120 BPM.
const BEAT: usize = 24_000;

#[test]
fn playback_goes_round_the_loop() {
    let (mut handle, mut engine) = create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/ramp.wav".into(), RATE, 2 * RATE as u64, 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    let ramp: Vec<f32> = (0..2 * RATE)
        .map(|i| i as f32 / RATE as f32 / 2.0)
        .collect();
    let audio = Arc::new(AudioBuffer::new(RATE, ramp.clone(), ramp.clone()));
    // Round beats 2 and 3.
    project
        .set_loop(Some(LoopRegion {
            start: Tick::QUARTER,
            end: Tick::QUARTER * 3,
            on: true,
        }))
        .unwrap();
    let lookup = |id| (id == source).then(|| audio.clone());
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();

    // Four beats in odd-sized buffers, so the loop's end falls mid-buffer.
    let mut played = Vec::new();
    let mut out = vec![0.0; 1000 * 2];
    while played.len() < 4 * BEAT {
        engine.process_interleaved(&mut out, 2);
        played.extend(out.chunks(2).map(|f| f[0]));
    }
    let at = |i: usize| ramp[i];
    // Beats 1 to 3 as they are, then beat 2 again, sample for sample.
    for i in [BEAT / 2, BEAT + 100, 3 * BEAT - 1] {
        assert!((played[i] - at(i)).abs() < 1e-6, "{i}");
    }
    for k in [0, 1, 500, BEAT - 1] {
        let wrapped = played[3 * BEAT + k];
        assert!(
            (wrapped - at(BEAT + k)).abs() < 1e-6,
            "{k} frames past the end, it plays the start again"
        );
    }
    let playhead = handle.playhead_samples() as usize;
    assert!((BEAT..3 * BEAT).contains(&playhead), "{playhead}");

    // Off: it plays on past.
    project
        .set_loop(project.loop_region.map(|r| LoopRegion { on: false, ..r }))
        .unwrap();
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.collect_garbage();
    for _ in 0..(3 * BEAT / 1000) {
        engine.process_interleaved(&mut out, 2);
    }
    assert!(handle.playhead_samples() as usize > 3 * BEAT);
}

#[test]
fn loops_are_kept_in_order_and_saved() {
    let mut project = Project::new();
    project
        .set_loop(Some(LoopRegion {
            start: Tick::QUARTER * 4,
            end: Tick::QUARTER,
            on: true,
        }))
        .unwrap();
    let region = project.loop_region.unwrap();
    assert_eq!(
        (region.start, region.end),
        (Tick::QUARTER, Tick::QUARTER * 4)
    );
    let loaded = Project::from_file_string(&project.to_file_string().unwrap()).unwrap();
    assert_eq!(loaded.loop_region, project.loop_region);
    // An empty one clears it.
    project
        .set_loop(Some(LoopRegion {
            start: Tick::QUARTER,
            end: Tick::QUARTER,
            on: true,
        }))
        .unwrap();
    assert_eq!(project.loop_region, None);
}
