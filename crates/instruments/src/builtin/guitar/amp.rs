//! A guitar amp and its speaker cabinet: the Guitar's built-in amp, and the
//! Amp effect's sound (`effects::amp` wraps [`AmpSim`] with every control).
//!
//! Like a real amp it is mono: left and right are summed into it. In order:
//!
//! 1. **Input**: a coupling highpass, tighter for the high-gain voicing so
//!    the low strings don't flub up the distortion.
//! 2. **Preamp**, run at four times the sample rate so the harmonics it
//!    makes don't fold back as inharmonic fizz (Pakarinen & Yeh 2009; the
//!    half-band filters are `guitar/oversample.rs`):
//!    - a first triode-like stage, a smooth clipper with an offset, whose
//!      asymmetry adds the even harmonics of a single-ended stage (Mačák &
//!      Schimmel 2011);
//!    - the tone stack, the bass/middle/treble network of a Fender Bassman
//!      or a Marshall JCM800, as Yeh & Smith 2006 derived it (third order,
//!      discretized by the bilinear transform), with its characteristic
//!      mid scoop;
//!    - for crunch and lead, a second, hotter stage.
//! 3. **Power amp**: presence (a treble shelf, as the power amp's feedback
//!    loop gives), then a symmetric push-pull clipper whose drive sags
//!    under load, the compression of a tube rectifier.
//! 4. **Cabinet**: the speaker's resonance (70 to 100 Hz, Zollner 2019;
//!    Yeh, Bank & Karjalainen 2008), the box's low-mid bump, cone
//!    break-up's presence peak, the steep roll-off above about 5 kHz that
//!    makes a guitar speaker sound like one, which a microphone off the
//!    cone's axis hears lower, and a short reflection off the cabinet.
//!
//! The output is levelled so turning the gain up changes the tone more
//! than the loudness.

use std::f32::consts::TAU;

use dsp::{Smoothed, db_to_gain};

use super::oversample::{Downsampler, Upsampler};

/// Labels for the Guitar's amp parameter: off, then the voicings.
pub(super) const MODES: &[&str] = &["Off", "Clean", "Crunch", "Lead"];

/// The voicings, as the Amp effect lists them.
pub const VOICINGS: &[&str] = &["Clean", "Crunch", "Lead"];

/// The cabinets, as the Amp effect lists them.
pub const CABINETS: &[&str] = &["Off", "1x12 open", "2x12 open", "4x12 closed"];

/// How late the output is: the oversampling's half-band filters delay by
/// 21.75 samples, and a quarter-sample interpolation makes it a whole
/// number.
pub const LATENCY: usize = 22;

/// The fraction of a sample added to make the latency whole.
const LATENCY_FRACTION: f32 = 0.25;

/// Input level the makeup gain is set for, a little under a strummed
/// chord's peaks.
const REFERENCE_LEVEL: f32 = 0.25;

/// Ramp for knob changes.
const RAMP_MS: f32 = 30.0;

/// Longest cabinet reflection, seconds.
const MAX_REFLECTION_S: f32 = 0.002;

/// Every setting of the amp, in the Amp effect's units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmpSettings {
    /// Index into [`VOICINGS`].
    pub voicing: usize,
    /// Preamp gain, `0..=1`.
    pub gain: f32,
    /// The tone stack's knobs, `0..=1`.
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    /// Treble boost in the power amp, `0..=1`.
    pub presence: f32,
    /// How much the power amp sags under load, `0..=1`.
    pub sag: f32,
    /// Index into [`CABINETS`].
    pub cabinet: usize,
    /// The microphone, 0 on the cone's axis (bright) to 1 well off it (dark).
    pub mic: f32,
    /// Output level, dB.
    pub output_db: f32,
}

impl AmpSettings {
    pub const DEFAULT: AmpSettings = AmpSettings {
        voicing: 0,
        gain: 0.4,
        bass: 0.5,
        mid: 0.5,
        treble: 0.5,
        presence: 0.4,
        sag: 0.3,
        cabinet: 2,
        mic: 0.3,
        output_db: 0.0,
    };
}

/// A tone stack's parts (Yeh & Smith 2006): capacitors in farads,
/// resistors in ohms (R1 treble, R2 bass, R3 middle pots; R4 the slope
/// resistor).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stack {
    c1: f64,
    c2: f64,
    c3: f64,
    r1: f64,
    r2: f64,
    r3: f64,
    r4: f64,
}

/// A '59 Fender Bassman's.
const BASSMAN: Stack = Stack {
    c1: 250e-12,
    c2: 20e-9,
    c3: 20e-9,
    r1: 250e3,
    r2: 1e6,
    r3: 25e3,
    r4: 56e3,
};

/// A Marshall JCM800's.
const JCM800: Stack = Stack {
    c1: 470e-12,
    c2: 22e-9,
    c3: 22e-9,
    r1: 220e3,
    r2: 1e6,
    r3: 22e3,
    r4: 33e3,
};

/// How a voicing sets up the amp.
struct Voicing {
    /// The input coupling highpass.
    input_hz: f32,
    /// The first stage's gain at gain 0 and 1, and its offset.
    stage1_db: [f32; 2],
    stage1_bias: f32,
    stack: Stack,
    /// The second stage's gain at gain 0 and 1 (none if `None`), its offset,
    /// and the highpass before it.
    stage2_db: Option<[f32; 2]>,
    stage2_bias: f32,
    stage2_hz: f32,
    /// How hard the power amp is driven.
    power_drive: f32,
}

const VOICING_SETUPS: [Voicing; 3] = [
    // Clean: a Fender-style amp that breaks up only when pushed.
    Voicing {
        input_hz: 40.0,
        stage1_db: [-2.0, 16.0],
        stage1_bias: 0.06,
        stack: BASSMAN,
        stage2_db: None,
        stage2_bias: 0.0,
        stage2_hz: 20.0,
        power_drive: 0.7,
    },
    // Crunch: a Marshall-style amp, chords still readable.
    Voicing {
        input_hz: 70.0,
        stage1_db: [8.0, 24.0],
        stage1_bias: 0.12,
        stack: JCM800,
        stage2_db: Some([2.0, 12.0]),
        stage2_bias: 0.1,
        stage2_hz: 60.0,
        power_drive: 1.2,
    },
    // Lead: saturated and singing, tight lows.
    Voicing {
        input_hz: 130.0,
        stage1_db: [16.0, 32.0],
        stage1_bias: 0.2,
        stack: JCM800,
        stage2_db: Some([10.0, 22.0]),
        stage2_bias: 0.15,
        stage2_hz: 110.0,
        power_drive: 1.5,
    },
];

