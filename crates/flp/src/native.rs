//! The saved states of FL Studio's own effects, for the ones the importer
//! carries settings over from. Most are a version number followed by the
//! plugin's parameters as 32-bit integers, in the order FL numbers them for
//! automation (so automation's parameter `n` is state number `n + 1`);
//! the oldest have no version.
//!
//! Fruity Parametric EQ 2's layout is documented (PyFLP issue #159) and its
//! units come from Image-Line's forum, where its knobs' typed-in values were
//! worked out; Fruity Balance's and Stereo Enhancer's are PyFLP's; the
//! others' were read from real projects, with the parameters automation
//! names and the manual's knob ranges to anchor them.

/// Reads the 32-bit numbers a state is made of.
fn numbers(state: &[u8]) -> Vec<i32> {
    state
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes(*b))
        .collect()
}

/// What a Parametric EQ 2 band does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eq2Type {
    Off,
    LowPass,
    BandPass,
    HighPass,
    Notch,
    LowShelf,
    Peaking,
    HighShelf,
}

impl Eq2Type {
    fn from_value(value: i32) -> Option<Eq2Type> {
        use Eq2Type::*;
        [
            Off, LowPass, BandPass, HighPass, Notch, LowShelf, Peaking, HighShelf,
        ]
        .get(usize::try_from(value).ok()?)
        .copied()
    }

