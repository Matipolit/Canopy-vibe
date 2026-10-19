//! The three guitars, as numbers: their strings, bodies and pickups.
//!
//! Inharmonicity starts from each string's gauge and tension,
//! `B = π³ E d⁴ / (64 T L²)` (Fletcher & Rossing 1998, §2.18), with only the
//! core counting for a wound string's stiffness. Decay times, the two
//! polarizations, inharmonicity and the excitation's brightness were then
//! set by measuring recordings of real guitars the same way the guitar's
//! tests measure the model (partial frequencies fitted for `B`; per-partial
//! decay early and late; envelope wobble; octave-band balance; pitch glide):
//!
//! - nylon: single open-string notes, picked and plucked (Freesound, SpeedY,
//!   pack 469, CC0);
//! - electric: a Fender Stratocaster's neck pickup, direct (IDMT-SMT-Guitar,
//!   Kehling et al. 2014) and clean notes by ntnvsl (Freesound pack 34051,
//!   CC0); palm mutes from EG-IPT (Fiorini et al. 2025);
//! - steel: a low E by FenrirFangs (Freesound 234739, CC0) and NSynth's
//!   acoustic guitars (Engel et al. 2017);
//! - strums: GuitarSet's hexaphonic recordings (Xi et al. 2018).
//!
//! What they showed, and the model follows: open A strings ring 9 to 11 s;
//! the treble of plain strings rings almost as long as their fundamental
//! (about 4 s at 2 to 4 kHz) while wound strings lose it much sooner; loss
//! grows roughly in proportion to frequency between a few hundred hertz and
//! a few kilohertz; partials 5 to 12 decay about twice as fast in the first
//! half second as later and wobble by 1 to 5 dB.
//!
//! Sources for the rest:
//! - String decay: Välimäki, Huopaniemi, Karjalainen & Jánosy 1996; Erkut,
//!   Välimäki, Karjalainen & Laurson 2000; Woodhouse 2004.
//! - Two polarizations: Weinreich 1977; Gough 1981; Karjalainen, Välimäki &
//!   Tolonen 1998.
//! - Body modes: Fletcher & Rossing 1998, ch. 9; Christensen & Vistisen
//!   1980; Elejabarrieta, Ezcurra & Santamaría 2002.
//! - Pickups: Mohamad, Dixon & Harte 2017; Zollner 2019; Lemme's measured
//!   pickup table; Paiva, Pakarinen & Välimäki 2012.

use super::fretboard::STRINGS;

/// Which guitar the `model` parameter picks; its labels' order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Steel = 0,
    Nylon = 1,
    Electric = 2,
}

impl ModelKind {
    pub const LABELS: &'static [&'static str] = &["Steel acoustic", "Nylon classical", "Electric"];

    pub fn from_index(index: usize) -> ModelKind {
        match index {
            1 => ModelKind::Nylon,
            2 => ModelKind::Electric,
            _ => ModelKind::Steel,
        }
    }

    pub fn spec(self) -> &'static Model {
        match self {
            ModelKind::Steel => &STEEL,
            ModelKind::Nylon => &NYLON,
            ModelKind::Electric => &ELECTRIC,
        }
    }
}

/// One string of a set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StringSpec {
    /// Inharmonicity coefficient `B` of the open string. Fretting shortens
    /// the string and raises it by the inverse square of the length.
    pub inharmonicity: f32,
    /// How long the open string's fundamental rings relative to the open A
    /// string's, which the `sustain` knob sets.
    pub decay: f32,
    /// How long its treble (at 4 kHz) rings over how long its fundamental
    /// does, at the tone knob's middle: wound strings are dull, plain ones
    /// ring nearly as long up high as down low.
    pub treble: f32,
    /// Wound strings scrape under a pick and squeak under a sliding finger.
    pub wound: bool,
}

const fn string(inharmonicity: f32, decay: f32, treble: f32, wound: bool) -> StringSpec {
    StringSpec {
        inharmonicity,
        decay,
        treble,
        wound,
    }
}

/// A body mode: frequency, Q, level (relative to the direct sound) and
/// where it is heard more, -1 the left microphone to 1 the right.
pub type Mode = (f32, f32, f32, f32);

/// An acoustic guitar's body: its main low modes, measured, and the dense
/// upper ones described statistically (their exact places differ from one
/// guitar to the next; only their spacing, damping and level matter).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodySpec {
    /// The air (Helmholtz) mode, the top's and back's first modes and their
    /// coupled partners, in frequency order.
    pub modes: &'static [Mode],
    /// How many upper modes, and the band they fill.
    pub dense_count: usize,
    pub dense_from_hz: f32,
    pub dense_to_hz: f32,
    /// Their Q at the bottom and the top of the band.
    pub dense_q: [f32; 2],
    /// Their level at the bottom and the top of the band.
    pub dense_level: [f32; 2],
    /// The bridge force heard straight through, smoothed: what the top
    /// radiates between its modes.
    pub direct: f32,
    /// Above this the top radiates less and less.
    pub treble_hz: f32,
    /// How strongly a note on one of the main modes loses its fundamental
    /// into the body: "dead spots" (Fletcher & Rossing 1998, §9.7).
    pub dead_spots: f32,
    /// Seed for the upper modes' places.
    pub seed: u32,
}