/// `tanh` within 0.3% for `|x| < 3`, and exactly ±1 beyond, where its slope
/// reaches 0 too, so the curve stays smooth.
#[inline]
fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

/// A triode-like stage: a smooth clipper with an offset, re-centred.
#[inline]
fn stage(x: f32, bias: f32, offset: f32) -> f32 {
    soft_clip(x + bias) - offset
}

/// The tone stack's transfer function at the knobs (bass `l` on a log pot,
/// middle `m`, treble `t`), as analog coefficients `b1..b3`, `a0..a3`.
fn stack_analog(s: &Stack, l: f64, m: f64, t: f64) -> ([f64; 3], [f64; 4]) {
    let Stack {
        c1,
        c2,
        c3,
        r1,
        r2,
        r3,
        r4,
    } = *s;
    let b1 = t * c1 * r1 + m * c3 * r3 + l * (c1 * r2 + c2 * r2) + (c1 * r3 + c2 * r3);
    let b2 = t * (c1 * c2 * r1 * r4 + c1 * c3 * r1 * r4)
        - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + m * (c1 * c3 * r1 * r3 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + l * (c1 * c2 * r1 * r2 + c1 * c2 * r2 * r4 + c1 * c3 * r2 * r4)
        + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3)
        + (c1 * c2 * r1 * r3 + c1 * c2 * r3 * r4 + c1 * c3 * r3 * r4);
    let b3 = l * m * (c1 * c2 * c3 * r1 * r2 * r3 + c1 * c2 * c3 * r2 * r3 * r4)
        - m * m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
        + m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
        + t * c1 * c2 * c3 * r1 * r3 * r4
        - t * m * c1 * c2 * c3 * r1 * r3 * r4
        + t * l * c1 * c2 * c3 * r1 * r2 * r4;
    let a0 = 1.0;
    let a1 =
        (c1 * r1 + c1 * r3 + c2 * r3 + c2 * r4 + c3 * r4) + m * c3 * r3 + l * (c1 * r2 + c2 * r2);
    let a2 = m * (c1 * c3 * r1 * r3 - c2 * c3 * r3 * r4 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3)
        - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + l * (c1 * c2 * r1 * r2 + c1 * c2 * r2 * r4 + c1 * c3 * r2 * r4 + c2 * c3 * r2 * r4)
        + (c1 * c2 * r1 * r4
            + c1 * c3 * r1 * r4
            + c1 * c2 * r3 * r4
            + c1 * c2 * r1 * r3
            + c1 * c3 * r3 * r4
            + c2 * c3 * r3 * r4);
    let a3 = l * m * (c1 * c2 * c3 * r1 * r2 * r3 + c1 * c2 * c3 * r2 * r3 * r4)
        - m * m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
        + m * (c1 * c2 * c3 * r3 * r3 * r4 + c1 * c2 * c3 * r1 * r3 * r3
            - c1 * c2 * c3 * r1 * r3 * r4)
        + l * c1 * c2 * c3 * r1 * r2 * r4
        + c1 * c2 * c3 * r1 * r3 * r4;
    ([b1, b2, b3], [a0, a1, a2, a3])
}

/// The bass knob turns a logarithmic pot.
fn bass_taper(knob: f32) -> f64 {
    let knob = f64::from(knob.clamp(0.0, 1.0));
    (knob * knob).max(1e-4)
}

/// The tone stack discretized at `rate` by the bilinear transform:
/// numerator and denominator, `a[0]` normalized to 1.
fn stack_digital(s: &Stack, bass: f32, mid: f32, treble: f32, rate: f32) -> ([f64; 4], [f64; 4]) {
    let ([b1, b2, b3], [a0, a1, a2, a3]) = stack_analog(
        s,
        bass_taper(bass),
        f64::from(mid.clamp(0.0, 1.0)),
        f64::from(treble.clamp(0.0, 1.0)),
    );
    let c = 2.0 * f64::from(rate);
    let (c2, c3) = (c * c, c * c * c);
    let b = [
        -b1 * c - b2 * c2 - b3 * c3,
        -b1 * c + b2 * c2 + 3.0 * b3 * c3,
        b1 * c + b2 * c2 - 3.0 * b3 * c3,
        b1 * c - b2 * c2 + b3 * c3,
    ];
    let a = [
        -a0 - a1 * c - a2 * c2 - a3 * c3,
        -3.0 * a0 - a1 * c + a2 * c2 + 3.0 * a3 * c3,
        -3.0 * a0 + a1 * c + a2 * c2 - 3.0 * a3 * c3,
        -a0 + a1 * c - a2 * c2 + a3 * c3,
    ];
    let norm = 1.0 / a[0];
    (b.map(|v| v * norm), a.map(|v| v * norm))
}

/// Magnitude of a digital filter `b / a` at `w` radians per sample.
fn magnitude(b: &[f64], a: &[f64], w: f64) -> f64 {
    let eval = |c: &[f64]| {
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in c.iter().enumerate() {
            re += v * (w * k as f64).cos();
            im -= v * (w * k as f64).sin();
        }
        (re * re + im * im).sqrt()
    };
    eval(b) / eval(a).max(1e-30)
}

/// A third-order filter, transposed direct form II, in double precision
/// (the tone stack's poles sit close together at four times the rate).
#[derive(Debug, Clone, Default)]
struct ThirdOrder {
    b: [f64; 4],
    a: [f64; 4],
    s: [f64; 3],
}