    /// Whether its level does anything (FL ignores it on passes and notches).
    pub fn has_gain(self) -> bool {
        matches!(
            self,
            Eq2Type::LowShelf | Eq2Type::Peaking | Eq2Type::HighShelf
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Eq2Band {
    pub kind: Eq2Type,
    pub gain_db: f32,
    pub hz: f32,
    /// Bandwidth, in octaves (for a bell; see `width`).
    pub octaves: f32,
    /// The bandwidth knob, `0..=1`: what it means depends on the type
    /// (`convert::eq2_q`).
    pub width: f32,
    /// A pass's slope: `-3..=3`, 0 for FL's default (12 dB/octave). The
    /// sign picks FL's "gentle" or "steep" shape, the size the steepness.
    pub order: i32,
}

/// Fruity Parametric EQ 2: seven bands and a main level.
#[derive(Debug, Clone, PartialEq)]
pub struct Eq2 {
    pub bands: [Eq2Band; 7],
    pub main_db: f32,
}

/// The parameters of each band, in FL's numbering: seven levels, then
/// seven frequencies, seven bandwidths, seven types and seven orders.
pub const EQ2_LEVEL: u16 = 0;
pub const EQ2_FREQ: u16 = 7;
pub const EQ2_WIDTH: u16 = 14;
pub const EQ2_TYPE: u16 = 21;
pub const EQ2_ORDER: u16 = 28;
pub const EQ2_MAIN: u16 = 35;

/// An EQ 2 level (as the state stores it, hundredths of a dB) in dB.
fn eq2_db(raw: i32) -> f32 {
    raw as f32 / 100.0
}

/// An EQ 2 knob (`0..=1`) as a frequency: 20 Hz to 20 kHz on a log scale.
pub fn eq2_hz(x: f64) -> f32 {
    (20.0 * 1000f64.powf(x.clamp(0.0, 1.0))) as f32
}

/// An EQ 2 knob (`0..=1`) as a level: ±18 dB, straight.
pub fn eq2_gain_db(x: f64) -> f32 {
    ((x.clamp(0.0, 1.0) - 0.5) * 36.0) as f32
}

/// An EQ 2 knob (`0..=1`) as a bandwidth in octaves: 0.1 to 5.
pub fn eq2_octaves(x: f64) -> f32 {
    (0.1 + 4.9 * x.clamp(0.0, 1.0)) as f32
}

/// A bandwidth in octaves as the Q of a bell that wide (between its
/// half-gain points).
pub fn octaves_to_q(octaves: f32) -> f32 {
    let half = std::f32::consts::LN_2 / 2.0 * octaves.max(0.01);
    1.0 / (2.0 * half.sinh())
}

/// Knobs store `0..=65536` for `0..=1`.
const KNOB: f64 = 65_536.0;

impl Eq2 {
    pub fn parse(state: &[u8]) -> Option<Eq2> {
        let n = numbers(state);
        if n.len() < 37 {
            return None;
        }
        let band = |i: usize| -> Option<Eq2Band> {
            let at = |base: u16| n[1 + base as usize + i];
            Some(Eq2Band {
                kind: Eq2Type::from_value(at(EQ2_TYPE))?,
                gain_db: eq2_db(at(EQ2_LEVEL)),
                hz: eq2_hz(f64::from(at(EQ2_FREQ)) / KNOB),
                octaves: eq2_octaves(f64::from(at(EQ2_WIDTH)) / KNOB),
                width: (f64::from(at(EQ2_WIDTH)) / KNOB).clamp(0.0, 1.0) as f32,
                order: at(EQ2_ORDER),
            })
        };
        Some(Eq2 {
            bands: [
                band(0)?,
                band(1)?,
                band(2)?,
                band(3)?,
                band(4)?,
                band(5)?,
                band(6)?,
            ],
            main_db: eq2_db(n[1 + EQ2_MAIN as usize]),
        })
    }
}

/// Fruity Compressor's knee types, in its menu's order; the "/R" ones add
/// transient-controlled release.
const COMPRESSOR_KNEES: [f32; 4] = [0.0, 6.0, 7.0, 15.0];

/// Fruity Compressor: threshold, ratio, gain, attack, release, type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Compressor {
    pub threshold_db: f32,
    pub ratio: f32,
    pub gain_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    /// One of the "Vintage" types.
    pub vintage: bool,
}

impl Compressor {
    /// Tenths of a dB, tenths of the ratio and tenths of a millisecond of
    /// attack (its range is 0 to 400 ms); release in milliseconds (1 to
    /// 4000).
    pub fn parse(state: &[u8]) -> Option<Compressor> {
        let n = numbers(state);
        if n.len() < 7 {
            return None;
        }
        let kind = usize::try_from(n[6]).ok().filter(|k| *k < 8)?;
        Some(Compressor {
            threshold_db: n[1] as f32 / 10.0,
            ratio: n[2] as f32 / 10.0,
            gain_db: n[3] as f32 / 10.0,
            attack_ms: n[4] as f32 / 10.0,
            release_ms: n[5] as f32,
            knee_db: COMPRESSOR_KNEES[kind % 4],
            vintage: kind % 4 == 2,
        })
    }
}

/// Fruity Reeverb 2, as far as its settings are clear. What each knob does
/// was measured from FL's renders of a noise burst through the plugin
/// (`flp/examples/probe.rs`, 4 Nov 2026).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reeverb2 {
    /// The low cut on the reverb, in Hz.
    pub low_cut_hz: f32,
    /// The high cut, in Hz: saved in hundreds of them.
    pub high_cut_hz: f32,
    /// Stereo separation, `-64..=64`: the tail's sides are scaled by
    /// `1 - separation / 64`, so 64 is mono and -64 twice as wide.
    pub separation: f32,
    pub predelay_ms: f32,
    /// `1..=100`.
    pub size: f32,
    /// The decay knob in seconds; the tail's RT60 is longer (see
    /// [`reeverb2_rt60`]).
    pub decay_s: f32,
    /// The high damping's corner in Hz (saved in hundreds of them), or
    /// `None` at its top, where it's off.
    pub damping_hz: Option<f32>,
    /// The bass's decay, as a percentage of the rest's knob (`20..=300`),
    /// and where it turns, in Hz.
    pub bass: f32,
    pub crossover_hz: f32,
    /// Dry and wet levels, as gains (1 at FL's 100%), on FL's curves.
    pub dry: f32,
    pub wet: f32,
}

/// Reeverb 2's dry and wet levels, in automation's numbering.
pub const REEVERB2_DRY: u16 = 10;
pub const REEVERB2_WET: u16 = 12;

/// The top of Reeverb 2's level knobs, saved as `0..=160` with 128 at 100%.
pub const REEVERB2_LEVEL_TOP: f32 = 160.0;

/// Reeverb 2's wet and dry knobs (`0..=160`), as measured: dB at each
/// point, linear in between; below the first point the gain falls in a
/// straight line to nothing. Neither is linear in the knob.
const REEVERB2_WET_DB: [(f32, f32); 9] = [
    (16.0, -23.1),
    (32.0, -16.4),
    (48.0, -12.2),
    (64.0, -9.0),
    (96.0, -4.0),
    (112.0, -1.9),
    (128.0, 0.0),
    (144.0, 1.8),
    (160.0, 3.5),
];
const REEVERB2_DRY_DB: [(f32, f32); 5] = [
    (32.0, -21.7),
    (64.0, -12.7),
    (96.0, -6.0),
    (128.0, 0.0),
    (160.0, 5.6),
];

/// A level knob's gain from a table of dB points.
fn knob_gain(table: &[(f32, f32)], knob: f32) -> f32 {
    let db = |db: f32| 10f32.powf(db / 20.0);
    let (first, first_db) = table[0];
    if knob <= 0.0 {
        return 0.0;
    }
    if knob < first {
        return db(first_db) * knob / first;
    }
    let last = table[table.len() - 1];
    if knob >= last.0 {
        return db(last.1);
    }
    let i = table.iter().position(|(at, _)| *at > knob).unwrap_or(1);
    let ((a, a_db), (b, b_db)) = (table[i - 1], table[i]);
    db(a_db + (knob - a) / (b - a) * (b_db - a_db))
}

/// Reeverb 2's wet level as a gain, for its knob's `0..=160`.
pub fn reeverb2_wet_gain(knob: f32) -> f32 {
    knob_gain(&REEVERB2_WET_DB, knob)
}

/// Reeverb 2's dry level as a gain, for its knob's `0..=160`.
pub fn reeverb2_dry_gain(knob: f32) -> f32 {
    knob_gain(&REEVERB2_DRY_DB, knob)
}

/// How long Reeverb 2's tail takes to fall 60 dB: its decay knob times a
/// factor that shrinks with size (1.83 at size 1, 1.41 at 100), and never
/// much under 0.35 s, which the diffusion alone rings for.
pub fn reeverb2_rt60(decay_s: f32, size: f32) -> f32 {
    const RATIO: [(f32, f32); 8] = [
        (1.0, 1.83),
        (10.0, 1.69),
        (25.0, 1.545),
        (35.0, 1.5),
        (44.0, 1.465),
        (50.0, 1.46),
        (75.0, 1.43),
        (100.0, 1.41),
    ];
    (decay_s * interpolate(&RATIO, size)).hypot(0.35)
}

/// Reeverb 2's bass knob (percent) as the multiple of the decay the bass
/// gets: far less than the knob says.
pub fn reeverb2_bass(percent: f32) -> f32 {
    const BASS: [(f32, f32); 5] = [
        (20.0, 0.375),
        (50.0, 0.705),
        (100.0, 1.0),
        (200.0, 1.25),
        (300.0, 1.42),
    ];
    let log = BASS.map(|(knob, bass)| (knob.ln(), bass.ln()));
    interpolate(&log, percent.max(1.0).ln()).exp()
}

/// `table`'s value at `x`, linear between points and flat beyond them.
fn interpolate(table: &[(f32, f32)], x: f32) -> f32 {
    let (first, last) = (table[0], table[table.len() - 1]);
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    let i = table.iter().position(|(at, _)| *at > x).unwrap_or(1);
    let ((a, a_y), (b, b_y)) = (table[i - 1], table[i]);
    a_y + (x - a) / (b - a) * (b_y - a_y)
}

impl Reeverb2 {
    /// After a version, the knobs in the plugin's order: low cut (Hz),
    /// high cut, pre-delay (ms), size (%), diffusion, decay (tenths of a
    /// second), high damping (hundreds of Hz, 221 for off), bass (%),
    /// crossover (Hz), stereo separation, then dry, early reflections and
    /// wet (`0..=160`, 128 for 100%), and the modulation's speed and depth.
    /// Diffusion, early reflections and modulation aren't carried over.
    pub fn parse(state: &[u8]) -> Option<Reeverb2> {
        let n = numbers(state);
        if n.len() < 14 {
            return None;
        }
        Some(Reeverb2 {
            low_cut_hz: n[1] as f32,
            high_cut_hz: n[2] as f32 * 100.0,
            separation: n[10] as f32,
            predelay_ms: n[3] as f32,
            size: n[4] as f32,
            decay_s: n[6] as f32 / 10.0,
            damping_hz: (n[7] < 221).then(|| n[7] as f32 * 100.0),
            bass: n[8] as f32,
            crossover_hz: n[9] as f32,
            dry: reeverb2_dry_gain(n[1 + REEVERB2_DRY as usize] as f32),
            wet: reeverb2_wet_gain(n[1 + REEVERB2_WET as usize] as f32),
        })
    }
}

/// Fruity Limiter: a compressor, then a gain, then a peak limiter (with
/// a noise gate in it) and a saturation. Its state is a version, its 18
/// A Fruity Limiter gain knob (`0..=2000`, 1000 for 0 dB) in dB, measured
/// from FL's renders; below its first point the gain falls to nothing at 0.
pub fn limiter_gain_db(knob: f32) -> f32 {
    const GAIN_DB: [(f32, f32); 9] = [
        (250.0, -19.6),
        (500.0, -11.2),
        (750.0, -5.2),
        (1000.0, 0.0),
        (1100.0, 1.9),
        (1250.0, 4.8),
        (1500.0, 9.3),
        (1750.0, 13.7),
        (2000.0, 18.1),
    ];
    if knob < 250.0 {
        -19.6 + 40.0 * (knob.max(1.0) / 250.0).log10()
    } else {
        interpolate(&GAIN_DB, knob)
    }
}

/// Fruity WaveShaper: a version word and a format word (2, or 4 for two
/// more bytes at the end), pre gain, mix and post gain (`0..=256`-ish, 128
/// by default, taken as shares of 128), switches (bipolar, centre, the
/// oversampling as 0 to 4), two words of 3, the curve's point count, then
/// its points as FL's envelopes keep them (24 bytes: the step from the
/// last point and the value as doubles, `0..=1` both, the tension as a
/// float, the segment's type in byte 20, 2 for a hold; byte 23 only says
/// which way the tension bends), then 24 or 26 more bytes. Its curve runs
/// from no input to full scale (unipolar, mirrored below zero) or from -1
/// to 1 (bipolar), output likewise.
#[derive(Debug, Clone, PartialEq)]
pub struct FruityWaveShaper {
    pub pre: f32,
    pub mix: f32,
    pub post: f32,
    pub bipolar: bool,
    pub oversampling: u8,
    /// `(x, y, tension, type)`, x from 0 to 1 in order.
    pub points: Vec<(f64, f64, f32, u8)>,
}

impl FruityWaveShaper {
    pub fn parse(state: &[u8]) -> Option<FruityWaveShaper> {
        let word = |at: usize| Some(i32::from_le_bytes(state.get(at..at + 4)?.try_into().ok()?));
        let count = usize::try_from(word(30)?).ok()?;
        if count < 2 || state.len() < 34 + 24 * count {
            return None;
        }
        let share = |v: i32| v.max(0) as f32 / 128.0;
        let mut x = 0.0;
        let mut points = Vec::with_capacity(count);
        for k in 0..count {
            let record = &state[34 + 24 * k..58 + 24 * k];
            let double = |b: &[u8]| f64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
            x += double(&record[0..8]).max(0.0);
            let tension = f32::from_le_bytes([record[16], record[17], record[18], record[19]]);
            points.push((
                x.min(1.0),
                double(&record[8..16]).clamp(0.0, 1.0),
                if tension.is_finite() { tension.clamp(-1.0, 1.0) } else { 0.0 },
                record[20],
            ));
        }
        Some(FruityWaveShaper {
            pre: share(word(4)?),
            mix: share(word(8)?),
            post: share(word(12)?),
            bipolar: state[16] != 0,
            oversampling: state[18],
            points,
        })
    }

