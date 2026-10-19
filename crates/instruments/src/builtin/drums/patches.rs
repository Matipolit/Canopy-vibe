//! The factory kit: each pad's default settings, which are its parameters'
//! defaults. Most are the TR-808 and TR-909 recipes, rebuilt from layers:
//! a sine dropping in pitch for the kick, metal and noise through
//! highpasses for the hats, bursts of bandpassed noise for the clap.
//! Unused layers wait switched off, ready for more.

use super::LAYERS;
use super::layer::{FilterKind, Mode};
use super::source::Source;

/// One layer's settings, in [`super::Layer`]'s units.
#[derive(Debug, Clone, Copy)]
pub(super) struct LayerPatch {
    pub mode: Mode,
    pub source: Source,
    pub level: f32,
    pub pan: f32,
    pub velocity: f32,
    pub pitch: f32,
    pub shape: f32,
    pub phase: f32,
    pub fm: f32,
    pub pitch_env: f32,
    pub pitch_time: f32,
    pub delay: f32,
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub curve: f32,
    /// How many: 1 is a single strike.
    pub bursts: usize,
    pub gap: f32,
    pub filter: FilterKind,
    pub cutoff: f32,
    pub resonance: f32,
    pub filter_env: f32,
    pub filter_time: f32,
    pub track: f32,
    pub drive: f32,
}

/// One pad's settings, in [`super::Voice`]'s units.
#[derive(Debug, Clone, Copy)]
pub(super) struct PadPatch {
    pub tune: f32,
    pub length: f32,
    pub level: f32,
    pub pan: f32,
    pub drive: f32,
    pub velocity: f32,
    pub choke: usize,
    pub filter: FilterKind,
    pub cutoff: f32,
    pub resonance: f32,
    pub layers: [LayerPatch; LAYERS],
}

/// A layer that's off, set up as a plain sine for when it's turned on.
const SPARE: LayerPatch = LayerPatch {
    mode: Mode::Off,
    source: Source::Sine,
    level: 0.0,
    pan: 0.0,
    velocity: 0.0,
    pitch: 200.0,
    shape: 0.0,
    phase: 0.0,
    fm: 0.0,
    pitch_env: 0.0,
    pitch_time: 30.0,
    delay: 0.0,
    attack: 0.0,
    hold: 0.0,
    decay: 300.0,
    curve: 0.0,
    bursts: 1,
    gap: 10.0,
    filter: FilterKind::Off,
    cutoff: 5000.0,
    resonance: 0.0,
    filter_env: 0.0,
    filter_time: 100.0,
    track: 1.0,
    drive: 0.0,
};

const PAD: PadPatch = PadPatch {
    tune: 0.0,
    length: 100.0,
    level: 0.0,
    pan: 0.0,
    drive: 0.0,
    velocity: 0.5,
    choke: 0,
    filter: FilterKind::Off,
    cutoff: 10_000.0,
    resonance: 0.0,
    layers: [SPARE; LAYERS],
};

/// An oscillator at `hz`, at `level` dB, falling 60 dB in `decay` ms.
const fn tone(source: Source, hz: f32, decay: f32, level: f32) -> LayerPatch {
    LayerPatch {
        mode: Mode::On,
        source,
        pitch: hz,
        decay,
        level,
        ..SPARE
    }
}

/// Noise through `filter` at `cutoff`, at `level` dB, falling 60 dB in
/// `decay` ms. The cutoff stays put when the pad is retuned.
const fn noise(filter: FilterKind, cutoff: f32, decay: f32, level: f32) -> LayerPatch {
    LayerPatch {
        mode: Mode::On,
        source: Source::White,
        filter,
        cutoff,
        decay,
        level,
        track: 0.0,
        ..SPARE
    }
}

/// The pad's six layers: those given, then spares.
const fn layers<const N: usize>(given: [LayerPatch; N]) -> [LayerPatch; LAYERS] {
    let mut all = [SPARE; LAYERS];
    let mut i = 0;
    while i < N {
        all[i] = given[i];
        i += 1;
    }
    all
}

/// The TR-909's kick: a sine dropping two octaves onto its note; under it
/// the clean sub, over it a saturated copy (the body) that decays sooner
/// and carries the punch on small speakers, and a beater click.
pub(super) const KICK: PadPatch = {
    let sweep = LayerPatch {
        pitch_env: 24.0,
        pitch_time: 45.0,
        ..tone(Source::Sine, 50.0, 500.0, -4.4)
    };
    PadPatch {
        drive: 0.15,
        filter: FilterKind::Low12,
        cutoff: 3500.0,
        layers: layers([
            sweep,
            LayerPatch {
                decay: 200.0,
                level: -8.0,
                drive: 0.1,
                ..sweep
            },
            noise(FilterKind::Band, 3500.0, 10.0, -12.4),
        ]),
        ..PAD
    }
};

