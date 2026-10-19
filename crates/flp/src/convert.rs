//! FL plugins' settings, carried over to the canopy effects that stand in
//! for them, and FL automation of plugin parameters, turned into canopy
//! automation of the matching ones.
//!
//! Only where a setting's meaning is clear: an effect FL saves in a way
//! that isn't understood starts at canopy's defaults instead of at wrong
//! values.

use effects::EffectRef;
use effects::chorus::{CHORUS, FLANGER, PHASER};

use crate::model as fl;
use crate::native::{
    self, Balance, Compressor, DELAY3_WET, Delay3, EQ2_FREQ, EQ2_LEVEL, EQ2_WIDTH, Eq2, Eq2Type,
    Flangus, REEVERB2_DRY, REEVERB2_WET, Reeverb2, StereoEnhancer, StereoShaper,
};
use crate::plugins;

mod thirdparty;

/// What came across from an FL plugin's settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    /// Canopy parameter ids and values, in the effect's own units.
    pub params: Vec<(String, f32)>,
    /// How much is carried over: everything that matters, or some of it.
    pub fidelity: Fidelity,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fidelity {
    /// Nothing: the effect starts at its defaults.
    #[default]
    None,
    /// The main settings; the rest stay at canopy's defaults.
    Partial,
    /// Everything the canopy effect can do of what FL's does.
    Full,
}

/// FL's slot dry/wet knob at 100%.
pub const FULL_MIX: i32 = 12_800;

/// The settings of FL effect `plugin` for canopy effect `effect`, with the
/// slot's dry/wet `mix` (FL's `0..=12800`). `automated(n)` tells whether the
/// plugin's parameter `n` is automated, so that what the automation moves
/// is kept switched on.
pub fn effect_settings(
    plugin: &fl::Plugin,
    effect: &str,
    mix: Option<i32>,
    automated: &dyn Fn(u16) -> bool,
) -> Settings {
    let name = plugins::normalize(&plugin.display_name());
    let mut settings = match (name.as_str(), effect) {
        ("fruity parametric eq 2", plugins::EQ) => {
            Eq2::parse(&plugin.state).map_or_else(Settings::default, |eq| eq2(&eq, automated))
        }
        ("fruity compressor", plugins::COMPRESSOR) => {
            Compressor::parse(&plugin.state).map_or_else(Settings::default, compressor)
        }
        ("fruity reeverb 2", plugins::REVERB) => {
            Reeverb2::parse(&plugin.state).map_or_else(Settings::default, reeverb2)
        }
        ("fruity delay 3", plugins::DELAY) => {
            Delay3::parse(&plugin.state).map_or_else(Settings::default, delay3)
        }
        ("transient processor", plugins::TRANSIENT) => {
            native::TransientProcessor::parse(&plugin.state)
                .map_or_else(Settings::default, transient_processor)
        }
        ("fruity balance", plugins::UTILITY) => {
            Balance::parse(&plugin.state).map_or_else(Settings::default, balance)
        }
        ("fruity stereo enhancer", plugins::UTILITY) => {
            StereoEnhancer::parse(&plugin.state).map_or_else(Settings::default, stereo_enhancer)
        }
        ("fruity stereo shaper", plugins::UTILITY) => {
            StereoShaper::parse(&plugin.state).map_or_else(Settings::default, stereo_shaper)
        }
        ("fruity center", plugins::UTILITY) => native::center_on(&plugin.state)
            .map_or_else(Settings::default, |on| {
                Settings::new(&[("dc", on_off(on))], Fidelity::Full)
            }),
        ("fruity chorus", plugins::CHORUS) => {
            native::Chorus::parse(&plugin.state).map_or_else(Settings::default, fruity_chorus)
        }
        ("fruity flangus", plugins::CHORUS) => {
            Flangus::parse(&plugin.state).map_or_else(Settings::default, flangus)
        }
        _ => {
            thirdparty::settings(plugin, &name, effect).unwrap_or_else(|| character(&name, effect))
        }
    };
    // The slot's own dry/wet, on top of the effect's.
    if let Some(mix) = mix.filter(|m| *m < FULL_MIX)
        && let Some(param) = mix_param(effect)
    {
        let own = settings
            .params
            .iter()
            .find(|(id, _)| id == "mix")
            .map_or(param.default_value(), |(_, v)| *v);
        let value = own * mix.max(0) as f32 / FULL_MIX as f32;
        settings.params.retain(|(id, _)| id != "mix");
        settings.params.push(("mix".to_owned(), value));
    }
    // Inside the canopy parameters' ranges, which FL's can go beyond.
    if let Some(descriptor) = EffectRef::builtin(effect).descriptor() {
        settings.params.retain_mut(|(id, value)| {
            let Some(param) = descriptor.params.iter().find(|p| p.id == id) else {
                debug_assert!(false, "{effect} has no parameter {id}");
                return false;
            };
            *value = param.clamp(*value);
            value.is_finite()
        });
    }
    settings
}