    /// The curve's value (`0..=1`) at `x` (`0..=1`): each segment bent by
    /// the tension on the point it ends at, as automation clips are.
    pub fn curve(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let mut previous = self.points[0];
        for &point in &self.points[1..] {
            if x <= point.0 {
                let span = point.0 - previous.0;
                if span <= 0.0 || point.3 == EnvelopePoint::HOLD {
                    return if x < point.0 { previous.1 } else { point.1 };
                }
                let t = ((x - previous.0) / span) as f32;
                return f64::from(project::segment_value(
                    previous.1 as f32,
                    point.1 as f32,
                    point.2,
                    t,
                ));
            }
            previous = point;
        }
        previous.1
    }
}

/// Maximus: a version word, then 7 numbers and a block of 13 for each band
/// (low, mid, high, master), the same again for the A/B comparison, then
/// each band's compression curve as an envelope. Automation numbers its
/// parameters in the same order (the master's post gain is 48). A band's
/// block: its state (0 on, 1 compression off, 2 muted, 3 off, 4 solo, in
/// the panel's order), pre gain, post gain, ..., the saturation's
/// threshold, its ceiling, attack, release 2, curve, release, curve and
/// sustain, the last six as the Fruity Limiter keeps them. Its gains are
/// taken to run on the Limiter's measured curve; the compression curves
/// aren't read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Maximus {
    pub bands: [MaximusBand; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaximusBand {
    pub state: i32,
    pub pre_db: f32,
    pub post_db: f32,
}

/// Where each band's block starts, in automation's numbering, and its gains.
pub const MAXIMUS_BAND: [u16; 4] = [7, 20, 33, 46];
pub const MAXIMUS_PRE: u16 = 1;
pub const MAXIMUS_POST: u16 = 2;
pub const MAXIMUS_MUTED: i32 = 2;

impl Maximus {
    pub fn parse(state: &[u8]) -> Option<Maximus> {
        let n = numbers(state);
        if n.len() < 60 {
            return None;
        }
        let field = |at: u16| n[1 + usize::from(at)];
        Some(Maximus {
            bands: MAXIMUS_BAND.map(|at| MaximusBand {
                state: field(at),
                pre_db: limiter_gain_db(field(at + MAXIMUS_PRE) as f32),
                post_db: limiter_gain_db(field(at + MAXIMUS_POST) as f32),
            }),
        })
    }
}

/// knobs, the same 18 again for the A/B comparison, and display settings.
/// Measured from FL's renders of a sine stepping up in level and of a
/// 24 dB jump (5 Nov 2026), as times to 63% of the way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limiter {
    /// The gain before the limiter (the compressor's make-up), in dB.
    pub gain_db: f32,
    /// The limiter's ceiling in dB, or `None` above 0 dB, where the
    /// saturation catches what passes instead.
    pub ceiling_db: Option<f32>,
    pub limiter_release_ms: f32,
    /// The compressor's threshold in dB, or `None` at the top (off), and
    /// its ratio (1 when off).
    pub threshold_db: Option<f32>,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl Limiter {
    /// Knob numbers (after the version): 1 gain, 2 saturation, 3 ceiling,
    /// 4 the limiter's attack (its look-ahead), 5 its curve, 6 release,
    /// 7 curve, 8 sustain; 9 the compressor's threshold, 10 ratio, 11 a
    /// bend of the ratio (not carried over), 13 release, 14 curve, 15
    /// attack; 16 and 17 the gate's closed level and threshold. Each is
    /// 0 to about 1000, the times and sustains to several thousand.
    pub fn parse(state: &[u8]) -> Option<Limiter> {
        let n = numbers(state);
        if n.len() < 19 {
            return None;
        }
        let knob = |i: usize| n[i] as f32;
        // What comes out at the most, with FL's 0.2 dB of safety.
        const CEILING_DB: [(f32, f32); 5] = [
            (250.0, -16.2),
            (500.0, -8.9),
            (750.0, -4.1),
            (900.0, -1.7),
            (1000.0, -0.2),
        ];
        const LIMITER_RELEASE_MS: [(f32, f32); 5] = [
            (0.0, 8.0),
            (1000.0, 10.0),
            (2000.0, 12.0),
            (5000.0, 39.0),
            (6810.0, 105.0),
        ];
        const THRESHOLD_DB: [(f32, f32); 4] =
            [(250.0, -16.3), (500.0, -8.1), (750.0, -2.1), (1000.0, 0.0)];
        const RATIO: [(f32, f32); 5] = [
            (0.0, 1.0),
            (250.0, 1.67),
            (500.0, 3.3),
            (750.0, 7.5),
            (1000.0, 20.0),
        ];
        let log = |table: &[(f32, f32)], x: f32| {
            let logged: Vec<(f32, f32)> = table.iter().map(|(k, v)| (*k, v.ln())).collect();
            interpolate(&logged, x).exp()
        };
        Some(Limiter {
            gain_db: limiter_gain_db(knob(1)),
            ceiling_db: (knob(3) <= 1000.0).then(|| interpolate(&CEILING_DB, knob(3))),
            limiter_release_ms: log(&LIMITER_RELEASE_MS, knob(6)),
            threshold_db: (knob(9) < 1000.0).then(|| {
                if knob(9) < 250.0 {
                    -16.3 - 44.0 * (1.0 - knob(9) / 250.0)
                } else {
                    interpolate(&THRESHOLD_DB, knob(9))
                }
            }),
            ratio: interpolate(&RATIO, knob(10)),
            // Both an exponential of the knob: 2 ms at 2000, 25 at 5313;
            // 3 ms at 3333, 35 at 6666 (the default), 283 at 10000.
            attack_ms: 0.436 * 10f32.powf(knob(15) / 3020.0),
            release_ms: 0.3 * 10f32.powf(knob(13) / 3333.0),
        })
    }

    /// Whether the compressor does the work: it's on, and the limiter
    /// passes everything below 0 dB untouched.
    pub fn compresses(&self) -> bool {
        self.threshold_db.is_some()
            && self.ratio > 1.01
            && self.gain_db <= 0.05
            && self.ceiling_db.is_none_or(|c| c >= -0.25)
    }
}

/// Fruity Balance: no version, just pan and volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Balance {
    /// `-128..=127`, 0 in the middle.
    pub pan: i32,
    /// `0..=320`, 256 at 0 dB, on the insert fader's curve.
    pub volume: i32,
}

impl Balance {
    /// PyFLP's layout.
    pub fn parse(state: &[u8]) -> Option<Balance> {
        let n = numbers(state);
        (n.len() == 2).then(|| Balance {
            pan: n[0],
            volume: n[1],
        })
    }
}

/// Fruity Center, FL's DC offset remover: whether it's on, the last
/// number (newer versions put another first).
pub fn center_on(state: &[u8]) -> Option<bool> {
    numbers(state).last().map(|on| *on != 0)
}

/// Fruity Stereo Enhancer, in PyFLP's layout (no version).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereoEnhancer {
    /// `-128..=127`.
    pub pan: i32,
    /// `0..=320`, 256 at 0 dB, as Fruity Balance's.
    pub volume: i32,
    /// `-96..=96`: turned left (negative) the sides are pushed apart,
    /// right they're merged, to about 98% either way.
    pub separation: i32,
    /// `-512..=512`: how far the left (negative) or right side is delayed,
    /// up to 500 ms.
    pub phase_offset: i32,
    /// Which side's polarity is flipped: 0 neither, 1 left, 2 right.
    pub invert: i32,
}