impl ThirdOrder {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let x = f64::from(x);
        let y = self.b[0] * x + self.s[0];
        self.s[0] = self.b[1] * x - self.a[1] * y + self.s[1];
        self.s[1] = self.b[2] * x - self.a[2] * y + self.s[2];
        self.s[2] = self.b[3] * x - self.a[3] * y;
        y as f32
    }

    fn reset(&mut self) {
        self.s = [0.0; 3];
    }
}

/// A trapezoidal state-variable filter with mixed outputs (Simper 2013):
/// lowpass, highpass, bell or high shelf. Unlike a direct-form biquad its
/// coefficients can move every sample, however fast, without clicks or
/// instability, which a modulated knob needs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Tpt {
    g: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    m0: f32,
    m1: f32,
    m2: f32,
    ic1: f32,
    ic2: f32,
}

impl Tpt {
    fn design(g: f32, k: f32, m: [f32; 3]) -> Self {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self {
            g,
            k,
            a1,
            a2,
            a3: g * a2,
            m0: m[0],
            m1: m[1],
            m2: m[2],
            ic1: 0.0,
            ic2: 0.0,
        }
    }

    fn warp(hz: f32, rate: f32) -> f32 {
        (std::f32::consts::PI * hz.min(0.45 * rate) / rate).tan()
    }

    pub fn lowpass(hz: f32, q: f32, rate: f32) -> Self {
        Self::design(Self::warp(hz, rate), 1.0 / q, [0.0, 0.0, 1.0])
    }

    pub fn highpass(hz: f32, q: f32, rate: f32) -> Self {
        let k = 1.0 / q;
        Self::design(Self::warp(hz, rate), k, [1.0, -k, -1.0])
    }

    /// A bell `db` high (or deep) at `hz`.
    pub fn bell(hz: f32, q: f32, db: f32, rate: f32) -> Self {
        let a = 10f32.powf(db / 40.0);
        let k = 1.0 / (q * a);
        Self::design(Self::warp(hz, rate), k, [1.0, k * (a * a - 1.0), 0.0])
    }

    /// A shelf lifting (or cutting) everything above `hz` by `db`.
    pub fn high_shelf(hz: f32, db: f32, rate: f32) -> Self {
        let a = 10f32.powf(db / 40.0);
        let k = std::f32::consts::SQRT_2;
        Self::design(
            Self::warp(hz, rate) * a.sqrt(),
            k,
            [a * a, k * (1.0 - a) * a, 1.0 - a * a],
        )
    }

    /// Takes `other`'s tuning, keeping this filter's state.
    pub fn retune(&mut self, other: Tpt) {
        let (ic1, ic2) = (self.ic1, self.ic2);
        *self = other;
        self.ic1 = ic1;
        self.ic2 = ic2;
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        if self.ic1.abs() < 1e-20 && self.ic2.abs() < 1e-20 {
            self.ic1 = 0.0;
            self.ic2 = 0.0;
        }
        self.m0 * x + self.m1 * v1 + self.m2 * v2
    }

    /// Gain at `w` radians per sample: the analog prototype at the
    /// frequency the trapezoidal rule maps `w` to.
    pub fn gain(&self, w: f32) -> f32 {
        let s = (0.5 * w).tan() / self.g;
        // D = 1 - s² + j k s; H = m0 + (m1 j s + m2) / D
        let (dr, di) = (1.0 - s * s, self.k * s);
        let d = dr * dr + di * di;
        let (nr, ni) = (self.m2, self.m1 * s);
        let re = self.m0 + (nr * dr + ni * di) / d;
        let im = (ni * dr - nr * di) / d;
        (re * re + im * im).sqrt()
    }
}

/// A cabinet's filters, at the base rate.
#[derive(Debug, Clone, Copy)]
struct CabinetShape {
    /// Speaker resonance (highpass with a bump): Hz and Q.
    resonance: (f32, f32),
    /// The box's low-mid bump: Hz, Q, dB.
    body: (f32, f32, f32),
    /// Cone break-up's presence peak on axis: Hz, Q, dB.
    breakup: (f32, f32, f32),
    /// Where it rolls off on axis and well off it.
    rolloff_hz: [f32; 2],
    /// The reflection off the cabinet: delay in seconds, level.
    reflection: (f32, f32),
}

const CABINET_SHAPES: [CabinetShape; 3] = [
    // 1x12 open back: lean lows, airy.
    CabinetShape {
        resonance: (95.0, 0.9),
        body: (210.0, 1.0, 1.5),
        breakup: (2400.0, 1.6, 4.0),
        rolloff_hz: [5200.0, 3200.0],
        reflection: (0.0011, 0.12),
    },
    // 2x12 open back.
    CabinetShape {
        resonance: (85.0, 1.1),
        body: (180.0, 1.0, 2.0),
        breakup: (2200.0, 1.5, 4.5),
        rolloff_hz: [5000.0, 3000.0],
        reflection: (0.0009, 0.15),
    },
    // 4x12 closed back: a tight thump and a darker top.
    CabinetShape {
        resonance: (75.0, 1.6),
        body: (150.0, 1.2, 3.0),
        breakup: (2000.0, 1.4, 5.0),
        rolloff_hz: [4600.0, 2700.0],
        reflection: (0.0006, 0.18),
    },
];

/// The cabinet's five filters for `shape` heard by a microphone at `mic`.
fn cabinet_filters(shape: &CabinetShape, mic: f32, rate: f32) -> [Tpt; 5] {
    let rolloff = shape.rolloff_hz[0] * (shape.rolloff_hz[1] / shape.rolloff_hz[0]).powf(mic);
    let (hz, q) = shape.resonance;
    let (body_hz, body_q, body_db) = shape.body;
    let (peak_hz, peak_q, peak_db) = shape.breakup;
    [
        Tpt::highpass(hz, q, rate),
        Tpt::bell(body_hz, body_q, body_db, rate),
        Tpt::bell(peak_hz, peak_q, peak_db * (1.0 - 0.7 * mic), rate),
        Tpt::lowpass(rolloff, 0.6, rate),
        Tpt::lowpass(rolloff * 1.15, 1.1, rate),
    ]
}