impl Settings {
    fn new(params: &[(&str, f32)], fidelity: Fidelity) -> Settings {
        Settings {
            params: params
                .iter()
                .map(|(id, value)| ((*id).to_owned(), *value))
                .collect(),
            fidelity,
        }
    }
}

fn on_off(on: bool) -> f32 {
    if on { 1.0 } else { 0.0 }
}

/// FL's `0..=320` gain knobs (Fruity Balance's, Stereo Enhancer's): 256 is
/// 0 dB, on the insert fader's curve. At 0 the sound is muted.
fn knob_volume(volume: i32) -> [(&'static str, f32); 2] {
    let raw = f64::from(volume.max(0)) / 320.0 * 16_000.0;
    [
        ("gain", crate::insert_volume_db(raw)),
        ("mute", on_off(volume <= 0)),
    ]
}

/// FL's `-128..=127` pan knobs as the utility's balance.
fn knob_balance(pan: i32) -> (&'static str, f32) {
    ("balance", pan as f32 / 128.0 * 100.0)
}

/// For plugins whose settings can't be read, the canopy effect's character
/// closest to theirs, so they start nearer their sound than the defaults.
fn character(name: &str, effect: &str) -> Settings {
    let starts = |prefix: &str| name.starts_with(prefix);
    let params: &[(&str, f32)] = match effect {
        plugins::COMPRESSOR if starts("leveling") || starts("cla-2a") || starts("la-2a") => {
            // Opto, slow and gentle, as a leveling amplifier is.
            &[
                ("model", 2.0),
                ("ratio", 3.0),
                ("attack", 10.0),
                ("release", 300.0),
            ]
        }
        plugins::COMPRESSOR if starts("supercharger") => &[("model", 3.0), ("ratio", 2.0)],
        plugins::WAVESHAPER
            if starts("msaturator") || starts("saturn") || starts("decapitator") =>
        {
            &[("shape", 2.0)]
        }
        plugins::WAVESHAPER if starts("mwavefolder") => &[("shape", 3.0)],
        // A flanger wants a short delay.
        plugins::CHORUS if name.contains("flang") => &[("mode", FLANGER as f32), ("delay", 2.5)],
        plugins::CHORUS if name.contains("phas") => &[("mode", PHASER as f32)],
        _ => &[],
    };
    Settings {
        params: params
            .iter()
            .map(|(id, value)| ((*id).to_owned(), *value))
            .collect(),
        fidelity: Fidelity::None,
    }
}

/// The canopy effect's dry/wet parameter (`0..=100`), if it has one.
fn mix_param(effect: &str) -> Option<&'static instruments::ParamDescriptor> {
    EffectRef::builtin(effect)
        .descriptor()?
        .params
        .iter()
        .find(|p| p.id == "mix")
}