impl StereoEnhancer {
    pub fn parse(state: &[u8]) -> Option<StereoEnhancer> {
        let n = numbers(state);
        (n.len() == 6).then(|| StereoEnhancer {
            pan: n[0],
            volume: n[1],
            separation: n[2],
            phase_offset: n[3],
            invert: n[5],
        })
    }
}

/// Fruity Stereo Shaper's mixer matrix, as gains (12800 is 1, negative
/// flips the polarity), and its delay and phase knobs, raw: their units
/// aren't known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereoShaper {
    pub left: f32,
    pub right: f32,
    pub right_to_left: f32,
    pub left_to_right: f32,
    pub delay: i32,
    pub phase: i32,
}

impl StereoShaper {
    /// A version, then the sliders in the order the manual gives them:
    /// right into left, left, right, left into right; then the delay and
    /// phase knobs.
    pub fn parse(state: &[u8]) -> Option<StereoShaper> {
        let n = numbers(state);
        if n.len() < 7 {
            return None;
        }
        let gain = |v: i32| v as f32 / 12_800.0;
        Some(StereoShaper {
            right_to_left: gain(n[1]),
            left: gain(n[2]),
            right: gain(n[3]),
            left_to_right: gain(n[4]),
            delay: n[5],
            phase: n[6],
        })
    }
}