/// Where an electric's pickups sit, and how they sound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickupSpec {
    /// Distance from the bridge saddle, metres: neck, middle, bridge.
    pub positions: [f32; 3],
    /// Width of string a pickup hears, metres (its aperture).
    pub aperture: f32,
    /// A humbucker's two coils are this far apart; 0 for a single coil.
    pub coil_spacing: f32,
    /// Resonance of the coil with the cable and the amp's input: Hz and Q.
    pub resonance_hz: f32,
    pub resonance_q: f32,
}

/// Everything that makes one guitar sound like itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    /// Low E first.
    pub strings: [StringSpec; STRINGS],
    /// Nut to saddle, metres.
    pub scale: f32,
    /// Fretted notes ring shorter: the fundamental's decay time scales as
    /// `(f_open / f)^fret_decay`.
    pub fret_decay: f32,
    /// How much faster the vertical polarization (into the top) loses its
    /// energy than the horizontal one, at the fundamental and in the treble:
    /// the first, faster stage of the two-stage decay. Recordings show it
    /// most in the partials above the first few.
    pub vertical_loss: [f32; 2],
    /// Typical frequency difference between the two polarizations, in
    /// cents: the beating.
    pub split_cents: f32,
    /// How much of the horizontal polarization reaches the body or the
    /// pickup, relative to the vertical.
    pub horizontal_gain: f32,
    /// Typical angle of a pick stroke to the top, and of a finger's,
    /// radians: how the pluck divides between the polarizations.
    pub pick_angle: f32,
    pub finger_angle: f32,
    /// How much a full-velocity pluck raises the pitch at first, in cents,
    /// as the stretched string's tension rises (Tolonen, Välimäki &
    /// Karjalainen 2000).
    pub tension_cents: f32,
    /// A fretted note is pulled sharp by pressing the string down to the
    /// fret: cents at the 12th fret.
    pub fret_sharpening: f32,
    /// Strength of the sympathetic coupling through the bridge.
    pub coupling: f32,
    /// Played with the fingers unless told otherwise.
    pub fingers: bool,
    /// An acoustic's body, or `None` for a solid body heard through pickups.
    pub body: Option<&'static BodySpec>,
    pub pickups: Option<&'static PickupSpec>,
    /// Output trim, dB, so the models play at similar loudness.
    pub trim_db: f32,
}

// ------------------------------------------------------------------ steel

/// A dreadnought-sized flat-top: the air mode near 100 Hz, the top's
/// first mode near 200 Hz and its coupled partner near 230 Hz, then the
/// top's and back's higher modes.
const STEEL_BODY: BodySpec = BodySpec {
    modes: &[
        (101.0, 18.0, 1.25, -0.35),
        (186.0, 22.0, 0.85, 0.25),
        (228.0, 26.0, 0.55, -0.1),
        (284.0, 28.0, 0.35, 0.3),
        (376.0, 30.0, 0.45, -0.3),
        (447.0, 32.0, 0.32, 0.2),
        (533.0, 32.0, 0.28, -0.25),
        (645.0, 30.0, 0.3, 0.3),
    ],
    dense_count: 56,
    dense_from_hz: 720.0,
    dense_to_hz: 7500.0,
    dense_q: [30.0, 16.0],
    dense_level: [0.3, 0.12],
    direct: 0.35,
    treble_hz: 6500.0,
    dead_spots: 0.35,
    seed: 0x5EE1,
};

/// Phosphor bronze lights (.012 to .053) on a 645 mm scale.
const STEEL: Model = Model {
    strings: [
        string(1.0e-4, 1.35, 0.2, true),
        string(7.0e-5, 1.0, 0.27, true),
        string(4.0e-5, 0.92, 0.33, true),
        string(2.0e-5, 0.85, 0.38, true),
        string(6.0e-5, 0.7, 0.55, false),
        string(2.5e-5, 0.6, 0.65, false),
    ],
    scale: 0.645,
    fret_decay: 0.6,
    vertical_loss: [1.15, 2.5],
    split_cents: 3.0,
    horizontal_gain: 0.45,
    pick_angle: 0.45,
    finger_angle: 0.75,
    tension_cents: 4.0,
    fret_sharpening: 3.0,
    coupling: 0.0016,
    fingers: false,
    body: Some(&STEEL_BODY),
    pickups: None,
    trim_db: 0.0,
};

// ------------------------------------------------------------------ nylon