/// FL EQ 2 bands onto canopy EQ bands 1 to 7; the rest stay off. Flat
/// bells and shelves are left off too, unless automation moves them.
fn eq2(eq: &Eq2, automated: &dyn Fn(u16) -> bool) -> Settings {
    let mut params = Vec::new();
    let mut set = |band: usize, which: &str, value: f32| {
        params.push((format!("b{}_{which}", band + 1), value));
    };
    for band in 0..effects::eq::BANDS {
        let Some(fl_band) = eq.bands.get(band) else {
            set(band, "on", 0.0);
            continue;
        };
        let index = band as u16;
        let moved = [EQ2_LEVEL, EQ2_FREQ, EQ2_WIDTH]
            .iter()
            .any(|base| automated(base + index));
        let kind = match fl_band.kind {
            Eq2Type::Off => None,
            Eq2Type::LowPass => Some(effects::eq::BandType::HighCut),
            Eq2Type::BandPass => Some(effects::eq::BandType::BandPass),
            Eq2Type::HighPass => Some(effects::eq::BandType::LowCut),
            Eq2Type::Notch => Some(effects::eq::BandType::Notch),
            Eq2Type::LowShelf => Some(effects::eq::BandType::LowShelf),
            Eq2Type::Peaking => Some(effects::eq::BandType::Bell),
            Eq2Type::HighShelf => Some(effects::eq::BandType::HighShelf),
        };
        let flat = fl_band.kind.has_gain() && fl_band.gain_db.abs() < 0.05;
        let Some(kind) = kind.filter(|_| !flat || moved) else {
            set(band, "on", 0.0);
            continue;
        };
        let type_index = effects::eq::BandType::ALL
            .iter()
            .position(|k| *k == kind)
            .unwrap_or(2);
        set(band, "on", 1.0);
        set(band, "type", type_index as f32);
        set(band, "freq", fl_band.hz);
        set(band, "gain", fl_band.gain_db);
        let q = if kind.has_slope() {
            // FL's bandwidth means nothing to its passes.
            std::f32::consts::FRAC_1_SQRT_2
        } else {
            native::octaves_to_q(fl_band.octaves)
        };
        set(band, "q", q);
        if kind.has_slope() {
            set(band, "slope", fl_band.order.unsigned_abs().min(3) as f32);
        }
    }
    params.push(("output".to_owned(), eq.main_db));
    Settings {
        params,
        fidelity: Fidelity::Full,
    }
}

fn balance(b: Balance) -> Settings {
    let mut params = knob_volume(b.volume).to_vec();
    params.push(knob_balance(b.pan));
    Settings::new(&params, Fidelity::Full)
}

/// Its separation as the utility's width, by eye: the knob's ends push
/// the sides about as far apart as width's do. Its phase offset isn't
/// carried over: how far the knob's numbers delay a side is unknown.
fn stereo_enhancer(e: StereoEnhancer) -> Settings {
    let mut params = knob_volume(e.volume).to_vec();
    params.extend([
        knob_balance(e.pan),
        ("width", 100.0 - e.separation as f32 / 96.0 * 100.0),
        ("invert_left", on_off(e.invert == 1)),
        ("invert_right", on_off(e.invert == 2)),
    ]);
    Settings::new(&params, Fidelity::Partial)
}

/// The mixer matrix as the utility's gain, balance, width and polarity:
/// exactly when it keeps or flips the sides, mixes them evenly or only
/// scales them; otherwise its symmetric part. Its delay and phase aren't
/// carried over.
fn stereo_shaper(s: StereoShaper) -> Settings {
    let db = |gain: f32| 20.0 * gain.abs().max(1e-6).log10();
    let mut params = Vec::new();
    let (a, b, c, d) = (s.left, s.right_to_left, s.left_to_right, s.right);
    let mut exact = true;
    if b == 0.0 && c == 0.0 {
        // Each side on its own: a gain each, perhaps flipped.
        let loud = a.abs().max(d.abs());
        params.push(("gain", db(loud)));
        if loud > 0.0 && a.abs() != d.abs() {
            // The quieter side fades along a quarter cosine.
            let quiet = a.abs().min(d.abs()) / loud;
            let turn = quiet.acos() / std::f32::consts::FRAC_PI_2 * 100.0;
            params.push(("balance", if a.abs() < d.abs() { turn } else { -turn }));
        }
        params.extend([
            ("invert_left", on_off(a < 0.0)),
            ("invert_right", on_off(d < 0.0)),
            ("mute", on_off(loud == 0.0)),
        ]);
    } else {
        // Symmetric: mid scaled by a + b, side by a - b.
        exact = a == d && b == c;
        let (same, cross) = ((a + d) / 2.0, (b + c) / 2.0);
        let mid = same + cross;
        let side = same - cross;
        if mid.abs() < 1e-6 {
            params.extend([
                ("channels", effects::utility::SIDE as f32),
                ("gain", db(side)),
            ]);
        } else {
            params.extend([
                ("gain", db(mid)),
                ("width", side / mid * 100.0),
                ("invert_left", on_off(mid < 0.0)),
                ("invert_right", on_off(mid < 0.0)),
            ]);
        }
    }
    let fidelity = if exact && s.delay == 0 && s.phase == 0 {
        Fidelity::Full
    } else {
        Fidelity::Partial
    };
    Settings::new(&params, fidelity)
}

