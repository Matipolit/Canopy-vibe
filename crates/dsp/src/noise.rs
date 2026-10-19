//! Noise and randomness for the audio thread: Marsaglia's xorshift32, as
//! cheap as a random number gets, and the two ways the devices here turn
//! its output into a sample.

/// One step of xorshift32 (shifts 13, 17, 5) on `state`: the new state,
/// which is also the output. A state of zero stays zero, so seed it
/// with anything else.
#[inline(always)]
pub fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// The top 24 bits of `bits` as a value in `0..1`: they fit an `f32`
/// exactly, so it never reaches 1.
#[inline(always)]
pub fn unit(bits: u32) -> f32 {
    (bits >> 8) as f32 / (1 << 24) as f32
}

/// The top 24 bits of `bits` as a value in `-1..1`.
#[inline(always)]
pub fn bipolar(bits: u32) -> f32 {
    (bits >> 8) as f32 / (1 << 23) as f32 - 1.0
}

/// Steps a 64-bit xorshift generator (Marsaglia's 13, 7, 17) and returns
/// the new state: for longer sequences than [`xorshift32`]'s, or more bits.
#[inline(always)]
pub fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}
