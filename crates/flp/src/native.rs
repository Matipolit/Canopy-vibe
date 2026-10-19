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
    /// Bandwidth, in octaves.
    pub octaves: f32,
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

/// Fruity Reeverb 2, as far as its settings are clear.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reeverb2 {
    pub predelay_ms: f32,
    /// `0..=100`.
    pub size: f32,
    pub decay_s: f32,
    /// Dry and wet levels, as gains (1 is FL's 100%).
    pub dry: f32,
    pub wet: f32,
}

/// Reeverb 2's dry and wet levels, in automation's numbering.
pub const REEVERB2_DRY: u16 = 10;
pub const REEVERB2_WET: u16 = 12;

impl Reeverb2 {
    /// Pre-delay in ms, size in percent, decay in tenths of a second, and
    /// the levels with 128 for 100%. The rest (filters, damping, bass,
    /// modulation) aren't carried over.
    pub fn parse(state: &[u8]) -> Option<Reeverb2> {
        let n = numbers(state);
        if n.len() < 14 {
            return None;
        }
        Some(Reeverb2 {
            predelay_ms: n[3] as f32,
            size: n[4] as f32,
            decay_s: n[6] as f32 / 10.0,
            dry: n[1 + REEVERB2_DRY as usize] as f32 / 128.0,
            wet: n[1 + REEVERB2_WET as usize] as f32 / 128.0,
        })
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
    /// Feedback, wet and dry levels as gains.
    pub feedback: f32,
    pub wet: f32,
    pub dry: f32,
}

/// Delay 3's time and output wet level, in automation's numbering (its
/// automation clips name them).
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

#[cfg(test)]
mod tests {
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
        assert!((r.wet - 76.0 / 128.0).abs() < 1e-6);
    }
}