/// Fruity Chorus, as far as its settings are clear: a version, then delay,
/// depth, stereo, the three LFOs' speeds, their shapes, the crossover's
/// type and cutoff, and "wet only".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chorus {
    /// `0..=1024` across the knob's 0 to 30 ms (FL's default, 512, is 15).
    pub delay: i32,
    /// The right side's LFO lead, in degrees: `0..=1024` is 0 to 360
    /// (FL's default, 341, is a third of a turn).
    pub stereo_degrees: f32,
    /// How many of the three LFOs run (a speed of 0 stops one).
    pub lfos: usize,
}

impl Chorus {
    pub fn parse(state: &[u8]) -> Option<Chorus> {
        let n = numbers(state);
        if n.len() != 13 {
            return None;
        }
        Some(Chorus {
            delay: n[1],
            stereo_degrees: n[3] as f32 * 360.0 / 1024.0,
            lfos: n[4..7].iter().filter(|speed| **speed > 0).count(),
        })
    }
}

/// Fruity Phaser: a version, then its knobs in order: sweep frequency,
/// minimum and maximum depth, frequency range, stereo, stages, feedback,
/// dry-wet and output gain. Only the stages and the dry-wet are read; the
/// rest's laws are yet to be measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Phaser {
    /// The stages knob (7 by default).
    pub stages: i32,
    /// The wet share, `0..=1` (`0..=1024` saved; FL's default, 512, an
    /// even blend).
    pub wet: f32,
}

impl Phaser {
    pub fn parse(state: &[u8]) -> Option<Phaser> {
        let n = numbers(state);
        (n.len() == 10).then(|| Phaser {
            stages: n[6],
            wet: (n[8] as f32 / 1024.0).clamp(0.0, 1.0),
        })
    }
}

/// Fruity Flangus: order (how many flangers are stacked), depth, speed,
/// delay, spread, then stereo cross, dry and wet, each `-128..=128`
/// across the knob's -100% to 100%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flangus {
    pub order: i32,
    /// As gains, `-1..=1`.
    pub dry: f32,
    pub wet: f32,
}

impl Flangus {
    pub fn parse(state: &[u8]) -> Option<Flangus> {
        let n = numbers(state);
        (n.len() == 8).then(|| Flangus {
            order: n[0],
            dry: n[6] as f32 / 128.0,
            wet: n[7] as f32 / 128.0,
        })
    }
}