/// The TR-808's rimshot: two short partials far apart, a click for the
/// stick on the rim, and a highpass that keeps it thin.
pub(super) const RIMSHOT: PadPatch = PadPatch {
    pan: -0.1,
    drive: 0.3,
    filter: FilterKind::High12,
    cutoff: 250.0,
    layers: layers([
        tone(Source::Sine, 455.0, 40.0, -3.7),
        tone(Source::Sine, 1670.0, 28.0, -3.7),
        noise(FilterKind::High12, 2000.0, 4.0, -10.0),
    ]),
    ..PAD
};

/// The TR-909's snare: two modes of the head, bending down a little as the
/// stick lands, and the wires: highpassed noise, with a sharper crack on
/// top that the Snappy knob used to bring up.
pub(super) const SNARE: PadPatch = {
    let head = LayerPatch {
        pitch_env: 4.0,
        pitch_time: 15.0,
        ..tone(Source::Sine, 185.0, 110.0, -8.4)
    };
    PadPatch {
        drive: 0.1,
        filter: FilterKind::Low6,
        cutoff: 14_000.0,
        layers: layers([
            head,
            LayerPatch {
                pitch: 338.6,
                decay: 66.0,
                level: -14.4,
                ..head
            },
            noise(FilterKind::High12, 1800.0, 220.0, -6.6),
            noise(FilterKind::High12, 1800.0, 18.0, -8.0),
        ]),
        ..PAD
    }
};

/// The TR-808's clap: bandpassed noise struck four times in quick
/// succession, then a short, reverb-like tail.
pub(super) const CLAP: PadPatch = {
    let band = LayerPatch {
        resonance: 0.45,
        track: 1.0,
        ..noise(FilterKind::Band, 1200.0, 300.0, 1.4)
    };
    PadPatch {
        layers: layers([
            LayerPatch {
                bursts: 4,
                gap: 10.0,
                decay: 9.0,
                level: 8.3,
                ..band
            },
            LayerPatch {
                delay: 30.0,
                ..band
            },
        ]),
        ..PAD
    }
};

/// The TR-808's hats: six square waves (Metal) and noise, through two
/// highpasses so nearly all of it sits above 5 kHz. Tune moves the filters
/// with the oscillators.
const fn hat(cutoff: f32, metal: f32, noise_level: f32, attack: f32, decay: f32) -> PadPatch {
    let metal = LayerPatch {
        shape: 0.5,
        filter: FilterKind::High24,
        cutoff,
        resonance: 0.35,
        attack,
        ..tone(Source::Metal, 205.3, decay, metal)
    };
    PadPatch {
        pan: -0.2,
        choke: 1,
        filter: FilterKind::Low6,
        cutoff: cutoff * 2.5,
        layers: layers([
            metal,
            LayerPatch {
                source: Source::White,
                level: noise_level,
                ..metal
            },
        ]),
        ..PAD
    }
}

pub(super) const CLOSED_HAT: PadPatch = hat(6450.0, 18.1, -1.9, 0.0, 45.0);
/// Softer at the start: the foot closing the cymbals rather than a stick.
pub(super) const PEDAL_HAT: PadPatch = hat(5170.0, 16.1, -0.4, 3.0, 80.0);
pub(super) const OPEN_HAT: PadPatch = hat(6450.0, 18.1, -1.9, 0.0, 450.0);

/// A tom: the head's fundamental and its first overtone (the membrane's
/// (1,1) mode at 1.59 times it), both falling in pitch as the skin relaxes,
/// with a touch of noise for the stick.
const fn tom(hz: f32, decay: f32, drop: f32, pan: f32) -> PadPatch {
    let head = LayerPatch {
        pitch_env: drop,
        pitch_time: decay * 0.4,
        ..tone(Source::Sine, hz, decay, -3.1)
    };
    PadPatch {
        pan,
        drive: 0.1,
        layers: layers([
            head,
            LayerPatch {
                pitch: hz * 1.59,
                decay: decay * 0.5,
                level: -13.6,
                ..head
            },
            noise(FilterKind::Band, 3000.0, 25.0, -17.1),
        ]),
        ..PAD
    }
}

pub(super) const LOW_TOM: PadPatch = tom(90.0, 450.0, 5.0, 0.3);
pub(super) const MID_TOM: PadPatch = tom(130.0, 380.0, 4.0, 0.0);
pub(super) const HIGH_TOM: PadPatch = tom(185.0, 320.0, 4.0, -0.3);

