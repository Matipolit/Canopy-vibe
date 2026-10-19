//! What the distortion and the waveshaper share: the curves the sound is
//! bent through ([`Mode`], [`bend`]), and the oversampled path it is bent
//! on ([`Oversampler`]): up by halfband doublings, through the curve at
//! the higher rate, and back down, so what the curve adds above the band
//! is filtered away instead of folding back as aliasing.

use std::f32::consts::{FRAC_PI_2, PI};

use crate::halfband::iir::{Design, Halfband};

/// Oversampling choices, as powers of two.
pub(crate) const OVERSAMPLING: &[&str] = &["Off", "2x", "4x", "8x"];
/// Halfband stages: 8x is three doublings.
pub(crate) const STAGES: usize = 3;
/// Most samples one input sample becomes.
const MAX_FACTOR: usize = 1 << STAGES;
/// How much of the band the oversampling keeps, as a fraction of the
/// (original) sample rate: 21.6 kHz at 48 kHz, 19.8 kHz at 44.1 kHz.
const PASS_BAND: f64 = 0.45;
/// How far down the halfbands put what would alias.
const STOP_BAND_DB: f64 = 100.0;
/// Corner of the high-pass that takes out the DC asymmetric curves make.
pub(crate) const DC_HZ: f32 = 10.0;
/// Bit depth of the bit crusher at no drive and at full drive.
pub(crate) const CRUSH_BITS: (f32, f32) = (16.0, 2.0);
/// The tube curve's floor: it saturates at this level downwards (and at 1
/// upwards).
const TUBE_FLOOR: f32 = 0.7;

/// The curves, in the order of the distortion's `mode` labels; the
/// waveshaper offers some of them ([`crate::waveshaper`]'s `CURVES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Saturates softly upwards and harder, lower, downwards: even
    /// harmonics, like an overdriven valve.
    Tube,
    /// `tanh`: rounds the peaks off gently, odd harmonics.
    SoftClip,
    /// Flat at ±1: bright, buzzy.
    HardClip,
    Diode1,
    Diode2,
    /// Reflects off ±1: past the edge the wave folds back on itself.
    LinearFold,
    /// A sine of the input: smooth folds, the classic west-coast timbre.
    SineFold,
    ZeroSquare,
    Downsample,
    Asym,
    /// Turns the negative half up (full-wave rectifier), then rounds the
    /// peaks off: an octave up, fuzzy.
    Rectify,
    SineShaper,
    StompBox,
    TapeSat,
    Overdrive,
    SoftSat,
    /// Rounds to a coarse grid of levels (see [`CRUSH_BITS`]).
    BitCrush,
}

impl Mode {
    pub const ALL: [Mode; 17] = [
        Mode::Tube,
        Mode::SoftClip,
        Mode::HardClip,
        Mode::Diode1,
        Mode::Diode2,
        Mode::LinearFold,
        Mode::SineFold,
        Mode::ZeroSquare,
        Mode::Downsample,
        Mode::Asym,
        Mode::Rectify,
        Mode::SineShaper,
        Mode::StompBox,
        Mode::TapeSat,
        Mode::Overdrive,
        Mode::SoftSat,
        Mode::BitCrush,
    ];

    pub fn from_index(index: f32) -> Mode {
        Self::ALL[(index.round().max(0.0) as usize).min(Self::ALL.len() - 1)]
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&m| m == self).unwrap_or(0)
    }

    /// Whether drive is a gain into the curve, rather than how long to
    /// hold (downsample) or how coarse to round (bit crush).
    pub fn drives(self) -> bool {
        !matches!(self, Mode::Downsample | Mode::BitCrush)
    }
}