/// Fruity Delay 3, as far as its settings are clear. Its levels run to
/// 6000 for 100%, which its input and dry levels start at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Delay3 {
    /// 0 mono, 1 stereo, 2 ping-pong, 3 off (no echoes).
    pub model: i32,
    pub synced: bool,
    /// In 48ths of a step when synced, else milliseconds.
    pub time: i32,
    /// Input wet (how much of the input goes into the echoes), feedback,
    /// wet and dry levels as gains.
    pub input: f32,
    pub feedback: f32,
    pub wet: f32,
    pub dry: f32,
}

/// Delay 3's input wet, time and output wet level, in automation's
/// numbering (its automation clips name them).
pub const DELAY3_INPUT: u16 = 0;
pub const DELAY3_TIME: u16 = 4;
pub const DELAY3_WET: u16 = 23;

impl Delay3 {
    /// A version, input wet, the model, tempo sync, keep pitch, time and
    /// feedback, ..., output wet, tone and dry. Feedback is the knob after
    /// time: the one that changes from project to project, where the next
    /// few sit at their defaults.
    pub fn parse(state: &[u8]) -> Option<Delay3> {
        let n = numbers(state);
        if n.len() < 27 {
            return None;
        }
        let level = |v: i32| v.max(0) as f32 / 6000.0;
        Some(Delay3 {
            input: level(n[1 + DELAY3_INPUT as usize]),
            model: n[2],
            synced: n[3] != 0,
            time: n[1 + DELAY3_TIME as usize],
            feedback: level(n[6]),
            wet: level(n[1 + DELAY3_WET as usize]),
            dry: level(n[26]),
        })
    }
}

/// FL's Transient Processor, as far as its settings are clear: a
/// version, then knobs that run `0..=160` with 80 in the middle. Attack and
/// release are the two of them that move from project to project (the
/// manual lists attack first), each followed later by its shape: 0 sharp,
/// 1 medium, 2 soft.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransientProcessor {
    /// `-1..=1`.
    pub attack: f32,
    pub release: f32,
    pub attack_shape: i32,
    pub release_shape: i32,
}

impl TransientProcessor {
    pub fn parse(state: &[u8]) -> Option<TransientProcessor> {
        let n = numbers(state);
        if n.len() != 12 {
            return None;
        }
        let bipolar = |v: i32| ((v - 80) as f32 / 80.0).clamp(-1.0, 1.0);
        Some(TransientProcessor {
            attack: bipolar(n[1]),
            release: bipolar(n[3]),
            attack_shape: n[5],
            release_shape: n[6],
        })
    }
}

/// A point of a Gross Beat envelope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvelopePoint {
    /// From the envelope's start, in quarter notes.
    pub beat: f64,
    /// `0..=1`: for time, 1 is the sound as it comes and 0 is
    /// [`GrossBeat::TIME_RANGE`] beats back; for volume, the gain.
    pub value: f64,
    /// How the segment that ends here bends (FL's tension, `-1..=1`).
    pub tension: f32,
    /// FL's envelope mode for that segment: 2 holds the value before it.
    pub mode: u8,
}

impl EnvelopePoint {
    pub const HOLD: u8 = 2;
}

/// Gross Beat: 36 time envelopes and 36 volume envelopes, each a bar
/// long, and which of each plays.
#[derive(Debug, Clone, PartialEq)]
pub struct GrossBeat {
    pub time: Vec<EnvelopePoint>,
    pub volume: Vec<EnvelopePoint>,
}

impl GrossBeat {
    /// How far back the bottom of the time envelope plays from, in quarter
    /// notes. Fitted against FL's renders of two Gross Beats (rows 9 and 12
    /// of right-here_2): every other reach lines their transients up worse.
    pub const TIME_RANGE: f64 = 8.0;
    /// An envelope's length, in quarter notes.
    pub const LENGTH: f64 = 4.0;

    /// A header (the chosen time slot is the 32-bit number at byte 4, the
    /// volume slot's at byte 8), then 72 slots, time's first: a
    /// length-prefixed name, 27 bytes ending in the point count, the
    /// points (24 bytes: the distance from the last point in quarter notes
    /// and the value as doubles, the tension as a float, the mode in the
    /// low byte of a word), and 20 more bytes.
    pub fn parse(state: &[u8]) -> Option<GrossBeat> {
        let word = |at: usize| {
            state
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        };
        let (time_slot, volume_slot) = (word(4)?, word(8)?);
        if time_slot >= 36 || volume_slot >= 36 {
            return None;
        }
        let first = state.windows(6).position(|w| w == b"\x05Empty")?;
        let mut at = first;
        let mut slots = Vec::with_capacity(72);
        while slots.len() < 72 {
            let name = usize::from(*state.get(at)?);
            at += 1 + name;
            let count = word(at + 23)?;
            at += 27;
            if count > 4096 {
                return None;
            }
            let mut beat = 0.0;
            let mut points = Vec::with_capacity(count);
            for _ in 0..count {
                let record = state.get(at..at + 24)?;
                let double = |b: &[u8]| f64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
                beat += double(&record[0..8]);
                points.push(EnvelopePoint {
                    beat,
                    value: double(&record[8..16]).clamp(0.0, 1.0),
                    tension: f32::from_le_bytes([record[16], record[17], record[18], record[19]])
                        .clamp(-1.0, 1.0),
                    mode: record[20],
                });
                at += 24;
            }
            at += 20;
            slots.push(points);
        }
        let mut slots = slots.into_iter();
        let time = slots.nth(time_slot)?;
        let volume = slots.nth(35 - time_slot + volume_slot)?;
        Some(GrossBeat { time, volume })
    }
}