/// Its delay, the right side's lead (folded to canopy's 0 to 180°) and a
/// voice per running LFO. Its depth and LFO speeds aren't carried over:
/// their units are unknown.
fn fruity_chorus(c: native::Chorus) -> Settings {
    let degrees = c.stereo_degrees.rem_euclid(360.0);
    let params = [
        ("mode", CHORUS as f32),
        ("delay", c.delay as f32 / 1024.0 * 30.0),
        ("spread", degrees.min(360.0 - degrees)),
        ("voices", c.lfos.clamp(1, 4) as f32 - 1.0),
    ];
    Settings::new(&params, Fidelity::Partial)
}

/// Stacked flangers: the flanger, with Flangus's dry and wet levels.
fn flangus(f: Flangus) -> Settings {
    let mut params = vec![("mode", FLANGER as f32), ("delay", 2.5)];
    let level = f.dry.abs() + f.wet.abs();
    if level > 0.0 {
        params.extend([
            ("mix", 100.0 * f.wet.abs() / level),
            ("output", 20.0 * level.log10()),
        ]);
    }
    Settings::new(&params, Fidelity::Partial)
}

/// The share of a dry and a wet level that's wet, as a mix.
fn wet_share(dry: f32, wet: f32) -> f32 {
    if dry + wet > 0.0 {
        100.0 * wet / (dry + wet)
    } else {
        0.0
    }
}

/// Its model, time, feedback and levels. Its filter, modulation,
/// diffusion and distortion aren't carried over.
fn delay3(d: Delay3) -> Settings {
    let mut params = vec![
        ("mode", if d.model == 2 { 1.0 } else { 0.0 }),
        ("feedback", 100.0 * d.feedback),
        // The model "Off" makes no echoes.
        (
            "mix",
            if d.model == 3 {
                0.0
            } else {
                wet_share(d.dry, d.wet)
            },
        ),
    ];
    if d.synced {
        // A step is a sixteenth: a 48th of one is 1/192 of a quarter.
        let quarters = f64::from(d.time) / 192.0;
        let distance = |q: f64| (q / quarters.max(1e-3)).ln().abs();
        let note = effects::delay::NOTES
            .iter()
            .enumerate()
            .min_by(|a, b| distance(a.1.1).total_cmp(&distance(b.1.1)))
            .map_or(6, |(i, _)| i);
        params.extend([("sync", 0.0), ("note", note as f32)]);
    } else {
        params.extend([("sync", 1.0), ("time", d.time as f32)]);
    }
    Settings::new(&params, Fidelity::Partial)
}

/// Attack and release as the shaper's attack and sustain, and the shapes
/// (sharp is quick, soft slow) as its speed. Its split, gain and drive
/// aren't carried over.
fn transient_processor(t: native::TransientProcessor) -> Settings {
    let speed = |shape: i32| match shape {
        0 => 2.0f32,
        2 => 0.5,
        _ => 1.0,
    };
    let params = [
        ("attack", 100.0 * t.attack),
        ("sustain", 100.0 * t.release),
        (
            "speed",
            (speed(t.attack_shape) * speed(t.release_shape)).sqrt(),
        ),
    ];
    Settings::new(&params, Fidelity::Partial)
}

fn compressor(c: Compressor) -> Settings {
    let params = [
        ("threshold", c.threshold_db),
        ("ratio", c.ratio),
        ("makeup", c.gain_db),
        ("attack", c.attack_ms),
        ("release", c.release_ms),
        ("knee", c.knee_db),
        // Its vintage types color the sound like an opto.
        ("model", if c.vintage { 2.0 } else { 0.0 }),
    ];
    Settings {
        params: params
            .iter()
            .map(|(id, value)| ((*id).to_owned(), *value))
            .collect(),
        fidelity: Fidelity::Full,
    }
}