/// A value gliding linearly to its target in a given number of control
/// ticks: the knob's pace, or a modulator's (within one engine block).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Glide {
    current: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Glide {
    fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            left: 0,
        }
    }

    fn set(&mut self, target: f32, ticks: u32) {
        if target == self.target && self.left == 0 {
            return;
        }
        self.target = target;
        self.left = ticks.max(1);
        self.step = (target - self.current) / self.left as f32;
    }

    fn jump(&mut self) {
        self.current = self.target;
        self.left = 0;
    }

    fn tick(&mut self) {
        if self.left > 0 {
            self.left -= 1;
            self.current = if self.left == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
    }

    fn moving(&self) -> bool {
        self.left > 0
    }
}

/// Samples per control tick while knobs move: the filters and gains are
/// worked out again this often.
const TICK: usize = 8;

/// A modulator's value lands within this many samples (an engine block).
const QUICK_SAMPLES: usize = 64;

/// An amp and cabinet. See the module documentation.
pub struct AmpSim {
    sample_rate: f32,
    settings: AmpSettings,
    /// Gain, bass, middle, treble, presence, mic, output gain.
    glides: [Glide; 7],
    /// Ticks for a knob's glide.
    knob_ticks: u32,
    input_highpass: Tpt,
    up: [Upsampler; 2],
    down: [Downsampler; 2],
    stage1_gain: f32,
    stage1_offset: f32,
    stack: ThirdOrder,
    stage2_state: f32,
    stage2_gain: f32,
    stage2_offset: f32,
    presence_shelf: Tpt,
    power_drive: f32,
    sag: f32,
    sag_envelope: f32,
    sag_attack: f32,
    sag_release: f32,
    makeup: f32,
    /// The last sample, for the fraction of a sample that makes the
    /// latency whole.
    previous: f32,
    dc: (f32, f32),
    dc_coefficient: f32,
    cabinet: [Tpt; 5],
    reflection: Vec<f32>,
    reflection_write: usize,
    reflection_delay: usize,
    reflection_gain: f32,
    /// Levels for meters: input and output peaks, how much the sag holds
    /// the power amp back (dB).
    input_peak: f32,
    output_peak: f32,
    sag_db: f32,
}

impl Default for AmpSim {
    fn default() -> Self {
        Self::new()
    }
}

const GAIN: usize = 0;
const BASS: usize = 1;
const MID: usize = 2;
const TREBLE: usize = 3;
const PRESENCE: usize = 4;
const MIC: usize = 5;
const OUTPUT: usize = 6;

impl AmpSim {
    pub fn new() -> Self {
        let defaults = AmpSettings::DEFAULT;
        let mut amp = Self {
            sample_rate: 48_000.0,
            settings: defaults,
            glides: [
                Glide::new(defaults.gain),
                Glide::new(defaults.bass),
                Glide::new(defaults.mid),
                Glide::new(defaults.treble),
                Glide::new(defaults.presence),
                Glide::new(defaults.mic),
                Glide::new(1.0),
            ],
            knob_ticks: 1,
            input_highpass: Tpt::default(),
            up: [Upsampler::new(), Upsampler::new()],
            down: [Downsampler::new(), Downsampler::new()],
            stage1_gain: 1.0,
            stage1_offset: 0.0,
            stack: ThirdOrder::default(),
            stage2_state: 0.0,
            stage2_gain: 1.0,
            stage2_offset: 0.0,
            presence_shelf: Tpt::default(),
            power_drive: 1.0,
            sag: 0.0,
            sag_envelope: 0.0,
            sag_attack: 0.0,
            sag_release: 0.0,
            makeup: 1.0,
            previous: 0.0,
            dc: (0.0, 0.0),
            dc_coefficient: 0.999,
            cabinet: [Tpt::default(); 5],
            reflection: Vec::new(),
            reflection_write: 0,
            reflection_delay: 1,
            reflection_gain: 0.0,
            input_peak: 0.0,
            output_peak: 0.0,
            sag_db: 0.0,
        };
        amp.prepare(48_000.0);
        amp
    }

