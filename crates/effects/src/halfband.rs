//! Halfband lowpass filters for oversampling by two, in two designs: an
//! IIR polyphase allpass pair ([`iir`]), cheap and nearly free of delay,
//! for the distortions, whose phase near the top doesn't matter; and
//! linear-phase FIRs ([`fir`]), for the mastering clipper and true-peak
//! safety, whose latency must be exact and whose phase must not smear.

pub(crate) mod fir;
pub(crate) mod iir;