/// An envelope's value `beat` quarter notes in (wrapping at
/// [`GrossBeat::LENGTH`]).
pub fn envelope_at(points: &[EnvelopePoint], beat: f64) -> f64 {
    let Some(first) = points.first() else {
        return 1.0;
    };
    let beat = beat.rem_euclid(GrossBeat::LENGTH);
    let next = points.partition_point(|p| p.beat <= beat);
    if next == 0 {
        return first.value;
    }
    let from = points[next - 1];
    let Some(to) = points.get(next) else {
        return from.value;
    };
    let span = to.beat - from.beat;
    if to.mode == EnvelopePoint::HOLD || span <= 0.0 {
        return from.value;
    }
    let t = ((beat - from.beat) / span) as f32;
    f64::from(project::segment_value(
        from.value as f32,
        to.value as f32,
        to.tension,
        t,
    ))
}

#[cfg(test)]
mod tests {
    /// A Gross Beat state with 72 slots, each a flat line but the chosen
    /// ones: time slot `time`, volume slot `volume`.
    fn gross_beat_state(time: u32, volume: u32) -> Vec<u8> {
        let mut out = vec![3, 0, 9, 0];
        out.extend(time.to_le_bytes());
        out.extend(volume.to_le_bytes());
        out.resize(49, 0);
        let point = |out: &mut Vec<u8>, dx: f64, y: f64, tension: f32, mode: u8| {
            out.extend(dx.to_le_bytes());
            out.extend(y.to_le_bytes());
            out.extend(tension.to_le_bytes());
            out.extend([mode, 0, 0, 0]);
        };
        for slot in 0..72u32 {
            let name: &[u8] = if slot % 36 == 0 { b"Empty" } else { b"Slot" };
            out.push(name.len() as u8);
            out.extend(name);
            let mut head = vec![5, 0, 0, 0, 5, 0, 0, 0];
            head.resize(23, 0);
            out.extend(head);
            if slot == time {
                // Half a bar as it comes, then held a beat back.
                out.extend(3u32.to_le_bytes());
                point(&mut out, 0.0, 1.0, 0.0, 2);
                point(&mut out, 2.0, 0.875, 0.0, 2);
                point(&mut out, 2.0, 0.875, 0.0, 2);
            } else if slot == 36 + volume {
                // A gate: a beat on, a beat off.
                out.extend(5u32.to_le_bytes());
                for (dx, y) in [(0.0, 1.0), (1.0, 0.0), (1.0, 1.0), (1.0, 0.0), (1.0, 0.0)] {
                    point(&mut out, dx, y, 0.0, 2);
                }
            } else {
                out.extend(2u32.to_le_bytes());
                point(&mut out, 0.0, 1.0, 0.0, 2);
                point(&mut out, 4.0, 1.0, 0.0, 2);
            }
            out.extend([0u8; 4]);
            out.extend([0xFFu8; 16]);
        }
        out.push(1);
        out
    }

    #[test]
    fn gross_beat_plays_its_chosen_time_and_volume_envelopes() {
        let g = GrossBeat::parse(&gross_beat_state(7, 12)).expect("reads");
        assert_eq!(g.time.len(), 3);
        assert_eq!(envelope_at(&g.time, 1.0), 1.0);
        // Held: the step comes at the point, not before.
        assert_eq!(envelope_at(&g.time, 1.99), 1.0);
        assert_eq!(envelope_at(&g.time, 3.0), 0.875);
        assert_eq!(g.volume.len(), 5);
        assert_eq!(envelope_at(&g.volume, 0.5), 1.0);
        assert_eq!(envelope_at(&g.volume, 1.5), 0.0);
        assert_eq!(envelope_at(&g.volume, 2.5), 1.0);
        // A bar long: round again.
        assert_eq!(envelope_at(&g.volume, 4.5), 1.0);
        assert!(GrossBeat::parse(&gross_beat_state(40, 0)).is_none());
    }

    use super::*;

    fn state(numbers: &[i32]) -> Vec<u8> {
        numbers.iter().flat_map(|n| n.to_le_bytes()).collect()
    }

