//! Playing a MIDI keyboard through the engine: routing, the sustain pedal,
//! what's reported for recording, and the metronome.

use engine::{Command, Compiler, Engine, EngineHandle, LiveEvent, LiveSender, Played, create};
use midi::{NoteEventKind, Pitch, Velocity};
use project::*;

const RATE: u32 = 48_000;

struct Rig {
    handle: EngineHandle,
    engine: Engine,
    live: LiveSender,
    track: TrackId,
}

impl Rig {
    fn new() -> Rig {
        let (mut handle, engine) = create(RATE as f32);
        let mut project = Project::new();
        let track = project
            .add_track(NewTrack::Midi, TrackPosition::END)
            .unwrap();
        let arrangement = Compiler::new(RATE).compile(&project, |_| None);
        handle.send(Command::Arrangement(arrangement)).unwrap();
        let live = handle.live_sender();
        Rig {
            handle,
            engine,
            live,
            track,
        }
    }

    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for chunk in out.chunks_mut(256 * 2) {
            self.engine.process_interleaved(chunk, 2);
        }
        self.handle.collect_garbage();
        out.chunks(2).map(|f| f[0]).collect()
    }

    fn played(&mut self) -> Vec<Played> {
        std::iter::from_fn(|| self.handle.pop_played()).collect()
    }
}

fn note(on: bool) -> LiveEvent {
    let pitch = Pitch::A4;
    LiveEvent::Note(if on {
        NoteEventKind::On {
            pitch,
            velocity: Velocity::MAX,
        }
    } else {
        NoteEventKind::Off {
            pitch,
            velocity: Velocity::CENTER,
        }
    })
}

fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn keys_play_the_target_track_and_the_pedal_holds_them() {
    let mut rig = Rig::new();
    // No target yet: nothing plays, but it's still reported.
    rig.live.send(note(true));
    assert_eq!(peak(&rig.run(4800)), 0.0);
    rig.live.send(note(false));
    rig.run(64);
    assert_eq!(rig.played().len(), 2);

    let track = rig.track;
    rig.handle
        .send(Command::SetLiveTarget(Some(track)))
        .unwrap();
    rig.live.send(note(true));
    assert!(peak(&rig.run(4800)) > 0.05, "plays");
    let played = rig.played();
    assert!(matches!(
        played[..],
        [Played {
            sample: None,
            kind: NoteEventKind::On { .. }
        }]
    ));

    // Pedal down, key up: still sounding a second later.
    rig.live.send(LiveEvent::Sustain(true));
    rig.live.send(note(false));
    assert!(
        peak(&rig.run(RATE as usize)[RATE as usize - 4800..]) > 0.05,
        "held by the pedal"
    );
    assert!(rig.played().is_empty(), "the release waits for the pedal");
    // Pedal up: released.
    rig.live.send(LiveEvent::Sustain(false));
    let out = rig.run(RATE as usize);
    assert!(peak(&out[RATE as usize - 4800..]) < 1e-3, "released");
    assert!(matches!(
        rig.played()[..],
        [Played {
            kind: NoteEventKind::Off { .. },
            ..
        }]
    ));
}

#[test]
fn changing_target_releases_held_notes_and_playing_reports_positions() {
    let mut rig = Rig::new();
    let track = rig.track;
    rig.handle
        .send(Command::SetLiveTarget(Some(track)))
        .unwrap();
    rig.handle.send(Command::SetPlaying(true)).unwrap();
    rig.run(4800);
    rig.live.send(note(true));
    rig.run(4800);
    let played = rig.played();
    let sample = played[0].sample.expect("rolling");
    assert!((4800..=4800 + 256).contains(&sample), "{sample}");

    rig.handle.send(Command::SetLiveTarget(None)).unwrap();
    let out = rig.run(RATE as usize);
    assert!(
        peak(&out[RATE as usize - 4800..]) < 1e-3,
        "released on the old track"
    );
    assert!(matches!(
        rig.played()[..],
        [Played {
            kind: NoteEventKind::Off { .. },
            ..
        }]
    ));
}

#[test]
fn the_metronome_clicks_on_beats() {
    let mut rig = Rig::new();
    rig.handle.send(Command::SetMetronome(true)).unwrap();
    // Stopped: quiet.
    assert_eq!(peak(&rig.run(RATE as usize)), 0.0);
    rig.handle.send(Command::SetPlaying(true)).unwrap();
    // 120 BPM: a beat every 24000 samples.
    let out = rig.run(RATE as usize * 2);
    for beat in 0..4 {
        let at = beat * 24_000;
        assert!(peak(&out[at..at + 1000]) > 0.1, "beat {beat}");
        assert!(
            peak(&out[at + 4000..at + 20_000]) < 1e-3,
            "quiet after beat {beat}"
        );
    }
    // The first beat of the bar is higher: more zero crossings.
    let crossings = |s: &[f32]| {
        s.windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count()
    };
    assert!(crossings(&out[0..1000]) > crossings(&out[24_000..25_000]));
}