    /// Allocates the reflection's line; call before the audio thread.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.knob_ticks = ((RAMP_MS * 0.001 * sample_rate / TICK as f32).round() as u32).max(1);
        let high_rate = 4.0 * sample_rate;
        self.sag_attack = 1.0 - (-1.0 / (0.01 * high_rate)).exp();
        self.sag_release = 1.0 - (-1.0 / (0.25 * high_rate)).exp();
        self.dc_coefficient = 1.0 - TAU * 8.0 / sample_rate;
        self.reflection = vec![0.0; (MAX_REFLECTION_S * sample_rate) as usize + 2];
        self.reset();
    }

    /// Clears everything ringing and jumps to the current settings.
    pub fn reset(&mut self) {
        for glide in &mut self.glides {
            glide.jump();
        }
        self.input_highpass.reset();
        for up in &mut self.up {
            up.reset();
        }
        for down in &mut self.down {
            down.reset();
        }
        self.stack.reset();
        self.stage2_state = 0.0;
        self.presence_shelf.reset();
        self.sag_envelope = 0.0;
        self.previous = 0.0;
        self.dc = (0.0, 0.0);
        for filter in &mut self.cabinet {
            filter.reset();
        }
        self.reflection.fill(0.0);
        self.reflection_write = 0;
        self.input_peak = 0.0;
        self.output_peak = 0.0;
        self.sag_db = 0.0;
        self.update();
    }

    /// New settings: knobs glide there at a knob's pace, or within an
    /// engine block if `quick` (a modulator's). The voicing and cabinet
    /// switch at once; [`SwitchingAmp`] crossfades them.
    pub fn set(&mut self, settings: &AmpSettings, quick: bool) {
        let switched = settings.voicing != self.settings.voicing
            || settings.cabinet != self.settings.cabinet
            || settings.sag != self.settings.sag;
        self.settings = *settings;
        let ticks = if quick {
            (QUICK_SAMPLES / TICK) as u32
        } else {
            self.knob_ticks
        };
        let targets = [
            settings.gain,
            settings.bass,
            settings.mid,
            settings.treble,
            settings.presence,
            settings.mic,
        ];
        for (glide, target) in self.glides.iter_mut().zip(targets) {
            glide.set(target.clamp(0.0, 1.0), ticks);
        }
        self.glides[OUTPUT].set(db_to_gain(settings.output_db), ticks);
        if switched {
            self.update();
        }
    }

    pub fn settings(&self) -> &AmpSettings {
        &self.settings
    }

    fn voicing(&self) -> &'static Voicing {
        &VOICING_SETUPS[self.settings.voicing.min(VOICING_SETUPS.len() - 1)]
    }

    /// Roughly how much the amp amplifies a quiet signal, for deciding
    /// when a fading string can no longer be heard.
    pub fn quiet_gain(&self) -> f32 {
        (self.stage1_gain * self.stage2_gain * self.power_drive * self.makeup).max(1.0)
    }

    /// Input peak and output peak (linear) and the sag's gain reduction
    /// (dB) since the last call.
    pub fn take_meters(&mut self) -> [f32; 3] {
        let out = [self.input_peak, self.output_peak, self.sag_db];
        self.input_peak = 0.0;
        self.output_peak = 0.0;
        out
    }

    /// Recomputes every filter and gain from the gliding knobs.
    fn update(&mut self) {
        let rate = self.sample_rate;
        let high_rate = 4.0 * rate;
        let voicing = self.voicing();
        let value = |i: usize| self.glides[i].current;
        let gain = value(GAIN);
        let lerp = |[a, b]: [f32; 2]| db_to_gain(a + (b - a) * gain);
        self.input_highpass
            .retune(Tpt::highpass(voicing.input_hz, 0.6, rate));
        self.stage1_gain = lerp(voicing.stage1_db);
        self.stage1_offset = soft_clip(voicing.stage1_bias);
        let (b, a) = stack_digital(
            &voicing.stack,
            value(BASS),
            value(MID),
            value(TREBLE),
            high_rate,
        );
        self.stack.b = b;
        self.stack.a = a;
        self.stage2_gain = voicing.stage2_db.map_or(1.0, lerp);
        self.stage2_offset = soft_clip(voicing.stage2_bias);
        self.presence_shelf
            .retune(Tpt::high_shelf(3500.0, 9.0 * value(PRESENCE), high_rate));
        self.power_drive = voicing.power_drive * (0.6 + 0.8 * gain);
        self.sag = self.settings.sag.clamp(0.0, 1.0);
        // Makeup: the chain's output for a steady input at the reference
        // level, through the stack's gain in the middle of the band.
        let stack_gain = magnitude(&b, &a, f64::from(TAU * 700.0 / high_rate)) as f32;
        let mut level = stage(REFERENCE_LEVEL * self.stage1_gain, voicing.stage1_bias, 0.0)
            .abs()
            .max(1e-6)
            * stack_gain;
        if voicing.stage2_db.is_some() {
            level = soft_clip(level * self.stage2_gain);
        }
        let out = soft_clip(level * self.power_drive);
        self.makeup = REFERENCE_LEVEL / out.max(1e-6);
        let cabinet = self.settings.cabinet.min(CABINETS.len() - 1);
        if cabinet > 0 {
            let shape = &CABINET_SHAPES[cabinet - 1];
            let filters = cabinet_filters(shape, value(MIC), rate);
            for (filter, new) in self.cabinet.iter_mut().zip(filters) {
                filter.retune(new);
            }
            let (delay, level) = shape.reflection;
            self.reflection_delay = ((delay * rate) as usize).clamp(1, self.reflection.len() - 1);
            self.reflection_gain = level;
        }
    }

    /// Runs `signal` (mono) through the amp in place.
    pub fn process_mono(&mut self, signal: &mut [f32]) {
        let mut start = 0;
        while start < signal.len() {
            let moving = self.glides.iter().any(Glide::moving);
            let end = if moving {
                for glide in &mut self.glides {
                    glide.tick();
                }
                self.update();
                (start + TICK).min(signal.len())
            } else {
                signal.len()
            };
            self.process_span(&mut signal[start..end]);
            start = end;
        }
    }

    /// Sums `left` and `right` into the amp and writes its output to both.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter()) {
            *l = 0.5 * (*l + *r);
        }
        self.process_mono(left);
        right.copy_from_slice(left);
    }

    fn process_span(&mut self, signal: &mut [f32]) {
        let voicing = self.voicing();
        let (bias1, bias2) = (voicing.stage1_bias, voicing.stage2_bias);
        let second = voicing.stage2_db.is_some();
        let stage2_coefficient = (-TAU * voicing.stage2_hz / (4.0 * self.sample_rate)).exp();
        let cabinet = self.settings.cabinet.min(CABINETS.len() - 1) > 0;
        let output = self.glides[OUTPUT].current * self.makeup;
        let mut sag_peak = 1.0f32;
        for x in signal.iter_mut() {
            self.input_peak = self.input_peak.max(x.abs());
            let input = self.input_highpass.process(*x) * self.stage1_gain;
            // Up to four times the rate, through the nonlinear stages and
            // back down.
            let [a, b] = self.up[0].process(input);
            let [a0, a1] = self.up[1].process(a);
            let [b0, b1] = self.up[1].process(b);
            let mut shaped = [a0, a1, b0, b1];
            for y in &mut shaped {
                let mut v = stage(*y, bias1, self.stage1_offset);
                v = self.stack.process(v);
                if second {
                    // A coupling capacitor before the second stage.
                    self.stage2_state = v + (self.stage2_state - v) * stage2_coefficient;
                    v = stage(
                        (v - self.stage2_state) * self.stage2_gain,
                        bias2,
                        self.stage2_offset,
                    );
                }
                v = self.presence_shelf.process(v);
                // The power amp sags as it works harder.
                let level = v.abs() * self.power_drive;
                let speed = if level > self.sag_envelope {
                    self.sag_attack
                } else {
                    self.sag_release
                };
                self.sag_envelope += (level - self.sag_envelope) * speed;
                let sag = 1.0 / (1.0 + self.sag * 1.5 * self.sag_envelope);
                sag_peak = sag_peak.min(sag);
                *y = soft_clip(v * self.power_drive * sag);
            }
            let c = self.down[1].process([shaped[0], shaped[1]]);
            let d = self.down[1].process([shaped[2], shaped[3]]);
            let y = self.down[0].process([c, d]);
            // A quarter of a sample's delay, for a whole-sample latency.
            let delayed = y + (self.previous - y) * LATENCY_FRACTION;
            self.previous = y;
            let (x1, y1) = &mut self.dc;
            let blocked = delayed - *x1 + self.dc_coefficient * *y1;
            *x1 = delayed;
            *y1 = if blocked.abs() < 1e-20 { 0.0 } else { blocked };
            let mut out = blocked * output;
            if cabinet {
                for filter in &mut self.cabinet {
                    out = filter.process(out);
                }
                let len = self.reflection.len();
                let read = (self.reflection_write + len - self.reflection_delay) % len;
                let echo = self.reflection[read];
                self.reflection[self.reflection_write] = out;
                self.reflection_write = (self.reflection_write + 1) % len;
                out += echo * self.reflection_gain;
            }
            self.output_peak = self.output_peak.max(out.abs());
            *x = out;
        }
        if self.stage2_state.abs() < 1e-20 {
            self.stage2_state = 0.0;
        }
        if self.sag_envelope < 1e-20 {
            self.sag_envelope = 0.0;
        }
        self.sag_db = -20.0 * sag_peak.max(1e-6).log10();
    }

    /// The amp's linear filtering at `hz` (tone stack, presence and
    /// cabinet), in dB, for drawing; the distortion is left out.
    pub fn response_db(settings: &AmpSettings, sample_rate: f32, hz: f32) -> f32 {
        let voicing = &VOICING_SETUPS[settings.voicing.min(VOICING_SETUPS.len() - 1)];
        let high_rate = 4.0 * sample_rate;
        let (b, a) = stack_digital(
            &voicing.stack,
            settings.bass,
            settings.mid,
            settings.treble,
            high_rate,
        );
        let w_high = TAU * hz / high_rate;
        let mut gain = magnitude(&b, &a, f64::from(w_high)) as f32;
        gain *= Tpt::high_shelf(3500.0, 9.0 * settings.presence, high_rate).gain(w_high);
        let cabinet = settings.cabinet.min(CABINETS.len() - 1);
        if cabinet > 0 {
            let w = TAU * hz.min(0.49 * sample_rate) / sample_rate;
            for filter in cabinet_filters(&CABINET_SHAPES[cabinet - 1], settings.mic, sample_rate) {
                gain *= filter.gain(w);
            }
        }
        20.0 * gain.max(1e-9).log10()
    }
}

