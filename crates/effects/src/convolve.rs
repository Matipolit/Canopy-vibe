//! Convolve: Serum 2's convolution module. The sound is convolved with an
//! impulse response (a room's, a plate's, a guitar cabinet's) chosen from a
//! menu and shaped by Size, Attack, Decay and Damp; then a tone tilt, a
//! gain, a dry/wet mix and an output level.
//!
//! **Impulses** ([`Impulse`]) are made here, procedurally, rather than
//! loaded from files (effects have no way to load data yet, as instruments
//! do): decaying stereo noise in three bands, each dying at its own rate,
//! after early reflections, for the rooms, the hall and the plate; a train
//! of chirps for the spring; for the cabinets and the radio, an impulse
//! through the resonances and roll-offs of a speaker in a box. Size
//! stretches a space's times (half to twice) or, for a box, lowers or
//! raises its resonances; Attack fades the impulse in; Decay shortens it
//! (which also makes it cheaper); Damp makes its highs die sooner than its
//! lows. Every impulse is scaled to the same energy, so changing it
//! doesn't jump the level; IR gain then sets it.
//!
//! **Convolution** is uniformly partitioned in the frequency domain, at
//! three sizes so it is cheap and has no latency: the impulse's first
//! [`DIRECT`] samples directly, as an FIR; up to [`HEAD`] samples in blocks
//! of [`DIRECT`] (FFTs of twice that), each block's sum worked out as its
//! input completes; and the rest in blocks of [`TAIL`], whose sums are due
//! a block later than their input completes, so each is worked out a slice
//! at a time over the block before (Gardner's non-uniform partitioning,
//! time-distributed). Both channels go through one complex FFT (the left as
//! its real part, the right as its imaginary part), and only partitions the
//! impulse reaches are multiplied.
//!
//! **Changing the impulse** (any of Impulse, Size, Attack, Decay or Damp)
//! is too much work for one block: a new impulse is made and transformed in
//! slices, a bounded amount per block, while the old one plays; then the
//! two crossfade over [`FADE_MS`]. A modulator moving them every block just
//! keeps a new impulse coming, one after another, each a fraction of a
//! second behind. Only before the first block after `prepare` (on the UI
//! thread, as a project loads) is an impulse made at once, so an effect
//! starts with the one it was saved with; after a `reset` on the audio
//! thread (coming back from bypass) the one it had plays until the new one
//! is ready. The other parameters (Pre-delay, Tone, IR gain, Mix, Output)
//! glide as everywhere else (see "Modulation" in `lib.rs`).

use std::f32::consts::{PI, TAU};

use instruments::ParamDescriptor;

use crate::line::{Ring, Tap};
use crate::meter::PeakMeter;
use crate::{Category, Context, Effect, EffectDescriptor, MAX_METERS, Values, choice, float};
use dsp::flush;
use dsp::svf::{Coefs, Shape, SvfState};
use dsp::{MAX_BLOCK, Pace, Smoothed};

pub const DESCRIPTOR: EffectDescriptor = EffectDescriptor {
    id: "canopy.convolve",
    name: "Convolve",
    description: "Convolution with built-in rooms, plates, a spring and guitar cabinets",
    category: Category::Space,
    params: PARAMS,
    meters: &[
        "Wet peak (linear, 1 = 0 dBFS)",
        "Impulse length (s)",
        "Making a new impulse (0 or 1)",
    ],
    create: || Box::new(Convolve::new()),
    ..EffectDescriptor::BASE
};

/// The impulses, in the order of the `impulse` parameter's labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Impulse {
    Room,
    Hall,
    Plate,
    Spring,
    Cabinet1x12,
    Cabinet4x12,
    Radio,
    Reverse,
}

impl Impulse {
    pub const ALL: [Impulse; 8] = [
        Impulse::Room,
        Impulse::Hall,
        Impulse::Plate,
        Impulse::Spring,
        Impulse::Cabinet1x12,
        Impulse::Cabinet4x12,
        Impulse::Radio,
        Impulse::Reverse,
    ];

    pub fn from_index(index: f32) -> Impulse {
        Self::ALL[(index.round().max(0.0) as usize).min(Self::ALL.len() - 1)]
    }

    /// Whether it's a box (a cabinet, a radio) rather than a space: Size
    /// moves its resonances rather than its times.
    pub fn is_box(self) -> bool {
        matches!(
            self,
            Impulse::Cabinet1x12 | Impulse::Cabinet4x12 | Impulse::Radio
        )
    }
}

pub const IMPULSES: &[&str] = &[
    "Room",
    "Hall",
    "Plate",
    "Spring",
    "Cabinet 1x12",
    "Cabinet 4x12",
    "Radio",
    "Reverse",
];

const PARAMS: &[ParamDescriptor] = &[
    choice("impulse", "Impulse", IMPULSES, 1),
    float("size", "Size", 0.0, 100.0, 50.0, "%", false),
    float("predelay", "Pre-delay", 0.0, 250.0, 0.0, "ms", false),
    float("attack", "Attack", 0.0, 100.0, 0.0, "%", false),
    float("decay", "Decay", 0.0, 100.0, 100.0, "%", false),
    float("damp", "Damp", 0.0, 100.0, 0.0, "%", false),
    float("tone", "Tone", -100.0, 100.0, 0.0, "%", false),
    float("ir_gain", "IR gain", -24.0, 12.0, 0.0, "dB", false),
    float("mix", "Mix", 0.0, 100.0, 35.0, "%", false),
    float("output", "Output", -24.0, 12.0, 0.0, "dB", false),
];

const COUNT: usize = 10;

pub const IMPULSE: usize = 0;
/// 0 to 100%: a space's times from half to twice, a box's resonances from
/// higher to lower, 50% as made.
pub const SIZE: usize = 1;
pub const PREDELAY: usize = 2;
/// 0 to 100%: fades the impulse in over up to half its length.
pub const ATTACK: usize = 3;
/// 0 to 100%: 100% is the impulse as made; less dies sooner.
pub const DECAY: usize = 4;
/// 0 to 100%: how much sooner the highs die than the lows.
pub const DAMP: usize = 5;
/// -100% (dark) to 100% (bright): a tilt around 1 kHz, ±6 dB at the ends.
pub const TONE: usize = 6;
pub const IR_GAIN: usize = 7;
pub const MIX: usize = 8;
pub const OUTPUT: usize = 9;

/// Meter slots, in [`DESCRIPTOR`]'s `meters` order.
pub const METER_WET: usize = 0;
pub const METER_LENGTH: usize = 1;
pub const METER_MAKING: usize = 2;

