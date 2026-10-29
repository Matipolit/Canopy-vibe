//! Random sessions thrown at the engine: odd clip bounds, notes before 0,
//! audio at another native rate, automation, tempo changes, play/stop/seek
//! and snapshots at random moments, any channel count and buffer size. The
//! output must stay finite and nothing may panic.

use std::sync::Arc;

use engine::{Command, Compiler, create};
use media::AudioBuffer;
use midi::{NoteEventKind, Pitch, Tick, Velocity};
use project::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo) as u64)) as i64
    }
}

fn run(seed: u64) {
    let mut rng = Rng(seed);
    let rate = [44_100u32, 48_000, 96_000][rng.range(0, 3) as usize];
    let (mut handle, mut engine) = create(rate as f32);
    let mut compiler = Compiler::new(rate);
    let mut project = Project::with_default_tracks();
    let ids = project.track_ids();
    let src_rate = [22_050u32, 44_100, 48_000][rng.range(0, 3) as usize];
    let frames = rng.range(1, 200_000) as u64;
    let source = project.add_source("/tmp/x.wav".into(), src_rate, frames, 2);
    let conv = (frames as f64 * rate as f64 / src_rate as f64).round() as usize;
    let data: Vec<f32> = (0..conv).map(|i| ((i % 100) as f32 / 50.0) - 1.0).collect();
    let audio = Arc::new(AudioBuffer::new(rate, data.clone(), data));
    let lookup = |id| (id == source).then(|| audio.clone());
    for step in 0..60 {
        match rng.range(0, 8) {
            0 | 1 => {
                let t = ids[rng.range(0, 2) as usize];
                let start = Tick::new(rng.range(0, 20_000));
                if let Ok(c) = project.add_midi_clip(t, start, Tick::new(rng.range(1, 8000))) {
                    let notes: Vec<_> = (0..rng.range(0, 40))
                        .map(|_| Note {
                            id: project.new_note_id(),
                            start: Tick::new(rng.range(-2000, 8000)),
                            len: Tick::new(rng.range(0, 3000)),
                            pitch: Pitch::new(rng.range(0, 128) as u8).unwrap(),
                            velocity: Velocity::MAX,
                            release_velocity: Velocity::CENTER,
                            muted: false,
                            bend: Vec::new(),
                        })
                        .collect();
                    let _ = project.set_notes(c, notes);
                    let s = Tick::new(rng.range(-3000, 20_000));
                    let _ = project.set_clip_bounds(c, s, s + Tick::new(rng.range(1, 9000)));
                }
            }
            2 => {
                let t = ids[2 + rng.range(0, 2) as usize];
                if let Ok(c) = project.add_audio_clip(t, Tick::new(rng.range(0, 20_000)), source) {
                    let s = Tick::new(rng.range(-3000, 20_000));
                    let _ = project.set_clip_bounds(c, s, s + Tick::new(rng.range(1, 9000)));
                    if rng.range(0, 2) == 0 {
                        let _ = project.move_clips(&[ClipMove {
                            clip: c,
                            track: t,
                            start: Tick::new(rng.range(-5000, 5000)),
                        }]);
                    }
                }
            }
            3 => {
                let t = ids[rng.range(0, 4) as usize];
                let target = match rng.range(0, 3) {
                    0 => AutomationTarget::Volume,
                    1 => AutomationTarget::Pan,
                    _ => AutomationTarget::InstrumentParam("cutoff".into()),
                };
                let pts: Vec<_> = (0..rng.range(0, 6))
                    .map(|_| AutomationPoint {
                        tick: Tick::new(rng.range(-2000, 20_000)),
                        value: rng.range(0, 100) as f32 / 100.0,
                        curve: 0.0,
                    })
                    .collect();
                let _ = project.set_automation(t, target, pts);
            }
            4 => {
                project.set_tempo_bpm(rng.range(20, 999) as f64);
            }
            5 => {
                let _ = handle.send(Command::SetPlaying(rng.range(0, 3) != 0));
            }
            6 => {
                let _ = handle.send(Command::Seek(rng.range(0, 2_000_000) as u64));
            }
            _ => {
                let t = ids[rng.range(0, 4) as usize];
                let p = Pitch::new(rng.range(0, 128) as u8).unwrap();
                let ev = if rng.range(0, 2) == 0 {
                    NoteEventKind::On {
                        pitch: p,
                        velocity: Velocity::MAX,
                    }
                } else {
                    NoteEventKind::Off {
                        pitch: p,
                        velocity: Velocity::MAX,
                    }
                };
                let _ = handle.send(Command::PreviewNote {
                    track: t,
                    event: ev,
                });
            }
        }
        if rng.range(0, 2) == 0 {
            let _ = handle.send(Command::Arrangement(compiler.compile(&project, lookup)));
        }
        let channels = rng.range(1, 4) as usize;
        let n = rng.range(1, 5000) as usize;
        let mut out = vec![0.0f32; n * channels];
        engine.process_interleaved(&mut out, channels);
        assert!(
            out.iter().all(|s| s.is_finite()),
            "seed {seed} step {step}: non-finite output"
        );
        handle.collect_garbage();
    }
}

#[test]
fn engine_fuzz() {
    for seed in 1..400u64 {
        run(seed * 2654435761 + 1);
    }
}