    /// An EQ 2 as FL 26 saved it in a real project: a low cut, some cuts
    /// and boosts, and a high shelf.
    const EQ2_REAL: [i32; 37] = [
        8, -115, 0, 0, -195, -1030, -1355, 40, 23836, 18204, 25486, 42211, 61838, 64171, 62976,
        36848, 25412, 25412, 5756, 0, 0, 65536, 3, 6, 6, 6, 6, 6, 7, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn eq2_reads_its_bands_in_their_units() {
        let eq = Eq2::parse(&state(&EQ2_REAL)).unwrap();
        assert_eq!(eq.bands[0].kind, Eq2Type::HighPass);
        assert_eq!(eq.bands[6].kind, Eq2Type::HighShelf);
        assert!((eq.bands[5].gain_db - -13.55).abs() < 1e-4);
        // FL's documented points: 0.33333 is 200 Hz, 0.56632 is 1 kHz.
        assert!((eq2_hz(1.0 / 3.0) - 200.0).abs() < 0.1);
        assert!((eq2_hz(0.56632) - 1000.0).abs() < 1.0);
        assert!((eq2_gain_db(2.0 / 3.0) - 6.0).abs() < 1e-3);
        // A two-octave bell is Q 2/3; FL's default bandwidth is about that.
        assert!((octaves_to_q(2.0) - 0.667).abs() < 0.01);
        assert!((eq.bands[1].octaves - 2.0).abs() < 0.01);
        assert!(Eq2::parse(&state(&EQ2_REAL[..20])).is_none());
    }

    #[test]
    fn stereo_tools_and_chorus_read_as_saved() {
        // From real projects.
        let e = StereoEnhancer::parse(&state(&[-18, 256, -54, -248, 1, 0])).unwrap();
        assert_eq!(
            (e.pan, e.volume, e.separation, e.phase_offset),
            (-18, 256, -54, -248)
        );
        let s = StereoShaper::parse(&state(&[0, 0, 12800, 12800, 0, 1552, 0, 0, 1])).unwrap();
        assert_eq!((s.left, s.right, s.right_to_left), (1.0, 1.0, 0.0));
        let c = Chorus::parse(&state(&[
            1, 512, 2250, 341, 1500, 2500, 3500, 23, 0, 0, 0, 2500, 0,
        ]))
        .unwrap();
        assert_eq!((c.delay, c.lfos), (512, 3));
        assert!((c.stereo_degrees - 120.0).abs() < 0.2);
        let f = Flangus::parse(&state(&[6, 231, 848, 0, 0, -66, 0, 112])).unwrap();
        assert_eq!((f.order, f.dry), (6, 0.0));
        assert_eq!(center_on(&state(&[0, 1])), Some(true));
        assert_eq!(
            Balance::parse(&state(&[0, 256])).map(|b| b.volume),
            Some(256)
        );
    }

    #[test]
    fn transient_processor_reads_bipolar_knobs() {
        let t = TransientProcessor::parse(&state(&[1, 160, 38, 40, 80, 2, 2, 0, 80, 1, 5, 12800]))
            .unwrap();
        assert_eq!((t.attack, t.release, t.attack_shape), (1.0, -0.5, 2));
    }

    #[test]
    fn delay3_reads_time_in_steps() {
        let d = Delay3::parse(&state(&[
            1, 6000, 2, 1, 0, 288, 4700, 500, 6000, 3000, 0, 0, 0, 3000, 0, 3000, 0, 3000, 3000, 0,
            0, 0, 60000, 6000, 2583, 0, 6000,
        ]))
        .unwrap();
        assert_eq!((d.model, d.synced, d.time), (2, true, 288));
        assert_eq!(d.dry, 1.0);
        assert!((d.wet - 2583.0 / 6000.0).abs() < 1e-6);
    }

    #[test]
    fn compressor_and_reverb_read_their_units() {
        let c = Compressor::parse(&state(&[2, -118, 32, 36, 0, 629, 4, 0])).unwrap();
        assert_eq!(
            (c.threshold_db, c.ratio, c.gain_db, c.release_ms, c.knee_db),
            (-11.8, 3.2, 3.6, 629.0, 0.0)
        );
        let r = Reeverb2::parse(&state(&[
            10001, 1319, 40, 0, 50, 100, 18, 40, 100, 500, -11, 128, 64, 76, 684, 0,
        ]))
        .unwrap();
        assert_eq!((r.size, r.decay_s, r.dry), (50.0, 1.8, 1.0));
        assert_eq!((r.damping_hz, r.bass, r.crossover_hz), (Some(4000.0), 100.0, 500.0));
        // 76 of 128 sits between the measured -9 dB at 64 and -4 at 96.
        let wet_db = 20.0 * r.wet.log10();
        assert!((wet_db - (-9.0 + 12.0 / 32.0 * 5.0)).abs() < 1e-3, "{wet_db}");
    }

    #[test]
    fn fruity_limiter_reads_its_limiter_and_compressor() {
        // The most common state: the limiter at 0 dB, the compressor off.
        let knobs = [
            1000, 1000, 1000, 1115, 3, 5000, 3, 2882, 1000, 0, 0, 0, 6666, 1, 650, 1000, 0, 4187,
        ];
        let mut numbers = vec![6];
        numbers.extend(knobs);
        numbers.extend(knobs);
        numbers.extend([50, 0, 16_843_009, 0, 0]);
        let l = Limiter::parse(&state(&numbers)).unwrap();
        assert_eq!((l.gain_db, l.ceiling_db, l.threshold_db), (0.0, Some(-0.2), None));
        assert!((l.limiter_release_ms - 39.0).abs() < 0.1);
        assert!(!l.compresses());
        // Its compressor at half threshold and full ratio, the ceiling off.
        numbers[3] = 2000;
        numbers[9] = 500;
        numbers[10] = 1000;
        let l = Limiter::parse(&state(&numbers)).unwrap();
        assert_eq!((l.ceiling_db, l.threshold_db, l.ratio), (None, Some(-8.1), 20.0));
        assert!((l.release_ms - 30.0).abs() < 0.5, "{}", l.release_ms);
        assert!(l.compresses());
    }

    #[test]
    fn reeverb2_curves_pass_through_the_measured_points() {
        assert_eq!(reeverb2_wet_gain(128.0), 1.0);
        assert!((20.0 * reeverb2_wet_gain(64.0).log10() + 9.0).abs() < 1e-4);
        assert!((20.0 * reeverb2_dry_gain(160.0).log10() - 5.6).abs() < 1e-4);
        assert_eq!(reeverb2_wet_gain(0.0), 0.0);
        assert!(reeverb2_wet_gain(8.0) < reeverb2_wet_gain(16.0));
        // At size 50 the tail lasts 1.46 times the knob; a tenth of a
        // second still rings for about 0.4.
        assert!((reeverb2_rt60(2.0, 50.0) - 2.92).abs() < 0.03);
        assert!((reeverb2_rt60(0.1, 50.0) - 0.39).abs() < 0.02);
        assert!((reeverb2_bass(300.0) - 1.42).abs() < 1e-4);
        assert!((reeverb2_bass(100.0) - 1.0).abs() < 1e-6);
    }
}
