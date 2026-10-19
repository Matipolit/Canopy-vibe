//! Instruments in the engine: their meters reach the UI, and the tempo
//! reaches them.

use engine::{Command, Compiler, create};
use midi::{NoteEventKind, Pitch, Velocity};
use project::*;

const RATE: u32 = 48_000;

#[test]
fn instrument_meters_reach_the_compiler() {
    let (mut handle, mut engine) = create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    project
        .set_instrument(track, instruments::InstrumentRef::builtin("canopy.drums"))
        .unwrap();
    let no_audio = |_: SourceId| None;
    handle
        .send(Command::Arrangement(compiler.compile(&project, no_audio)))
        .unwrap();
    let mut out = vec![0.0; 512 * 2];
    engine.process_interleaved(&mut out, 2);
    assert_eq!(compiler.instrument_meters(track)[0], 0.0, "silent kit");

    // A kick (36): its pad's meter, slot 0, lights.
    handle
        .send(Command::PreviewNote {
            track,
            event: NoteEventKind::On {
                pitch: Pitch::new(36).unwrap(),
                velocity: Velocity::MAX,
            },
        })
        .unwrap();
    engine.process_interleaved(&mut out, 2);
    // The callback's load is measured, and taken once.
    let load = handle.take_load();
    assert!(load > 0.0 && load.is_finite(), "{load}");
    assert_eq!(handle.take_load(), 0.0);
    let meters = compiler.instrument_meters(track);
    assert!(meters[0] > 0.5, "the kick's envelope: {}", meters[0]);
    assert_eq!(meters[2], 0.0, "and only the kick's");

    // A new snapshot keeps the same meters.
    project.rename_track(track, "Drums").unwrap();
    handle
        .send(Command::Arrangement(compiler.compile(&project, no_audio)))
        .unwrap();
    engine.process_interleaved(&mut out, 2);
    handle.collect_garbage();
    assert!(compiler.instrument_meters(track)[0] > 0.1);
}