fn reeverb2(r: Reeverb2) -> Settings {
    // One dry/wet for FL's separate levels: the wet's share.
    let mix = wet_share(r.dry, r.wet);
    let params = [
        ("predelay", r.predelay_ms),
        ("size", r.size),
        ("decay", r.decay_s),
        ("mix", mix),
    ];
    Settings {
        params: params
            .iter()
            .map(|(id, value)| ((*id).to_owned(), *value))
            .collect(),
        fidelity: Fidelity::Partial,
    }
}

/// How an automated FL value (`0..=1`) becomes a canopy parameter's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Conversion {
    Eq2Gain,
    Eq2Freq,
    Eq2Width,
    /// A slot's dry/wet, scaling the effect's own `mix` (`0..=100`).
    SlotMix(f32),
    /// A wet level against a dry one (as a gain), the wet knob reaching
    /// `top`.
    WetAgainstDry {
        dry: f32,
        top: f32,
    },
    /// A dry level, the knob reaching `top`, against a wet one.
    DryAgainstWet {
        wet: f32,
        top: f32,
    },
}

impl Conversion {
    pub fn apply(self, x: f64) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Conversion::Eq2Gain => native::eq2_gain_db(x),
            Conversion::Eq2Freq => native::eq2_hz(x),
            Conversion::Eq2Width => native::octaves_to_q(native::eq2_octaves(x)),
            Conversion::SlotMix(own) => own * x as f32,
            Conversion::WetAgainstDry { dry, top } => wet_share(dry, top * x as f32),
            Conversion::DryAgainstWet { wet, top } => wet_share(top * x as f32, wet),
        }
    }
}

/// What automation of FL plugin `plugin`'s parameter `param` drives on
/// canopy effect `effect`: the parameter's id and how to convert values.
pub fn plugin_automation(
    plugin: &fl::Plugin,
    effect: &str,
    param: u16,
) -> Option<(String, Conversion)> {
    let name = plugins::normalize(&plugin.display_name());
    match (name.as_str(), effect) {
        ("fruity parametric eq 2", plugins::EQ) => {
            let (which, base, conversion) = match param {
                0..7 => ("gain", EQ2_LEVEL, Conversion::Eq2Gain),
                7..14 => ("freq", EQ2_FREQ, Conversion::Eq2Freq),
                14..21 => ("q", EQ2_WIDTH, Conversion::Eq2Width),
                _ => return None,
            };
            Some((format!("b{}_{which}", param - base + 1), conversion))
        }
        ("fruity reeverb 2", plugins::REVERB) if param == REEVERB2_WET => {
            let dry = Reeverb2::parse(&plugin.state).map_or(1.0, |r| r.dry);
            // Its wet knob reaches 200%.
            Some((
                "mix".to_owned(),
                Conversion::WetAgainstDry { dry, top: 2.0 },
            ))
        }
        ("fruity reeverb 2", plugins::REVERB) if param == REEVERB2_DRY => {
            let wet = Reeverb2::parse(&plugin.state).map_or(0.25, |r| r.wet);
            Some((
                "mix".to_owned(),
                Conversion::DryAgainstWet { wet, top: 1.0 },
            ))
        }
        ("fruity delay 3", plugins::DELAY) if param == DELAY3_WET => {
            let dry = Delay3::parse(&plugin.state).map_or(1.0, |d| d.dry);
            Some((
                "mix".to_owned(),
                Conversion::WetAgainstDry { dry, top: 1.0 },
            ))
        }
        _ => None,
    }
}

/// Whether a plugin parameter's name says it's the dry/wet.
pub fn is_mix_name(name: &str) -> bool {
    let name = name.trim().to_lowercase().replace(' ', "");
    matches!(
        name.as_str(),
        "mix" | "dry/wet" | "wet/dry" | "drywet" | "drywetmix" | "wet" | "amount" | "blend"
    )
}

/// What automation of a slot's dry/wet drives on canopy effect `effect`,
/// given the mix its settings came over with.
pub fn slot_mix_automation(effect: &str, own: Option<f32>) -> Option<(String, Conversion)> {
    let param = mix_param(effect)?;
    let own = own.unwrap_or(param.default_value());
    Some(("mix".to_owned(), Conversion::SlotMix(own)))
}