/// How long a change of voicing or cabinet crossfades.
const SWITCH_FADE_MS: f32 = 15.0;

/// Samples processed at a time while crossfading.
const FADE_CHUNK: usize = 64;

/// An [`AmpSim`] whose voicing and cabinet crossfade when switched: a
/// second amp starts on the new ones while the first fades out. A switch
/// asked for while one is fading waits for it to end.
pub struct SwitchingAmp {
    amps: [AmpSim; 2],
    active: usize,
    /// Share of the way from the old amp to the new, while switching.
    fade: f32,
    fade_step: f32,
    waiting: Option<(AmpSettings, bool)>,
    /// Nothing has been processed since the last reset: settings apply at
    /// once, so an amp built from a project starts the way it was saved.
    fresh: bool,
}

impl Default for SwitchingAmp {
    fn default() -> Self {
        Self::new()
    }
}

impl SwitchingAmp {
    pub fn new() -> Self {
        Self {
            amps: [AmpSim::new(), AmpSim::new()],
            active: 0,
            fade: 1.0,
            fade_step: 0.0,
            waiting: None,
            fresh: true,
        }
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        for amp in &mut self.amps {
            amp.prepare(sample_rate);
        }
        self.fade_step = 1.0 / (SWITCH_FADE_MS * 0.001 * sample_rate);
        self.reset();
    }

    pub fn reset(&mut self) {
        if let Some((settings, _)) = self.waiting.take() {
            self.amps[self.active].set(&settings, false);
        }
        self.amps[self.active].reset();
        self.fade = 1.0;
        self.fresh = true;
    }

    pub fn settings(&self) -> &AmpSettings {
        self.amps[self.active].settings()
    }

    pub fn latency(&self) -> usize {
        LATENCY
    }

    /// New settings (see [`AmpSim::set`]).
    pub fn set(&mut self, settings: &AmpSettings, quick: bool) {
        if self.fresh {
            self.amps[self.active].set(settings, quick);
            self.amps[self.active].reset();
            return;
        }
        let current = *self.amps[self.active].settings();
        let switched = settings.voicing != current.voicing || settings.cabinet != current.cabinet;
        if self.fade < 1.0 {
            if switched || self.waiting.is_some() {
                self.waiting = Some((*settings, quick));
            } else {
                self.amps[self.active].set(settings, quick);
            }
            return;
        }
        if switched {
            let next = 1 - self.active;
            self.amps[next].set(settings, quick);
            self.amps[next].reset();
            self.active = next;
            self.fade = 0.0;
        } else {
            self.amps[self.active].set(settings, quick);
        }
    }

    pub fn quiet_gain(&self) -> f32 {
        self.amps[self.active].quiet_gain()
    }

    pub fn take_meters(&mut self) -> [f32; 3] {
        let other = self.amps[1 - self.active].take_meters();
        let mut meters = self.amps[self.active].take_meters();
        if self.fade < 1.0 {
            meters[0] = meters[0].max(other[0]);
            meters[1] = meters[1].max(other[1]);
        }
        meters
    }