/// What `x` becomes through `mode`'s curve (the curve modes; Downsample
/// passes it, for its hold happens elsewhere). `levels` is Bit crush's
/// number of steps from 0 to 1.
#[inline]
pub(crate) fn bend(mode: Mode, x: f32, levels: f32) -> f32 {
    match mode {
        Mode::Tube => {
            if x >= 0.0 {
                1.0 - (-x).exp()
            } else {
                TUBE_FLOOR * (x / TUBE_FLOOR).tanh()
            }
        }
        Mode::SoftClip => x.tanh(),
        Mode::HardClip => x.clamp(-1.0, 1.0),
        Mode::Diode1 => x / (1.0 + x.powi(4)).sqrt().sqrt(),
        Mode::Diode2 => {
            if x >= 0.0 {
                x / (1.0 + x * x).sqrt()
            } else {
                x / (1.0 + 0.25 * x * x).sqrt()
            }
        }
        // A triangle wave of period 4 through (0, 0) and (1, 1).
        Mode::LinearFold => 1.0 - ((x + 1.0).rem_euclid(4.0) - 2.0).abs(),
        Mode::SineFold => (FRAC_PI_2 * x).sin(),
        Mode::ZeroSquare => (x * x).min(1.0).copysign(x),
        Mode::Downsample => x,
        Mode::Asym => {
            if x >= 0.0 {
                x.tanh()
            } else {
                0.5 * (2.0 * x).tanh()
            }
        }
        Mode::Rectify => x.abs().tanh(),
        Mode::SineShaper => (1.5 * PI * x.tanh()).sin(),
        Mode::StompBox => ((3.0 * x + 0.45).tanh() - 0.45f32.tanh()) * 0.85,
        Mode::TapeSat => x / (1.0 + x.abs().powf(1.5)).powf(1.0 / 1.5),
        Mode::Overdrive => std::f32::consts::FRAC_2_PI * (1.6 * x).atan(),
        Mode::SoftSat => {
            let x = (x / 1.5).clamp(-1.0, 1.0);
            1.5 * (x - x * x * x / 3.0)
        }
        Mode::BitCrush => (x.clamp(-1.0, 1.0) * levels).round() / levels,
    }
}

/// Each doubling's halfband: every stage keeps [`PASS_BAND`] of the
/// original rate, so a stage's transition band narrows as its rate rises.
pub(crate) fn designs() -> [Design; STAGES] {
    let transition = |stage: usize| 2.0 * (0.25 - PASS_BAND / f64::from(2u32 << stage));
    [0, 1, 2].map(|stage| Design::new(STOP_BAND_DB, transition(stage)))
}

/// Both channels' halfbands at one oversampling factor.
#[derive(Debug, Clone)]
pub(crate) struct Oversampler {
    /// How many halfband doublings: 0 (off) to [`STAGES`].
    stages: usize,
    up: [[Halfband; STAGES]; 2],
    down: [[Halfband; STAGES]; 2],
}

impl Oversampler {
    pub fn new(stages: usize) -> Oversampler {
        Oversampler {
            stages,
            up: [[Halfband::default(); STAGES]; 2],
            down: [[Halfband::default(); STAGES]; 2],
        }
    }

    /// Samples each input sample becomes.
    pub fn factor(&self) -> usize {
        1 << self.stages
    }

    pub fn clear(&mut self) {
        for halfband in self.up.iter_mut().chain(&mut self.down).flatten() {
            halfband.clear();
        }
    }

    pub fn flush(&mut self) {
        for halfband in self.up.iter_mut().chain(&mut self.down).flatten() {
            halfband.flush();
        }
    }

    /// One sample `x` of `channel` up to the higher rate, each sample there
    /// through `shape`, and back down.
    #[inline(always)]
    pub fn tick(
        &mut self,
        designs: &[Design; STAGES],
        channel: usize,
        x: f32,
        mut shape: impl FnMut(f32) -> f32,
    ) -> f32 {
        let mut buffer = [0.0f32; MAX_FACTOR];
        let mut spare = [0.0f32; MAX_FACTOR];
        buffer[0] = x;
        let mut len = 1;
        for (stage, design) in designs.iter().enumerate().take(self.stages) {
            let filter = &mut self.up[channel][stage];
            for i in 0..len {
                let [a, b] = filter.upsample(design, buffer[i]);
                spare[2 * i] = a;
                spare[2 * i + 1] = b;
            }
            len *= 2;
            std::mem::swap(&mut buffer, &mut spare);
        }
        for sample in &mut buffer[..len] {
            *sample = shape(*sample);
        }
        for (stage, design) in designs.iter().enumerate().take(self.stages).rev() {
            let filter = &mut self.down[channel][stage];
            len /= 2;
            for i in 0..len {
                buffer[i] = filter.downsample(design, buffer[2 * i], buffer[2 * i + 1]);
            }
        }
        buffer[0]
    }
}
