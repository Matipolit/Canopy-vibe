//! The wavetable synth, set up the way its tests start.

use instruments::Instrument;
use instruments::builtin::wavetable::*;
use midi::NoteEvent;

use super::RATE;

pub const BLOCK: usize = 256;

pub fn default_synth() -> Wavetable {
    let mut synth = Wavetable::new();
    synth.prepare(RATE, BLOCK);
    synth
}

/// A plain sine from osc A alone: no unison, filter, matrix or velocity, so
/// tests start from a known sound.
pub fn plain_sine() -> Wavetable {
    let mut synth = default_synth();
    let a = |offset| osc_param(0, offset);
    synth.set_param(a(OSC_POSITION), 0.0);
    synth.set_param(a(OSC_UNISON), 0.0);
    synth.set_param(a(OSC_RANDOM), 0.0);
    synth.set_param(osc_param(1, OSC_ON), 0.0);
    synth.set_param(FILTER_ON, 0.0);
    synth.set_param(VELOCITY_SENS, 0.0);
    synth.set_param(ENV1.sustain, 1.0);
    for slot in 0..MOD_SLOTS {
        synth.set_param(slot_param(slot, SLOT_SOURCE), 0.0);
    }
    synth.reset();
    synth
}

/// Renders `seconds`, with `events` at the start, and returns both channels.
pub fn render(synth: &mut Wavetable, events: &[NoteEvent], seconds: f32) -> (Vec<f32>, Vec<f32>) {
    let blocks = (seconds * RATE / BLOCK as f32).ceil() as usize;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    for block in 0..blocks {
        synth.process(if block == 0 { events } else { &[] }, &mut l, &mut r);
        assert!(
            l.iter().chain(&r).all(|s| s.is_finite()),
            "non-finite output"
        );
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
    }
    (left, right)
}