    /// Runs `signal` (mono) through the amp in place.
    pub fn process_mono(&mut self, signal: &mut [f32]) {
        self.fresh &= signal.is_empty();
        let mut start = 0;
        while start < signal.len() {
            if self.fade >= 1.0 {
                if let Some((settings, quick)) = self.waiting.take() {
                    self.set(&settings, quick);
                    continue;
                }
                self.amps[self.active].process_mono(&mut signal[start..]);
                return;
            }
            let end = (start + FADE_CHUNK).min(signal.len());
            let mut old = [0.0f32; FADE_CHUNK];
            let chunk = &mut signal[start..end];
            old[..chunk.len()].copy_from_slice(chunk);
            self.amps[1 - self.active].process_mono(&mut old[..chunk.len()]);
            self.amps[self.active].process_mono(chunk);
            for (new, &old) in chunk.iter_mut().zip(&old) {
                self.fade = (self.fade + self.fade_step).min(1.0);
                // Equal power: the two amps' sounds are unrelated.
                let angle = 0.5 * std::f32::consts::PI * self.fade;
                *new = *new * angle.sin() + old * angle.cos();
            }
            start = end;
        }
    }

    /// Sums `left` and `right` into the amp and writes its output to both.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter()) {
            *l = 0.5 * (*l + *r);
        }
        self.process_mono(left);
        right.copy_from_slice(left);
    }
}

/// The Guitar's built-in amp: a mode (off or a voicing), drive and tone,
/// crossfading as it is switched in or out.
pub(super) struct Amp {
    amp: SwitchingAmp,
    mode: usize,
    drive: f32,
    tone: f32,
    /// How much of the output is the amp's, 0 (bypassed) to 1.
    wet: Smoothed,
    /// The dry sound, delayed to line up with the amp's.
    dry: Vec<f32>,
    dry_write: usize,
}

/// Crossfade when the amp is switched in or out.
const SWITCH_MS: f32 = 20.0;

/// The built-in amp plays a little under the guitar's own level, so a
/// strummed chord through it stays clear of full scale.
const GUITAR_AMP_TRIM_DB: f32 = -4.0;

impl Amp {
    pub fn new() -> Self {
        let mut amp = Self {
            amp: SwitchingAmp::new(),
            mode: 0,
            drive: 0.5,
            tone: 0.5,
            wet: Smoothed::new(0.0, SWITCH_MS),
            dry: vec![0.0; LATENCY + 1],
            dry_write: 0,
        };
        amp.apply();
        amp
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        self.amp.prepare(sample_rate);
        self.wet.prepare(sample_rate);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.wet.settle();
        self.amp.reset();
        self.dry.fill(0.0);
        self.dry_write = 0;
    }

    /// `0` is off; then clean, crunch, lead.
    pub fn set_mode(&mut self, mode: usize) {
        self.mode = mode.min(MODES.len() - 1);
        self.wet.set_target(if self.mode == 0 { 0.0 } else { 1.0 });
        self.apply();
    }

    pub fn set_drive(&mut self, drive: f32) {
        self.drive = drive;
        self.apply();
    }

    pub fn set_tone(&mut self, tone: f32) {
        self.tone = tone;
        self.apply();
    }

    /// The amp effect's settings for the guitar's three knobs: the tone
    /// tilts the stack and the presence; crunch and lead play through a
    /// closed 4x12, clean through an open 2x12.
    fn apply(&mut self) {
        // Off keeps the last voicing, so switching off fades the same sound out.
        let Some(voicing) = self.mode.checked_sub(1) else {
            return;
        };
        let tone = self.tone;
        self.amp.set(
            &AmpSettings {
                voicing,
                gain: self.drive,
                bass: 0.75 - 0.4 * tone,
                mid: 0.5,
                treble: 0.2 + 0.6 * tone,
                presence: 0.15 + 0.6 * tone,
                sag: 0.3,
                cabinet: if voicing == 0 { 2 } else { 3 },
                mic: 0.3,
                output_db: GUITAR_AMP_TRIM_DB,
            },
            false,
        );
    }

    pub fn is_bypassed(&self) -> bool {
        self.mode == 0 && self.wet.current() == 0.0 && !self.wet.is_smoothing()
    }

    pub fn quiet_gain(&self) -> f32 {
        if self.is_bypassed() {
            1.0
        } else {
            self.amp.quiet_gain()
        }
    }