/// A classical guitar: a lighter, more responsive top than a steel-string's,
/// with the air mode near 95 Hz and the top's first mode near 190 Hz.
const NYLON_BODY: BodySpec = BodySpec {
    modes: &[
        (96.0, 16.0, 1.6, -0.35),
        (193.0, 20.0, 1.3, 0.25),
        (245.0, 22.0, 0.5, -0.1),
        (390.0, 26.0, 0.45, 0.3),
        (480.0, 28.0, 0.35, -0.3),
        (560.0, 28.0, 0.3, 0.2),
        (680.0, 28.0, 0.24, -0.25),
    ],
    dense_count: 56,
    dense_from_hz: 760.0,
    dense_to_hz: 6500.0,
    dense_q: [26.0, 14.0],
    dense_level: [0.22, 0.06],
    direct: 0.15,
    treble_hz: 5000.0,
    dead_spots: 0.45,
    seed: 0xC1A5,
};

/// Normal-tension nylon trebles (0.71 to 1.03 mm) and silver-wound
/// multifilament basses on a 650 mm scale: very little stiffness in the
/// basses, and nylon's high internal damping darkens the trebles.
const NYLON: Model = Model {
    strings: [
        string(1.4e-5, 1.4, 0.14, true),
        string(1.3e-5, 1.0, 0.22, true),
        string(1.2e-5, 0.88, 0.27, true),
        string(8.0e-5, 0.5, 0.6, false),
        string(7.2e-5, 0.47, 0.7, false),
        string(5.5e-5, 0.38, 0.9, false),
    ],
    scale: 0.65,
    fret_decay: 0.65,
    vertical_loss: [1.1, 2.6],
    split_cents: 2.2,
    horizontal_gain: 0.45,
    pick_angle: 0.45,
    finger_angle: 0.8,
    tension_cents: 6.0,
    fret_sharpening: 4.5,
    coupling: 0.0022,
    fingers: true,
    body: Some(&NYLON_BODY),
    pickups: None,
    trim_db: 1.5,
};

// --------------------------------------------------------------- electric

/// A Stratocaster's three single coils: 160, 100 and 41 mm from the
/// saddle (Mohamad, Dixon & Harte 2017 measured 160, 102 and 38.5 to 48 mm
/// on a slanted bridge pickup), each hearing about 8 mm of string (Zollner
/// 2019 measured a 5 to 10 mm window), loaded to a resonance near 3.8 kHz
/// (Lemme: 3.8 kHz with 680 pF; Zollner: Q about 3 with pots and amp).
const SINGLE_COILS: PickupSpec = PickupSpec {
    positions: [0.160, 0.100, 0.041],
    aperture: 0.008,
    coil_spacing: 0.0,
    resonance_hz: 3800.0,
    resonance_q: 2.6,
};

/// Humbuckers where a Les Paul has them, 147 and 45 mm from the saddle
/// (Zollner 2019), plus one in the middle; two coils 18 mm apart, resonating
/// lower and broader (Lemme: Gibson 490, 2.9 kHz with 680 pF, Q 2 to 2.5).
const HUMBUCKERS: PickupSpec = PickupSpec {
    positions: [0.147, 0.100, 0.045],
    aperture: 0.008,
    coil_spacing: 0.018,
    resonance_hz: 2900.0,
    resonance_q: 2.0,
};

/// The pickups an electric has, by the `pickup_type` parameter.
pub fn pickups(humbuckers: bool) -> &'static PickupSpec {
    if humbuckers {
        &HUMBUCKERS
    } else {
        &SINGLE_COILS
    }
}

/// Nickel-wound 10s (.010 to .046) on a 648 mm scale: a solid body takes
/// little energy from the strings, so they ring long and both
/// polarizations decay alike.
const ELECTRIC: Model = Model {
    strings: [
        string(1.6e-4, 1.4, 0.17, true),
        string(9.0e-5, 1.0, 0.29, true),
        string(6.0e-5, 0.92, 0.36, true),
        string(8.0e-5, 0.8, 0.53, false),
        string(3.0e-5, 0.86, 0.45, false),
        string(2.0e-5, 0.62, 0.6, false),
    ],
    scale: 0.648,
    fret_decay: 0.5,
    vertical_loss: [1.05, 1.8],
    split_cents: 1.4,
    horizontal_gain: 0.45,
    pick_angle: 0.4,
    finger_angle: 0.7,
    tension_cents: 2.5,
    fret_sharpening: 2.0,
    coupling: 0.0006,
    fingers: false,
    body: None,
    pickups: Some(&SINGLE_COILS),
    trim_db: 0.0,
};

/// The pickup switch's positions.
pub const PICKUP_LABELS: &[&str] = &[
    "Neck",
    "Neck + Middle",
    "Middle",
    "Middle + Bridge",
    "Bridge",
];

/// How much of each pickup (neck, middle, bridge) a switch position mixes.
/// Two pickups in parallel each give a little more than half.
pub fn pickup_mix(position: usize) -> [f32; 3] {
    match position {
        0 => [1.0, 0.0, 0.0],
        1 => [0.6, 0.6, 0.0],
        2 => [0.0, 1.0, 0.0],
        3 => [0.0, 0.6, 0.6],
        _ => [0.0, 0.0, 1.0],
    }
}