/// The longest impulse, in seconds.
pub const MAX_SECONDS: f32 = 4.0;
/// The direct part's length and the head's block, in samples; the head's
/// reach; the tail's block.
pub const DIRECT: usize = 64;
pub const HEAD: usize = 4096;
pub const TAIL: usize = 2048;
/// How long a new impulse crossfades in.
pub const FADE_MS: f32 = 50.0;
/// Work allowed per [`DIRECT`] samples while making an impulse: samples of
/// it made, or head partitions transformed, or tail partitions.
const SYNTH_PER_STEP: usize = 2048;
const HEAD_PER_STEP: usize = 8;
const TAIL_PER_STEP: usize = 1;
/// How long the pre-delay crossfades, and glides at most.
const PREDELAY_FADE_MS: f32 = 20.0;
const MAX_TAP_SLOPE: f32 = 0.5;
/// How long the floats take to reach a new value.
const SMOOTH_MS: f32 = 20.0;
/// The tone's tilt at its ends, in dB either way, and its pivot.
const TONE_DB: f32 = 6.0;
const TONE_HZ: f32 = 1000.0;

/// An in-place radix-2 FFT of one size, on separate real and imaginary
/// arrays (which the compiler vectorizes well).
#[derive(Debug, Clone, Default)]
struct Fft {
    n: usize,
    cos: Vec<f32>,
    sin: Vec<f32>,
    reversed: Vec<u32>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        let bits = n.trailing_zeros();
        Fft {
            n,
            cos: (0..n / 2)
                .map(|k| (std::f64::consts::TAU * k as f64 / n as f64).cos() as f32)
                .collect(),
            sin: (0..n / 2)
                .map(|k| (std::f64::consts::TAU * k as f64 / n as f64).sin() as f32)
                .collect(),
            reversed: (0..n as u32)
                .map(|i| {
                    if bits == 0 {
                        0
                    } else {
                        i.reverse_bits() >> (32 - bits)
                    }
                })
                .collect(),
        }
    }

    /// The transform of `re + j·im`, in place; `inverse` without the 1/n.
    fn transform(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        for i in 0..n {
            let j = self.reversed[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let sign = if inverse { 1.0 } else { -1.0 };
        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let step = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..half {
                    let (c, s) = (self.cos[k * step], sign * self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }
}

/// Splits the transform of `left + j·right` (both real) in `re`/`im` into
/// each one's half spectrum (bins 0 to n/2), into `out`: the left's real
/// and imaginary parts, then the right's, `bins` each.
fn unpack(re: &[f32], im: &[f32], out: &mut [f32], bins: usize) {
    let n = re.len();
    let (left, right) = out.split_at_mut(2 * bins);
    let (left_re, left_im) = left.split_at_mut(bins);
    let (right_re, right_im) = right.split_at_mut(bins);
    for k in 0..bins {
        let mirror = (n - k) % n;
        let (a, b, c, d) = (re[k], im[k], re[mirror], im[mirror]);
        left_re[k] = 0.5 * (a + c);
        left_im[k] = 0.5 * (b - d);
        right_re[k] = 0.5 * (b + d);
        right_im[k] = 0.5 * (c - a);
    }
}

/// The inverse of [`unpack`]: the full spectrum of `left + j·right` from
/// their half spectra.
fn pack(spectra: &[f32], re: &mut [f32], im: &mut [f32], bins: usize) {
    let n = re.len();
    let (left_re, rest) = spectra.split_at(bins);
    let (left_im, rest) = rest.split_at(bins);
    let (right_re, right_im) = rest.split_at(bins);
    for k in 0..bins {
        let (p, q, r, s) = (left_re[k], left_im[k], right_re[k], right_im[k]);
        re[k] = p - s;
        im[k] = q + r;
        if k > 0 && k < n - k {
            re[n - k] = p + s;
            im[n - k] = r - q;
        }
    }
}

/// One size of partitions: the impulse from `offset` on, in partitions of
/// `block` samples, convolved with the input through FFTs of twice that.
#[derive(Debug, Clone, Default)]
struct Level {
    block: usize,
    bins: usize,
    offset: usize,
    /// Partitions room is kept for, and how many each impulse reaches.
    partitions: usize,
    reach: [usize; 2],
    fft: Fft,
    /// Per impulse, each partition's half spectra (as [`unpack`] lays
    /// them out), one after another.
    spectra: [Vec<f32>; 2],
    /// The input's half spectra, a ring of `partitions` of them; `newest`
    /// is the latest's place.
    history: Vec<f32>,
    newest: usize,
    /// The input's last two blocks, per channel, and how far into the
    /// newer one it has come.
    input: [Vec<f32>; 2],
    filled: usize,
    /// Per impulse: the block being heard, per channel, and (for a tail)
    /// the next one's sum being gathered.
    heard: [[Vec<f32>; 2]; 2],
    next: [[Vec<f32>; 2]; 2],
    sums: [Vec<f32>; 2],
    /// A tail's slices: which one comes next, and how many a block has.
    slice: usize,
    slices: usize,
    /// Scratch for the transforms.
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Level {
    fn new(block: usize, offset: usize, reach: usize, slices: usize) -> Level {
        let bins = block + 1;
        let partitions = reach.saturating_sub(offset).div_ceil(block).max(1);
        let half = 4 * bins;
        Level {
            block,
            bins,
            offset,
            partitions,
            reach: [0; 2],
            fft: Fft::new(2 * block),
            spectra: [vec![0.0; partitions * half], vec![0.0; partitions * half]],
            history: vec![0.0; partitions * half],
            newest: 0,
            input: [vec![0.0; 2 * block], vec![0.0; 2 * block]],
            filled: 0,
            heard: std::array::from_fn(|_| [vec![0.0; block], vec![0.0; block]]),
            next: std::array::from_fn(|_| [vec![0.0; block], vec![0.0; block]]),
            sums: [vec![0.0; half], vec![0.0; half]],
            slice: 0,
            slices,
            re: vec![0.0; 2 * block],
            im: vec![0.0; 2 * block],
        }
    }

    fn clear(&mut self) {
        self.history.fill(0.0);
        for buffer in self
            .input
            .iter_mut()
            .chain(self.heard.iter_mut().flatten())
            .chain(self.next.iter_mut().flatten())
            .chain(&mut self.sums)
        {
            buffer.fill(0.0);
        }
        self.filled = 0;
        self.slice = 0;
    }

    /// The impulse's half-spectrum of partition `partition` for impulse
    /// `set`, from its two channels in `ir` (the first `len` samples of
    /// each; what's after is left from an older one), scaled by `gain`.
    fn transform_partition(
        &mut self,
        set: usize,
        partition: usize,
        ir: &[Vec<f32>; 2],
        len: usize,
        gain: f32,
    ) {
        let start = self.offset + partition * self.block;
        for (i, (re, im)) in self.re.iter_mut().zip(&mut self.im).enumerate() {
            let at = start + i;
            let inside = i < self.block && at < len;
            *re = if inside {
                gain * ir[0].get(at).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            *im = if inside {
                gain * ir[1].get(at).copied().unwrap_or(0.0)
            } else {
                0.0
            };
        }
        self.fft.transform(&mut self.re, &mut self.im, false);
        let half = 4 * self.bins;
        let out = &mut self.spectra[set][partition * half..(partition + 1) * half];
        unpack(&self.re, &self.im, out, self.bins);
    }

    /// How many of this level's partitions an impulse `len` samples long
    /// reaches.
    fn reach_of(&self, len: usize) -> usize {
        len.saturating_sub(self.offset)
            .div_ceil(self.block)
            .min(self.partitions)
    }

    /// One sample of input in.
    #[inline]
    fn push(&mut self, left: f32, right: f32) {
        let at = self.block + self.filled;
        self.input[0][at] = left;
        self.input[1][at] = right;
        self.filled += 1;
    }

    /// The newest block of input is complete: its spectrum goes in the
    /// history, and the input slides along a block.
    fn take_block(&mut self) {
        self.re.copy_from_slice(&self.input[0]);
        self.im.copy_from_slice(&self.input[1]);
        self.fft.transform(&mut self.re, &mut self.im, false);
        let half = 4 * self.bins;
        self.newest = (self.newest + 1) % self.partitions;
        let place = self.newest * half;
        unpack(
            &self.re,
            &self.im,
            &mut self.history[place..place + half],
            self.bins,
        );
        for channel in &mut self.input {
            channel.copy_within(self.block.., 0);
        }
        self.filled = 0;
    }

    /// Adds partitions `from..to` of impulse `set` times the matching
    /// history into its sum: partition `j` meets the input `j` blocks
    /// before the newest.
    fn gather(&mut self, set: usize, from: usize, to: usize) {
        let half = 4 * self.bins;
        let bins = self.bins;
        let sum = &mut self.sums[set];
        for j in from..to.min(self.reach[set]) {
            let frame = (self.newest + self.partitions - j) % self.partitions;
            let x = &self.history[frame * half..(frame + 1) * half];
            let h = &self.spectra[set][j * half..(j + 1) * half];
            for channel in 0..2 {
                let base = 2 * bins * channel;
                let (xr, xi) = (&x[base..base + bins], &x[base + bins..base + 2 * bins]);
                let (hr, hi) = (&h[base..base + bins], &h[base + bins..base + 2 * bins]);
                let (sr, rest) = sum[base..base + 2 * bins].split_at_mut(bins);
                for k in 0..bins {
                    sr[k] += xr[k] * hr[k] - xi[k] * hi[k];
                    rest[k] += xr[k] * hi[k] + xi[k] * hr[k];
                }
            }
        }
    }

    /// Turns impulse `set`'s sum into a block of output, into `out`, and
    /// clears the sum.
    fn finish(&mut self, set: usize, next: bool) {
        pack(&self.sums[set], &mut self.re, &mut self.im, self.bins);
        self.fft.transform(&mut self.re, &mut self.im, true);
        let scale = 1.0 / (2 * self.block) as f32;
        let [left, right] = if next {
            &mut self.next[set]
        } else {
            &mut self.heard[set]
        };
        let late = self.re[self.block..].iter().zip(&self.im[self.block..]);
        for ((left, right), (re, im)) in left.iter_mut().zip(right.iter_mut()).zip(late) {
            *left = flush(re * scale);
            *right = flush(im * scale);
        }
        self.sums[set].fill(0.0);
    }
}

/// The shaping every impulse is made with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shaping {
    pub impulse: Impulse,
    /// Size as a stretch, 0.5 to 2.
    pub stretch: f32,
    pub attack: f32,
    pub decay: f32,
    pub damp: f32,
}

impl Shaping {
    /// The shaping at these parameter values (in [`DESCRIPTOR`]'s order;
    /// missing or NaN ones count as defaults).
    pub fn of(values: &[f32]) -> Shaping {
        let value = |index: usize| crate::param_value(PARAMS, values, index);
        Shaping {
            impulse: Impulse::from_index(value(IMPULSE)),
            stretch: ((value(SIZE) - 50.0) / 50.0).exp2(),
            attack: value(ATTACK) / 100.0,
            decay: value(DECAY) / 100.0,
            damp: value(DAMP) / 100.0,
        }
    }

    /// How long the impulse is, in seconds: what it's made for, shortened
    /// by the decay.
    pub fn seconds(&self) -> f32 {
        let natural = match self.impulse {
            Impulse::Room => 1.0 * self.stretch,
            Impulse::Hall => 3.0 * self.stretch,
            Impulse::Plate => 2.5 * self.stretch,
            Impulse::Spring => 2.0 * self.stretch,
            Impulse::Cabinet1x12 => 0.04,
            Impulse::Cabinet4x12 => 0.05,
            Impulse::Radio => 0.06,
            Impulse::Reverse => 2.0 * self.stretch,
        };
        (natural * (0.05 + 0.95 * self.decay * self.decay)).clamp(0.01, MAX_SECONDS)
    }
}

/// The most filters a box's impulse goes through.
const MAX_FILTERS: usize = 6;

/// A state-variable filter for making boxes' impulses.
#[derive(Debug, Clone, Copy, Default)]
struct BoxFilter {
    coefs: Coefs,
    state: SvfState,
}

impl BoxFilter {
    fn new(kind: char, hz: f32, q: f32, db: f32, sample_rate: f32) -> BoxFilter {
        let g = (PI * hz.min(0.45 * sample_rate) / sample_rate).tan();
        let a = dsp::db_to_gain(db * 0.5);
        let (k, [m0, m1, m2]) = match kind {
            // Low-pass, high-pass, bell.
            'l' => (1.0 / q, [0.0, 0.0, 1.0]),
            'h' => (1.0 / q, [1.0, -1.0 / q, -1.0]),
            _ => (1.0 / (q * a), [1.0, (a * a - 1.0) / (q * a), 0.0]),
        };
        BoxFilter {
            coefs: Coefs::new(&Shape { g, k, m0, m1, m2 }),
            state: SvfState::default(),
        }
    }

    fn tick(&mut self, x: f32) -> f32 {
        self.state.tick(&self.coefs, x)
    }
}

/// Makes an impulse a few samples at a time: its state between calls.
#[derive(Debug, Clone)]
struct Maker {
    shaping: Shaping,
    sample_rate: f32,
    len: usize,
    at: usize,
    noise: [u32; 2],
    /// The three bands' splitting filters' memories, per channel.
    bands: [[f32; 2]; 2],
    /// The boxes' filters, per channel (the first `filter_count` of them),
    /// and the impulse's own damping.
    filters: [[BoxFilter; MAX_FILTERS]; 2],
    filter_count: usize,
    damp: [f32; 2],
    energy: f64,
}

/// A space's character: where its tail starts and how fast it builds (s),
/// each band's time to fall 60 dB (low, mid, high, in s), its early
/// reflections (time in s, level, where: -1 left to 1 right), and how loud
/// the reflections are against the tail.
struct Space {
    gap: f32,
    build: f32,
    rt60: [f32; 3],
    early: &'static [(f32, f32, f32)],
}

const ROOM_EARLY: &[(f32, f32, f32)] = &[
    (0.0031, 0.9, -0.6),
    (0.0047, 0.8, 0.7),
    (0.0068, 0.7, -0.2),
    (0.0089, 0.65, 0.5),
    (0.0112, 0.55, -0.8),
    (0.0137, 0.5, 0.3),
    (0.0161, 0.45, -0.4),
    (0.0189, 0.4, 0.8),
    (0.0213, 0.35, -0.1),
    (0.0247, 0.3, 0.6),
];
const HALL_EARLY: &[(f32, f32, f32)] = &[
    (0.019, 0.6, -0.7),
    (0.027, 0.5, 0.6),
    (0.036, 0.45, -0.3),
    (0.044, 0.4, 0.8),
    (0.053, 0.35, -0.6),
    (0.061, 0.3, 0.2),
];

fn space_of(impulse: Impulse) -> Space {
    match impulse {
        Impulse::Room => Space {
            gap: 0.002,
            build: 0.005,
            rt60: [0.7, 0.6, 0.35],
            early: ROOM_EARLY,
        },
        Impulse::Plate => Space {
            gap: 0.0,
            build: 0.002,
            rt60: [2.0, 2.0, 1.6],
            early: &[],
        },
        Impulse::Spring => Space {
            gap: 0.0,
            build: 0.001,
            rt60: [1.5, 1.4, 0.6],
            early: &[],
        },
        Impulse::Reverse => Space {
            gap: 0.0,
            build: 0.01,
            rt60: [2.0, 1.8, 1.0],
            early: &[],
        },
        _ => Space {
            gap: 0.018,
            build: 0.04,
            rt60: [2.6, 2.2, 1.2],
            early: HALL_EARLY,
        },
    }
}

/// White noise from `state` (xorshift), -1 to 1.
#[inline]
fn noise(state: &mut u32) -> f32 {
    dsp::xorshift32(state) as f32 / u32::MAX as f32 * 2.0 - 1.0
}

impl Maker {
    fn new(shaping: Shaping, sample_rate: f32) -> Maker {
        let len = ((shaping.seconds() * sample_rate) as usize).max(1);
        let mut maker = Maker {
            shaping,
            sample_rate,
            len,
            at: 0,
            noise: [0x1234_5678, 0x0bad_cafe],
            bands: [[0.0; 2]; 2],
            filters: [[BoxFilter::default(); MAX_FILTERS]; 2],
            filter_count: 0,
            damp: [0.0; 2],
            energy: 0.0,
        };
        // A box's resonances, lower for a bigger one.
        let scale = 1.0 / shaping.stretch;
        let chain: &[(char, f32, f32, f32)] = match shaping.impulse {
            Impulse::Cabinet1x12 => &[
                ('h', 85.0, 1.0, 0.0),
                ('b', 110.0, 1.5, 5.0),
                ('b', 2600.0, 1.8, 6.0),
                ('l', 5000.0, 0.7, 0.0),
                ('l', 5000.0, 0.7, 0.0),
            ],
            Impulse::Cabinet4x12 => &[
                ('h', 70.0, 1.0, 0.0),
                ('b', 95.0, 1.4, 4.0),
                ('b', 1800.0, 1.2, 3.0),
                ('b', 3200.0, 2.0, 5.0),
                ('l', 4200.0, 0.7, 0.0),
                ('l', 4200.0, 0.7, 0.0),
            ],
            Impulse::Radio => &[
                ('h', 450.0, 0.7, 0.0),
                ('h', 450.0, 0.7, 0.0),
                ('b', 1500.0, 1.0, 6.0),
                ('l', 3000.0, 0.7, 0.0),
                ('l', 3000.0, 0.7, 0.0),
            ],
            _ => &[],
        };
        maker.filter_count = chain.len().min(MAX_FILTERS);
        for filters in &mut maker.filters {
            for (filter, &(kind, hz, q, db)) in filters.iter_mut().zip(chain) {
                *filter = BoxFilter::new(kind, hz * scale, q, db, sample_rate);
            }
        }
        maker
    }

    /// The raw impulse's sample `n`, per channel, before the shaping.
    fn raw(&mut self, n: usize) -> [f32; 2] {
        let t = n as f32 / self.sample_rate;
        let shaping = self.shaping;
        if shaping.impulse.is_box() {
            // An impulse through the box; the 4x12's four speakers a
            // fraction of a millisecond apart.
            let mut out = [0.0; 2];
            for (channel, filters) in self.filters.iter_mut().enumerate() {
                let mut x = if n == 0 { 1.0 } else { 0.0 };
                if shaping.impulse == Impulse::Cabinet4x12 {
                    for delay_ms in [0.21, 0.37, 0.52] {
                        let late = (delay_ms * 0.001 * self.sample_rate / shaping.stretch
                            * (1.0 + 0.1 * channel as f32))
                            as usize;
                        if n == late {
                            x += 0.8;
                        }
                    }
                }
                if shaping.impulse == Impulse::Radio {
                    for (delay_ms, level) in [(1.3, 0.4), (2.9, 0.25), (4.1, 0.15)] {
                        let late = (delay_ms * 0.001 * self.sample_rate) as usize + channel;
                        if n == late {
                            x += level;
                        }
                    }
                }
                for filter in &mut filters[..self.filter_count] {
                    x = filter.tick(x);
                }
                out[channel] = x;
            }
            return out;
        }
        let space = space_of(shaping.impulse);
        let stretch = shaping.stretch;
        let mut out = [0.0; 2];
        // Early reflections.
        for &(time, level, pan) in space.early {
            if n == (time * stretch * self.sample_rate) as usize {
                out[0] += level * (0.5 + 0.5 * pan);
                out[1] += level * (0.5 - 0.5 * pan);
            }
        }
        // The spring's chirps: every 28 ms (times the stretch), a quick
        // falling sweep, each a little quieter.
        if shaping.impulse == Impulse::Spring {
            let period = 0.028 * stretch;
            let into = t % period;
            if into < 0.012 {
                let sweep = 2500.0 * (1.0 - into / 0.012) + 200.0;
                let phase = TAU * sweep * into;
                let level = 0.6 * (1.0 - into / 0.012) * (-6.9 * t / (1.4 * stretch)).exp();
                out[0] += level * phase.sin();
                out[1] += level * (phase + 0.4).sin();
            }
        }
        // The tail: noise in three bands, each dying at its own rate,
        // after the gap and a short build.
        let start = space.gap * stretch;
        if t >= start {
            let since = t - start;
            let build = 1.0 - (-since / (space.build * stretch).max(1e-4)).exp();
            let low_pole = (-TAU * 500.0 / self.sample_rate).exp();
            let high_pole = (-TAU * 4000.0 / self.sample_rate).exp();
            let channels = self.noise.iter_mut().zip(&mut self.bands).zip(&mut out);
            for ((state, [low, below_high]), out) in channels {
                let x = noise(state);
                *low = x + low_pole * (*low - x);
                *below_high = x + high_pole * (*below_high - x);
                let parts = [*low, *below_high - *low, x - *below_high];
                let mut tail = 0.0;
                for (part, rt60) in parts.iter().zip(space.rt60) {
                    tail += part * (-6.9 * since / (rt60 * stretch)).exp();
                }
                *out += 0.5 * build * tail;
            }
        }
        out
    }

    /// Makes up to `count` more samples of the impulse into `ir`; whether
    /// it's done.
    fn make(&mut self, ir: &mut [Vec<f32>; 2], count: usize) -> bool {
        let end = (self.at + count).min(self.len);
        let shaping = self.shaping;
        let seconds = self.len as f32 / self.sample_rate;
        let [left, right] = ir;
        let made = left[self.at..end].iter_mut().zip(&mut right[self.at..end]);
        for (n, (left, right)) in (self.at..end).zip(made) {
            // Reverse plays the space's impulse backwards: made from its
            // end.
            let source = if shaping.impulse == Impulse::Reverse {
                self.len - 1 - n
            } else {
                n
            };
            let raw = if shaping.impulse == Impulse::Reverse {
                self.reversed(source)
            } else {
                self.raw(source)
            };
            let t = n as f32 / self.sample_rate;
            // Attack fades in over up to half the length; Decay is in the
            // length; Damp lowers a one-pole's corner as it goes, from
            // 20 kHz towards 200 Hz.
            let rise = shaping.attack * 0.5 * seconds;
            let fade_in = if rise > 0.0 && t < rise {
                0.5 - 0.5 * (PI * t / rise).cos()
            } else {
                1.0
            };
            let fade_out = if n + 64 > self.len {
                (self.len - n) as f32 / 64.0
            } else {
                1.0
            };
            let corner = (20_000.0 * (-shaping.damp * 7.0 * t / seconds).exp2()).max(200.0);
            let pole = (-TAU * corner.min(0.45 * self.sample_rate) / self.sample_rate).exp();
            let samples = [left, right].into_iter().zip(&mut self.damp).zip(raw);
            for ((sample, damp), raw) in samples {
                let x = fade_in * fade_out * raw;
                let y = if shaping.damp > 0.0 {
                    *damp = x + pole * (*damp - x);
                    *damp
                } else {
                    x
                };
                *sample = y;
                self.energy += f64::from(y) * f64::from(y);
            }
        }
        self.at = end;
        self.at >= self.len
    }

    /// The space's impulse sample `n` (for Reverse): its tail is simple
    /// enough to make at any point, without the samples before.
    fn reversed(&mut self, n: usize) -> [f32; 2] {
        let space = space_of(Impulse::Reverse);
        let t = n as f32 / self.sample_rate;
        let mut out = [0.0; 2];
        for (channel, out) in out.iter_mut().enumerate() {
            let x = noise(&mut self.noise[channel]);
            *out = 0.5 * x * (-6.9 * t / (space.rt60[1] * self.shaping.stretch)).exp();
        }
        out
    }

    /// The gain that brings the impulse to unit energy, per channel on
    /// average.
    fn gain(&self) -> f32 {
        if self.energy > 0.0 {
            (2.0 / self.energy).sqrt() as f32
        } else {
            0.0
        }
    }
}

/// Makes a whole impulse at once, at `sample_rate`, scaled to unit energy:
/// what the panel draws (at a low rate, so it's quick). Not for the audio
/// thread.
pub fn impulse(values: &[f32], sample_rate: f32) -> [Vec<f32>; 2] {
    let mut maker = Maker::new(Shaping::of(values), sample_rate);
    let mut ir = [vec![0.0; maker.len], vec![0.0; maker.len]];
    maker.make(&mut ir, usize::MAX);
    let gain = maker.gain();
    for channel in &mut ir {
        channel.iter_mut().for_each(|s| *s *= gain);
    }
    ir
}

/// Where making a new impulse has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Making {
    Idle,
    Synthesis,
    Head(usize),
    Tail(usize),
    /// Made; waiting for the tail's next block to start gathering both.
    Ready,
    /// Both are being gathered; the crossfade starts at the tail's next
    /// block.
    Warming,
    Fading,
}

pub struct Convolve {
    values: Values<COUNT>,
    sample_rate: f32,
    /// The impulse heard, and the one being made or faded to (0 or 1).
    current: usize,
    /// Each impulse's first [`DIRECT`] samples, per channel, and their
    /// lengths in samples.
    direct: [[[f32; DIRECT]; 2]; 2],
    lengths: [usize; 2],
    /// The input's last [`DIRECT`] samples per channel, written twice, a
    /// block apart, so the latest [`DIRECT`] always lie in a row, oldest
    /// first, after `recent_at`; the direct parts are kept reversed to
    /// match.
    recent: [[f32; 2 * DIRECT]; 2],
    recent_at: usize,
    head: Level,
    tail: Level,
    /// Making a new impulse: what's next, the maker, and the impulse.
    making: Making,
    maker: Option<Maker>,
    ir: [Vec<f32>; 2],
    /// The shaping each impulse was made with, and the one being made (or
    /// heard, when none is).
    shapings: [Shaping; 2],
    made: Shaping,
    /// Nothing has been processed since `prepare`: parameters still arrive
    /// on the UI thread, where an impulse can be made at once.
    unplayed: bool,
    /// How far the crossfade has got, and its step a sample.
    fade: f32,
    fade_step: f32,
    predelay: [Ring; 2],
    predelay_tap: Tap,
    tone: Smoothed,
    tone_state: [f32; 2],
    tone_pole: f32,
    gain: Smoothed,
    mix: Smoothed,
    output: Smoothed,
    wet_peak: PeakMeter,
}

impl Convolve {
    pub fn new() -> Convolve {
        let smoothed = |value: f32| Smoothed::new(value, SMOOTH_MS);
        let shaping = Shaping::of(&crate::defaults::<COUNT>(PARAMS));
        let mut convolve = Convolve {
            values: Values::new(PARAMS),
            sample_rate: 48_000.0,
            current: 0,
            direct: [[[0.0; DIRECT]; 2]; 2],
            lengths: [0; 2],
            recent: [[0.0; 2 * DIRECT]; 2],
            recent_at: 0,
            head: Level::default(),
            tail: Level::default(),
            making: Making::Idle,
            maker: None,
            ir: [Vec::new(), Vec::new()],
            shapings: [shaping; 2],
            made: shaping,
            unplayed: true,
            fade: 0.0,
            fade_step: 1.0,
            predelay: [Ring::default(), Ring::default()],
            predelay_tap: Tap::new(0.0),
            tone: smoothed(0.0),
            tone_state: [0.0; 2],
            tone_pole: 0.0,
            gain: smoothed(1.0),
            mix: smoothed(0.0),
            output: smoothed(1.0),
            wet_peak: PeakMeter::default(),
        };
        convolve.prepare(48_000.0, 0);
        convolve
    }

    fn smoothers(&mut self) -> [&mut Smoothed; 4] {
        [
            &mut self.tone,
            &mut self.gain,
            &mut self.mix,
            &mut self.output,
        ]
    }

    /// Hands parameter `index`'s value to whatever uses it, gliding there
    /// at `pace`. The impulse's own parameters are taken up in `process`.
    fn apply(&mut self, index: usize, pace: Pace) {
        let value = self.values[index];
        match index {
            PREDELAY if pace == Pace::Modulation => {
                self.predelay_tap
                    .glide_to(self.predelay_age(false), MAX_BLOCK);
            }
            PREDELAY => self.predelay_tap.set_target(self.predelay_age(true)),
            TONE => self.tone.move_to(value / 100.0, pace),
            IR_GAIN => self.gain.move_to(dsp::db_to_gain(value), pace),
            MIX => self.mix.move_to(value / 100.0, pace),
            OUTPUT => self.output.move_to(dsp::db_to_gain(value), pace),
            _ => {}
        }
    }

    fn predelay_age(&self, whole: bool) -> f32 {
        let samples = self.values[PREDELAY] * 0.001 * self.sample_rate;
        let samples = if whole { samples.round() } else { samples };
        samples.min(self.predelay[0].len().saturating_sub(3) as f32)
    }

    /// Makes the impulse `set` from `shaping` all at once: for `prepare`
    /// and `reset`, not while playing.
    fn make_now(&mut self, set: usize, shaping: Shaping) {
        let mut maker = Maker::new(shaping, self.sample_rate);
        let len = maker.len.min(self.ir[0].len());
        maker.len = len;
        maker.make(&mut self.ir, usize::MAX);
        let gain = maker.gain();
        self.install(set, len, gain, None);
        self.shapings[set] = shaping;
        self.made = shaping;
    }

    /// Transforms the impulse in `self.ir` (`len` samples, scaled by
    /// `gain`) into slot `set`: the direct part, then every head and tail
    /// partition, or only those `only` says (head, then tail ranges).
    fn install(&mut self, set: usize, len: usize, gain: f32, only: Option<(bool, usize, usize)>) {
        if only.is_none() {
            self.install_direct(set, len, gain);
        }
        let (head, tail) = match only {
            None => ((0, self.head.reach[set]), (0, self.tail.reach[set])),
            Some((true, from, to)) => ((from, to), (0, 0)),
            Some((false, from, to)) => ((0, 0), (from, to)),
        };
        for partition in head.0..head.1.min(self.head.reach[set]) {
            self.head
                .transform_partition(set, partition, &self.ir, len, gain);
        }
        for partition in tail.0..tail.1.min(self.tail.reach[set]) {
            self.tail
                .transform_partition(set, partition, &self.ir, len, gain);
        }
    }

    /// The direct part of the impulse in `self.ir` (reversed, to meet the
    /// input's history oldest first) and how far each level reaches, for
    /// slot `set`.
    fn install_direct(&mut self, set: usize, len: usize, gain: f32) {
        for channel in 0..2 {
            for (i, tap) in self.direct[set][channel].iter_mut().rev().enumerate() {
                *tap = if i < len {
                    gain * self.ir[channel][i]
                } else {
                    0.0
                };
            }
        }
        self.lengths[set] = len;
        self.head.reach[set] = self.head.reach_of(len.min(HEAD));
        self.tail.reach[set] = self.tail.reach_of(len);
    }

    /// One step of making a new impulse, a bounded amount of work, once
    /// per [`DIRECT`] samples.
    fn make_step(&mut self) {
        let wanted = Shaping::of(&self.values);
        let next = 1 - self.current;
        match self.making {
            Making::Idle => {
                if wanted != self.made {
                    let mut maker = Maker::new(wanted, self.sample_rate);
                    maker.len = maker.len.min(self.ir[0].len());
                    self.maker = Some(maker);
                    self.made = wanted;
                    self.making = Making::Synthesis;
                }
            }
            Making::Synthesis => {
                if let Some(maker) = &mut self.maker
                    && maker.make(&mut self.ir, SYNTH_PER_STEP)
                {
                    let (len, gain) = (maker.len, maker.gain());
                    // The direct part and the reach, now the impulse is
                    // whole.
                    self.install_direct(next, len, gain);
                    self.making = Making::Head(0);
                }
            }
            Making::Head(from) => {
                let to = from + HEAD_PER_STEP;
                let gain = self.maker.as_ref().map_or(1.0, Maker::gain);
                self.install(next, self.lengths[next], gain, Some((true, from, to)));
                self.making = if to >= self.head.reach[next] {
                    Making::Tail(0)
                } else {
                    Making::Head(to)
                };
            }
            Making::Tail(from) => {
                let to = from + TAIL_PER_STEP;
                let gain = self.maker.as_ref().map_or(1.0, Maker::gain);
                self.install(next, self.lengths[next], gain, Some((false, from, to)));
                self.making = if to >= self.tail.reach[next] {
                    self.shapings[next] = self.made;
                    Making::Ready
                } else {
                    Making::Tail(to)
                };
            }
            // Moved on at the tail's block boundaries and while fading.
            Making::Ready | Making::Warming | Making::Fading => {}
        }
    }

    /// Whether impulse `set` is being gathered this block: the one heard,
    /// and the next while it warms up and fades in.
    fn gathering(&self, set: usize) -> bool {
        set == self.current || matches!(self.making, Making::Warming | Making::Fading)
    }

    /// Every [`DIRECT`] samples, as the head's block of input completes:
    /// the head's next block worked out for every impulse gathered; at the
    /// tail's own boundary, its block worked out over the last one goes
    /// out and its input is taken in; then a slice of the tail's next
    /// block, and a step of making a new impulse.
    fn block_done(&mut self) {
        self.head.take_block();
        for set in 0..2 {
            if self.gathering(set) {
                let reach = self.head.reach[set];
                self.head.gather(set, 0, reach);
                self.head.finish(set, false);
            }
        }
        if self.tail.filled == TAIL {
            self.tail.take_block();
            for set in 0..2 {
                std::mem::swap(&mut self.tail.heard[set], &mut self.tail.next[set]);
            }
            self.tail.slice = 0;
            // A new impulse made is gathered too from now (from a clean
            // sum: the impulse that last used its slot may have stopped
            // halfway through a block); one gathered all through the last
            // block is ready to fade in.
            match self.making {
                Making::Ready => {
                    self.tail.sums[1 - self.current].fill(0.0);
                    self.making = Making::Warming;
                }
                Making::Warming => self.making = Making::Fading,
                _ => {}
            }
        }
        let slices = self.tail.slices;
        if self.tail.slice < slices {
            let per = self.tail.partitions.div_ceil(slices);
            let (from, to) = (self.tail.slice * per, (self.tail.slice + 1) * per);
            for set in 0..2 {
                if self.gathering(set) {
                    self.tail.gather(set, from, to);
                    if self.tail.slice + 1 == slices {
                        self.tail.finish(set, true);
                    }
                }
            }
            self.tail.slice += 1;
        }
        self.make_step();
    }
}

impl Default for Convolve {
    fn default() -> Self {
        Convolve::new()
    }
}

impl Effect for Convolve {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        let longest = (MAX_SECONDS * sample_rate).ceil() as usize;
        self.ir = [vec![0.0; longest], vec![0.0; longest]];
        self.head = Level::new(DIRECT, DIRECT, HEAD, 1);
        self.tail = Level::new(TAIL, HEAD, longest, TAIL / DIRECT);
        let samples_per_ms = 0.001 * sample_rate;
        for ring in &mut self.predelay {
            ring.allocate((250.0 * samples_per_ms).ceil() as usize + 4);
        }
        self.predelay_tap
            .set_fade_length((PREDELAY_FADE_MS * samples_per_ms) as usize);
        self.predelay_tap.set_max_slope(MAX_TAP_SLOPE);
        for smoothed in self.smoothers() {
            smoothed.prepare(sample_rate);
        }
        self.tone_pole = (-TAU * TONE_HZ / sample_rate).exp();
        self.fade_step = 1.0 / (FADE_MS * samples_per_ms).max(1.0);
        self.wet_peak.prepare(sample_rate);
        for index in 0..COUNT {
            self.apply(index, Pace::Knob);
        }
        self.current = 0;
        self.make_now(0, Shaping::of(&self.values));
        self.unplayed = true;
        self.reset();
    }

    fn reset(&mut self) {
        for smoothed in self.smoothers() {
            smoothed.settle();
        }
        self.predelay_tap.jump(self.predelay_age(true));
        for ring in &mut self.predelay {
            ring.clear();
        }
        self.recent = [[0.0; 2 * DIRECT]; 2];
        self.recent_at = 0;
        self.head.clear();
        self.tail.clear();
        self.tone_state = [0.0; 2];
        // Whatever was being made is dropped; the impulse heard stays, and
        // if it isn't the one asked for, the next is made from scratch.
        self.making = Making::Idle;
        self.maker = None;
        self.made = self.shapings[self.current];
        self.fade = 0.0;
        self.wet_peak.reset();
        self.values.refresh();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _context: &Context) {
        self.values.played();
        self.unplayed = false;
        if self.ir[0].is_empty() {
            return;
        }
        let len = left.len().min(right.len());
        let mut wet_peak = 0.0f32;
        for n in 0..len {
            let clean = |x: f32| if x.is_finite() { x } else { 0.0 };
            let dry = [clean(left[n]), clean(right[n])];
            let [line_l, line_r] = &mut self.predelay;
            line_l.push(dry[0]);
            line_r.push(dry[1]);
            let taps = self.predelay_tap.advance();
            let input = [taps.read(line_l), taps.read(line_r)];
            self.recent_at = (self.recent_at + 1) % DIRECT;
            for (recent, &x) in self.recent.iter_mut().zip(&input) {
                recent[self.recent_at] = x;
                recent[self.recent_at + DIRECT] = x;
            }
            self.head.push(input[0], input[1]);
            self.tail.push(input[0], input[1]);

            // Each impulse being gathered: its direct part, and where this
            // sample is in the head's and the tail's blocks heard now.
            let (head_at, tail_at) = (self.head.filled - 1, self.tail.filled - 1);
            let window = self.recent_at + 1..self.recent_at + 1 + DIRECT;
            let mut wet = [[0.0f32; 2]; 2];
            for (set, wet) in wet.iter_mut().enumerate() {
                if !self.gathering(set) {
                    continue;
                }
                for (channel, wet) in wet.iter_mut().enumerate() {
                    let direct: f32 = self.direct[set][channel]
                        .iter()
                        .zip(&self.recent[channel][window.clone()])
                        .map(|(h, x)| h * x)
                        .sum();
                    *wet = direct
                        + self.head.heard[set][channel][head_at]
                        + self.tail.heard[set][channel][tail_at];
                }
            }
            let mut out = wet[self.current];
            if self.making == Making::Fading {
                let next = wet[1 - self.current];
                self.fade = (self.fade + self.fade_step).min(1.0);
                for channel in 0..2 {
                    out[channel] += self.fade * (next[channel] - out[channel]);
                }
                if self.fade >= 1.0 {
                    self.current = 1 - self.current;
                    self.making = Making::Idle;
                    self.maker = None;
                    self.fade = 0.0;
                }
            }
            if self.head.filled == DIRECT {
                self.block_done();
            }

            // Tone, gain, mix and output.
            let tilt = (self.tone.tick() * TONE_DB / 20.0 * std::f32::consts::LOG2_10).exp2();
            let gain = self.gain.tick();
            let mix = self.mix.tick();
            let output = self.output.tick();
            let mut result = [0.0f32; 2];
            for channel in 0..2 {
                let x = out[channel];
                let state = &mut self.tone_state[channel];
                *state = x + self.tone_pole * (*state - x);
                let low = *state;
                let wet = gain * (low / tilt + (x - low) * tilt);
                wet_peak = wet_peak.max(wet.abs());
                result[channel] = output * (dry[channel] + mix * (wet - dry[channel]));
            }
            left[n] = result[0];
            right[n] = result[1];
        }
        for state in &mut self.tone_state {
            *state = if state.is_finite() {
                flush(*state)
            } else {
                0.0
            };
        }
        self.wet_peak.update(wet_peak, len);
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn receive(&mut self, index: usize, value: f32, pace: Pace) {
        if self.values.set(index, value) {
            self.apply(index, pace);
            if self.values.is_fresh() {
                self.reset();
            }
            let wanted = Shaping::of(&self.values);
            if self.unplayed && wanted != self.shapings[self.current] {
                self.make_now(self.current, wanted);
            }
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out[METER_WET] = self.wet_peak.level();
        out[METER_LENGTH] = self.lengths[self.current] as f32 / self.sample_rate;
        out[METER_MAKING] = if self.making == Making::Idle {
            0.0
        } else {
            1.0
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{RATE, db, noise, peak, rms, run, set, sine, values};

    fn convolve(settings: &[(&str, f32)]) -> Convolve {
        let mut convolve = Convolve::new();
        convolve.prepare(RATE, 64);
        for &(id, value) in settings {
            set(&mut convolve, id, value);
        }
        convolve
    }

    #[test]
    fn the_fft_is_a_dft() {
        let n = 64;
        let fft = Fft::new(n);
        let x = noise(2 * n, 3);
        let (mut re, mut im) = (x[..n].to_vec(), x[n..].to_vec());
        fft.transform(&mut re, &mut im, false);
        for k in 0..n {
            let (mut a, mut b) = (0.0f64, 0.0f64);
            for t in 0..n {
                let angle = -std::f64::consts::TAU * (k * t) as f64 / n as f64;
                let (xr, xi) = (f64::from(x[t]), f64::from(x[n + t]));
                a += xr * angle.cos() - xi * angle.sin();
                b += xr * angle.sin() + xi * angle.cos();
            }
            assert!((f64::from(re[k]) - a).abs() < 1e-4 && (f64::from(im[k]) - b).abs() < 1e-4);
        }
        fft.transform(&mut re, &mut im, true);
        for t in 0..n {
            assert!((re[t] / n as f32 - x[t]).abs() < 1e-5);
            assert!((im[t] / n as f32 - x[n + t]).abs() < 1e-5);
        }
    }

    #[test]
    fn it_convolves_exactly_with_no_latency() {
        // Short bursts through every level (direct, head and tail),
        // against the plain sum: the room's impulse, a second long.
        let settings = [("impulse", 0.0), ("mix", 100.0), ("predelay", 0.0)];
        let ir = impulse(&values(PARAMS, &settings), RATE);
        let mut convolve = convolve(&settings);
        let mut input_l = noise(1500, 5);
        let mut input_r = noise(1500, 6);
        input_l.resize(ir[0].len() + 3000, 0.0);
        input_r.resize(ir[0].len() + 3000, 0.0);
        let (mut left, mut right) = (input_l.clone(), input_r.clone());
        run(&mut convolve, &mut left, &mut right, 64, &Context::DEFAULT);
        let mut worst = 0.0f64;
        let mut by_range = [0.0f64; 4];
        for (out, input, h) in [(&left, &input_l, &ir[0]), (&right, &input_r, &ir[1])] {
            for n in (0..out.len()).step_by(37) {
                let expected: f64 = (0..1500.min(n + 1))
                    .filter(|&i| n - i < h.len())
                    .map(|i| f64::from(input[i]) * f64::from(h[n - i]))
                    .sum();
                let error = (f64::from(out[n]) - expected).abs();
                worst = worst.max(error);
                let range = [64, HEAD, HEAD + 1600, usize::MAX]
                    .iter()
                    .position(|&end| n < end)
                    .unwrap();
                by_range[range] = by_range[range].max(error);
            }
        }
        println!("errors by range: {by_range:?}");
        let level = rms(&left[..ir[0].len()]);
        println!("largest error {worst:.2e} at a level of {level:.3}");
        assert!(worst < 1e-4 * level, "{by_range:?}");
    }

    #[test]
    fn every_impulse_is_about_as_loud_and_shaped_as_meant() {
        // White noise, long enough for the longest impulse to be all in
        // play; both sides together, as a stereo impulse can lean.
        let input = noise(48_000 * 5, 8);
        for (index, name) in IMPULSES.iter().enumerate() {
            let mut convolve = convolve(&[("impulse", index as f32), ("mix", 100.0)]);
            let (mut left, mut right) = (input.clone(), input.clone());
            run(&mut convolve, &mut left, &mut right, 64, &Context::DEFAULT);
            let both: Vec<f32> = left[192_000..]
                .iter()
                .chain(&right[192_000..])
                .copied()
                .collect();
            let level = db(rms(&both) / rms(&input[192_000..]));
            println!("{name}: {level:+.1} dB");
            assert!(level.abs() < 4.0, "{name}: {level}");
        }
        // A cabinet has no highs to speak of; a hall plenty.
        let level = |index: usize, freq: f64| {
            let ir = impulse(&values(PARAMS, &[("impulse", index as f32)]), RATE);
            let step = std::f64::consts::TAU * freq / f64::from(RATE);
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &h) in ir[0].iter().enumerate() {
                re += f64::from(h) * (step * n as f64).cos();
                im -= f64::from(h) * (step * n as f64).sin();
            }
            db((re * re + im * im).sqrt())
        };
        let cabinet = level(4, 14_000.0) - level(4, 2600.0);
        let hall = level(1, 14_000.0) - level(1, 2600.0);
        println!("14 kHz against 2.6 kHz: cabinet {cabinet:.1} dB, hall {hall:.1} dB");
        assert!(cabinet < -30.0 && hall > -15.0);
    }

    #[test]
    fn size_and_decay_set_the_length() {
        let seconds = |settings: &[(&str, f32)]| Shaping::of(&values(PARAMS, settings)).seconds();
        assert!((seconds(&[("impulse", 1.0)]) - 3.0).abs() < 1e-3);
        assert!((seconds(&[("impulse", 1.0), ("size", 100.0)]) - 4.0).abs() < 1e-3);
        assert!(seconds(&[("impulse", 1.0), ("decay", 30.0)]) < 0.5);
        let short = convolve(&[("impulse", 1.0), ("decay", 30.0)]);
        let mut meters = [0.0; MAX_METERS];
        short.meters(&mut meters);
        assert!(meters[METER_LENGTH] < 0.5, "{meters:?}");
        // Damp darkens the end more than the start.
        let ir = impulse(&values(PARAMS, &[("impulse", 1.0), ("damp", 100.0)]), RATE);
        let steps =
            |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / rms(x) as f32;
        let n = ir[0].len();
        assert!(steps(&ir[0][n / 2..]) < 0.5 * steps(&ir[0][4800..n / 4]));
    }

    #[test]
    fn a_new_impulse_fades_in_while_playing() {
        let mut convolve = convolve(&[("mix", 100.0), ("impulse", 0.0)]);
        let input = sine(110.0, 0.4, 48_000 * 3, RATE);
        let (mut left, mut right) = (input.clone(), input.clone());
        let mut making = Vec::new();
        for (block, (l, r)) in left.chunks_mut(64).zip(right.chunks_mut(64)).enumerate() {
            if block == 375 {
                set(&mut convolve, "impulse", 4.0);
            }
            convolve.process(l, r, &Context::DEFAULT);
            let mut meters = [0.0; MAX_METERS];
            convolve.meters(&mut meters);
            making.push(meters[METER_MAKING]);
        }
        // It takes a fraction of a second, then it's done.
        let busy = making.iter().filter(|&&m| m > 0.0).count() * 64;
        println!("made and faded in {:.0} ms", busy as f32 / RATE * 1000.0);
        assert!(busy > 0 && (busy as f32) < 0.6 * RATE);
        assert_eq!(*making.last().unwrap(), 0.0);
        // No click on the way.
        let step = |x: &[f32]| {
            x.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let before = step(&left[12_000..24_000]);
        let through = step(&left[24_000..72_000]);
        let after = step(&left[96_000..]);
        println!("largest steps: before {before:.4}, through {through:.4}, after {after:.4}");
        assert!(through < 1.3 * before.max(after));
    }

    #[test]
    fn reset_silences_and_mix_zero_is_dry() {
        let mut wet = convolve(&[("mix", 100.0)]);
        let input = noise(9600, 2);
        let (mut l, mut r) = (input.clone(), input.clone());
        run(&mut wet, &mut l, &mut r, 64, &Context::DEFAULT);
        wet.reset();
        assert_eq!(wet.meter(), 0.0);
        let (mut l, mut r) = (vec![0.0; 9600], vec![0.0; 9600]);
        run(&mut wet, &mut l, &mut r, 64, &Context::DEFAULT);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));

        let mut dry = convolve(&[("mix", 0.0)]);
        let (mut l, mut r) = (input.clone(), input.clone());
        run(&mut dry, &mut l, &mut r, 64, &Context::DEFAULT);
        assert_eq!(l, input);
        assert!(peak(&r) > 0.0);
    }
}