/// Crash and ride: metal and noise split into a low band that carries the
/// stick's clang and dies quickly, and a high band that carries the long
/// wash; plus a bell's partials.
#[allow(clippy::too_many_arguments)]
const fn cymbal(
    hz: f32,
    spread: f32,
    low_hz: f32,
    low: [f32; 3],
    high_hz: f32,
    high: [f32; 2],
    bloom: f32,
    decay: f32,
    bell: [f32; 3],
    pan: f32,
) -> PadPatch {
    let clang = LayerPatch {
        shape: spread,
        filter: FilterKind::Band,
        cutoff: low_hz,
        resonance: 0.3,
        ..tone(Source::Metal, hz, low[2], low[0])
    };
    let wash = LayerPatch {
        filter: FilterKind::High12,
        cutoff: high_hz,
        resonance: 0.0,
        attack: bloom,
        decay,
        level: high[0],
        ..clang
    };
    PadPatch {
        pan,
        filter: FilterKind::Low6,
        cutoff: high_hz * 3.0,
        layers: layers([
            clang,
            LayerPatch {
                source: Source::White,
                level: low[1],
                ..clang
            },
            wash,
            LayerPatch {
                source: Source::White,
                level: high[1],
                ..wash
            },
            LayerPatch {
                shape: 0.3,
                ..tone(Source::Bell, bell[0], bell[1], bell[2])
            },
        ]),
        ..PAD
    }
}

pub(super) const CRASH: PadPatch = cymbal(
    245.0,
    0.45,
    3550.0,
    [-2.4, -12.0, 440.0],
    5000.0,
    [2.4, -7.1],
    6.0,
    2200.0,
    [470.0, 660.0, -18.6],
    -0.35,
);

pub(super) const RIDE: PadPatch = cymbal(
    296.0,
    0.42,
    2600.0,
    [0.7, -18.4, 240.0],
    4000.0,
    [3.2, -15.9],
    1.0,
    3000.0,
    [800.0, 1350.0, -9.8],
    0.35,
);

/// The TR-808's cowbell: two square waves just under a fifth apart (540 and
/// 800 Hz) through a resonant filter, with a bright spike over a rounder ring.
pub(super) const COWBELL: PadPatch = {
    let spike = LayerPatch {
        filter: FilterKind::Band,
        cutoff: 2600.0,
        ..tone(Source::Square, 540.0, 35.0, -10.0)
    };
    let ring = LayerPatch {
        filter: FilterKind::Low12,
        resonance: 0.8,
        decay: 280.0,
        level: -16.0,
        ..spike
    };
    PadPatch {
        pan: 0.15,
        layers: layers([
            spike,
            LayerPatch {
                pitch: 800.0,
                ..spike
            },
            ring,
            LayerPatch {
                pitch: 800.0,
                ..ring
            },
        ]),
        ..PAD
    }
};

/// Shaker: a band of high noise that swells in as the beads swing and
/// dies away. The second layer, off, is a tambourine's jingles: metal
/// through a ringing filter.
pub(super) const SHAKER: PadPatch = PadPatch {
    pan: 0.25,
    layers: layers([
        LayerPatch {
            resonance: 0.4,
            attack: 12.0,
            track: 1.0,
            ..noise(FilterKind::Band, 6500.0, 125.0, 1.5)
        },
        LayerPatch {
            mode: Mode::Off,
            shape: 0.35,
            filter: FilterKind::Band,
            cutoff: 8000.0,
            resonance: 0.6,
            ..tone(Source::Metal, 2140.0, 225.0, 11.6)
        },
    ]),
    ..PAD
};

/// A conga: a drum head's modes, sliding down a little, and a slap.
pub(super) const CONGA: PadPatch = PadPatch {
    pan: 0.2,
    layers: layers([
        LayerPatch {
            shape: 0.4,
            pitch_env: 3.0,
            pitch_time: 20.0,
            ..tone(Source::Membrane, 280.0, 450.0, -2.0)
        },
        LayerPatch {
            resonance: 0.2,
            ..noise(FilterKind::Band, 2200.0, 15.0, -14.0)
        },
    ]),
    ..PAD
};

/// Claves: a hard, pure ring high up, and the tick of the strike.
pub(super) const CLAVE: PadPatch = PadPatch {
    pan: -0.25,
    layers: layers([
        tone(Source::Sine, 2500.0, 80.0, -3.0),
        tone(Source::Impulse, 6000.0, 5.0, -12.0),
    ]),
    ..PAD
};