    /// Runs `left` and `right` through the amp, crossfading while it is
    /// switched in or out. Does nothing while off.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.is_bypassed() {
            return;
        }
        if !self.wet.is_smoothing() && self.wet.current() == 1.0 {
            self.amp.process(left, right);
            return;
        }
        // Switching: the dry sound, delayed as the amp delays, faded
        // against the amp's.
        let len = self.dry.len();
        for (left, right) in left
            .chunks_mut(FADE_CHUNK)
            .zip(right.chunks_mut(FADE_CHUNK))
        {
            let mut wet = [0.0f32; FADE_CHUNK];
            for ((w, l), r) in wet.iter_mut().zip(left.iter()).zip(right.iter()) {
                *w = 0.5 * (*l + *r);
            }
            let wet = &mut wet[..left.len()];
            let mut dry = [0.0f32; FADE_CHUNK];
            for (d, &mono) in dry.iter_mut().zip(wet.iter()) {
                *d = self.dry[(self.dry_write + 1) % len];
                self.dry[self.dry_write] = mono;
                self.dry_write = (self.dry_write + 1) % len;
            }
            self.amp.process_mono(wet);
            for ((l, r), (&w, &d)) in left
                .iter_mut()
                .zip(right.iter_mut())
                .zip(wet.iter().zip(&dry))
            {
                let out = d + (w - d) * self.wet.tick();
                *l = out;
                *r = out;
            }
        }
        if self.is_bypassed() {
            // Faded out: start from silence next time.
            self.amp.reset();
            self.dry.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn amp(voicing: usize, gain: f32) -> AmpSim {
        let mut amp = AmpSim::new();
        amp.prepare(RATE);
        amp.set(
            &AmpSettings {
                voicing,
                gain,
                ..AmpSettings::DEFAULT
            },
            false,
        );
        amp.reset();
        amp
    }

    /// Output for a sine at `hz` and `level`, after the filters settle.
    fn run(amp: &mut AmpSim, hz: f32, level: f32) -> Vec<f32> {
        let n = RATE as usize / 2;
        let mut signal: Vec<f32> = (0..n)
            .map(|i| level * (TAU * hz * i as f32 / RATE).sin())
            .collect();
        for chunk in signal.chunks_mut(256) {
            amp.process_mono(chunk);
        }
        signal.split_off(n / 2)
    }

    /// Level of the partial at `hz` (Hann-windowed DFT bin).
    fn partial(signal: &[f32], hz: f32) -> f32 {
        let n = signal.len();
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &x) in signal.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
            let arg = std::f64::consts::TAU * f64::from(hz) * i as f64 / f64::from(RATE);
            re += f64::from(x) * w * arg.cos();
            im += f64::from(x) * w * arg.sin();
        }
        (4.0 * (re * re + im * im).sqrt() / n as f64) as f32
    }

    #[test]
    fn the_tone_stack_scoops_the_mids_as_published() {
        // Yeh & Smith 2006: a Bassman at noon dips about 12 dB near 700 Hz.
        let settings = AmpSettings {
            cabinet: 0,
            presence: 0.0,
            ..AmpSettings::DEFAULT
        };
        let db = |hz: f32| AmpSim::response_db(&settings, RATE, hz);
        let dip = (300..1500)
            .step_by(10)
            .map(|hz| (db(hz as f32), hz))
            .fold((f32::MAX, 0), |m, v| if v.0 < m.0 { v } else { m });
        let shoulders = db(100.0).max(db(4000.0));
        println!(
            "Bassman dip {:.1} dB at {} Hz, shoulders {:.1} dB",
            dip.0, dip.1, shoulders
        );
        assert!((500..1000).contains(&dip.1), "{dip:?}");
        assert!(shoulders - dip.0 > 8.0, "{dip:?} {shoulders}");
        // More treble lifts the top, more bass the bottom.
        let bright = AmpSettings {
            treble: 1.0,
            ..settings
        };
        assert!(AmpSim::response_db(&bright, RATE, 4000.0) > db(4000.0) + 2.0);
        let (thin, boomy) = (
            AmpSettings {
                bass: 0.0,
                ..settings
            },
            AmpSettings {
                bass: 1.0,
                ..settings
            },
        );
        let range =
            AmpSim::response_db(&boomy, RATE, 80.0) - AmpSim::response_db(&thin, RATE, 80.0);
        assert!(range > 8.0, "{range}");
    }

    #[test]
    fn cabinets_roll_off_above_five_kilohertz_and_off_axis_lower() {
        for cabinet in 1..CABINETS.len() {
            let on = AmpSettings {
                cabinet,
                mic: 0.0,
                ..AmpSettings::DEFAULT
            };
            let off = AmpSettings { mic: 1.0, ..on };
            let db = |s: &AmpSettings, hz: f32| AmpSim::response_db(s, RATE, hz);
            assert!(
                db(&on, 10_000.0) < db(&on, 2000.0) - 18.0,
                "cabinet {cabinet}"
            );
            assert!(
                db(&off, 4000.0) < db(&on, 4000.0) - 4.0,
                "cabinet {cabinet}"
            );
            // The speaker's resonance: lows fall away below it.
            assert!(db(&on, 40.0) < db(&on, 150.0) - 10.0, "cabinet {cabinet}");
        }
    }

    #[test]
    fn more_gain_more_harmonics_at_similar_loudness() {
        let mut levels = Vec::new();
        let mut harmonics = Vec::new();
        for voicing in 0..3 {
            let mut amp = amp(voicing, 0.5);
            let out = run(&mut amp, 220.0, REFERENCE_LEVEL);
            let fundamental = partial(&out, 220.0);
            levels.push(fundamental);
            harmonics.push((partial(&out, 440.0) + partial(&out, 660.0)) / fundamental);
        }
        println!("harmonics over fundamental: {harmonics:?}, levels {levels:?}");
        assert!(
            harmonics[0] < harmonics[1] && harmonics[1] < harmonics[2],
            "{harmonics:?}"
        );
        for level in &levels {
            assert!((0.05..1.5).contains(level), "{levels:?}");
        }
    }

    #[test]
    fn heavy_drive_does_not_alias_audibly() {
        // A high note driven hard: its harmonics above Nyquist must not
        // fold back as inharmonic tones in the band the cabinet passes.
        let mut amp = amp(2, 1.0);
        let hz = 1975.0; // B6
        let out = run(&mut amp, hz, REFERENCE_LEVEL);
        let fundamental = partial(&out, hz);
        for k in 23..=26 {
            let alias = (k as f32 * hz - RATE).abs();
            let level = partial(&out, alias) / fundamental;
            assert!(
                level < 3e-3,
                "harmonic {k} aliased to {alias} Hz at {level}"
            );
        }
    }

    #[test]
    fn the_latency_is_what_it_says() {
        // The resampling path the amp's preamp runs through, with the
        // fraction of a sample after it: its impulse response is centred
        // LATENCY samples late.
        let (mut up, mut down) = (
            [Upsampler::new(), Upsampler::new()],
            [Downsampler::new(), Downsampler::new()],
        );
        let mut previous = 0.0;
        let response: Vec<f32> = (0..128)
            .map(|n| {
                let x = if n == 0 { 1.0 } else { 0.0 };
                let [a, b] = up[0].process(x);
                let [a0, a1] = up[1].process(a);
                let [b0, b1] = up[1].process(b);
                let c = down[1].process([a0, a1]);
                let d = down[1].process([b0, b1]);
                let y = down[0].process([c, d]);
                let delayed = y + (previous - y) * LATENCY_FRACTION;
                previous = y;
                delayed
            })
            .collect();
        // Its delay at 500 Hz, from the phase of its response there.
        let w = TAU * 500.0 / RATE;
        let (re, im) = response
            .iter()
            .enumerate()
            .fold((0.0f32, 0.0f32), |(re, im), (n, v)| {
                (re + v * (w * n as f32).cos(), im - v * (w * n as f32).sin())
            });
        let centre = -im.atan2(re) / w;
        assert!((centre - LATENCY as f32).abs() < 0.02, "{centre}");
    }

    #[test]
    fn the_guitars_amp_leaves_the_signal_alone_when_off() {
        let mut amp = Amp::new();
        amp.prepare(RATE);
        let input: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let (mut left, mut right) = (input.clone(), input.clone());
        amp.process(&mut left, &mut right);
        assert_eq!(left, input);
    }
}
