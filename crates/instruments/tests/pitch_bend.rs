//! Every built-in instrument follows the pitch wheel.

use instruments::builtins;
use midi::{NoteEvent, NoteEventKind, Pitch, Velocity};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 64;

/// The pitch, from zero crossings, over `seconds` of output.
fn measure(instrument: &mut dyn instruments::Instrument, seconds: f32) -> f32 {
    let blocks = (seconds * RATE) as usize / BLOCK;
    let mut out = Vec::new();
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    for _ in 0..blocks {
        instrument.process(&[], &mut left, &mut right);
        out.extend_from_slice(&left);
    }
    let crossings = out
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    crossings as f32 / 2.0 / seconds
}

/// The pitched synths that take notes as they come (no samples to load).
const PITCHED: &[&str] = &["canopy.poly_synth", "canopy.simple_osc"];

#[test]
fn a_full_bend_is_two_semitones_and_centering_returns() {
    for descriptor in builtins().iter().filter(|d| PITCHED.contains(&d.id)) {
        let mut instrument = (descriptor.create)();
        instrument.prepare(RATE, BLOCK);
        if let Some(index) = descriptor.params.iter().position(|p| p.id == "waveform") {
            instrument.set_param(index, 0.0); // sine
        }
        let event = |kind| [NoteEvent { frame: 0, kind }];
        let on = NoteEventKind::On {
            pitch: Pitch::A4,
            velocity: Velocity::MAX,
        };
        let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
        instrument.process(&event(on), &mut left, &mut right);
        let plain = measure(&mut *instrument, 0.5);
        assert!((plain - 440.0).abs() < 4.0, "{}: {plain}", descriptor.name);

        instrument.process(
            &event(NoteEventKind::PitchBend(8191)),
            &mut left,
            &mut right,
        );
        let bent = measure(&mut *instrument, 0.5);
        let up = 440.0 * 2f32.powf(2.0 / 12.0);
        assert!(
            (bent - up).abs() < 4.0,
            "{}: {bent} vs {up}",
            descriptor.name
        );

        instrument.process(&event(NoteEventKind::PitchBend(0)), &mut left, &mut right);
        let back = measure(&mut *instrument, 0.5);
        assert!((back - 440.0).abs() < 4.0, "{}: {back}", descriptor.name);
    }
}
