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
    self, Balance, Compressor, DELAY3_INPUT, DELAY3_WET, Delay3, EQ2_FREQ, EQ2_LEVEL, EQ2_WIDTH, Eq2, Eq2Type,
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
    /// Other canopy effects that together with this one do what the FL
    /// plugin does (a transient shaper ahead of a clipper, say).
    pub companions: Vec<Companion>,
}

/// A canopy effect next to the main one, with its settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Companion {
    pub effect: &'static str,
    /// Ahead of the main effect in the chain, or after it.
    pub before: bool,
    pub params: Vec<(String, f32)>,
}

impl Companion {
    pub fn new(effect: &'static str, before: bool, params: &[(&str, f32)]) -> Companion {
        Companion {
            effect,
            before,
            params: params
                .iter()
                .map(|(id, value)| ((*id).to_owned(), *value))
                .collect(),
        }
    }
}

/// `params` kept to the parameters `effect` has, each inside its range
/// (FL's can go beyond canopy's).
fn in_range(effect: &str, params: &mut Vec<(String, f32)>) {
    if let Some(descriptor) = EffectRef::builtin(effect).descriptor() {
        params.retain_mut(|(id, value)| {
            let Some(param) = descriptor.params.iter().find(|p| p.id == id) else {
                debug_assert!(false, "{effect} has no parameter {id}");
                return false;
            };
            *value = param.clamp(*value);
            value.is_finite()
        });
    }
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
        ("fruity limiter", plugins::LIMITER) => {
            native::Limiter::parse(&plugin.state).map_or_else(Settings::default, limiter)
        }
        ("fruity limiter", plugins::COMPRESSOR) => {
            native::Limiter::parse(&plugin.state).map_or_else(Settings::default, limiter_compressor)
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
        ("fruity phaser", plugins::CHORUS) => {
            native::Phaser::parse(&plugin.state).map_or_else(Settings::default, fruity_phaser)
        }
        ("fruity flangus", plugins::CHORUS) => {
            Flangus::parse(&plugin.state).map_or_else(Settings::default, flangus)
        }
        ("gross beat", plugins::SHAPER) => {
            native::GrossBeat::parse(&plugin.state).map_or_else(Settings::default, gross_beat)
        }
        ("maximus", plugins::MULTIBAND) => {
            native::Maximus::parse(&plugin.state).map_or_else(Settings::default, maximus)
        }
        ("fruity waveshaper", plugins::WAVESHAPER) => native::FruityWaveShaper::parse(&plugin.state)
            .map_or_else(Settings::default, |w| fruity_waveshaper(&w)),
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
    } else if let Some(mix) = mix.filter(|m| *m < FULL_MIX)
        && effect == plugins::REVERB
    {
        // The reverb's levels instead: the slot's dry adds to its own.
        let share = mix.max(0) as f32 / FULL_MIX as f32;
        let level = |id: &str, default: f32| {
            settings
                .params
                .iter()
                .find(|(p, _)| p == id)
                .map_or(default, |(_, v)| *v)
        };
        let dry = 100.0 * (1.0 - share) + share * level("dry", 100.0);
        let wet = share * level("wet", 25.0);
        settings.params.retain(|(id, _)| id != "dry" && id != "wet");
        settings.params.extend([("dry".to_owned(), dry), ("wet".to_owned(), wet)]);
    }
    // Inside the canopy parameters' ranges, which FL's can go beyond.
    in_range(effect, &mut settings.params);
    for companion in &mut settings.companions {
        in_range(companion.effect, &mut companion.params);
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
            companions: Vec::new(),
        }
    }

    fn with(mut self, companion: Companion) -> Settings {
        self.companions.push(companion);
        self
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
        companions: Vec::new(),
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
        set(band, "q", eq2_q(fl_band.kind, fl_band.width, fl_band.hz, fl_band.order));
        if kind.has_slope() {
            set(band, "slope", fl_band.order.unsigned_abs().min(3) as f32);
        }
    }
    params.push(("output".to_owned(), eq.main_db));
    Settings {
        params,
        fidelity: Fidelity::Full,
        companions: Vec::new(),
    }
}

/// The canopy effect an FL effect becomes when the user hasn't chosen one,
/// where its settings decide: a Fruity Limiter used only as a compressor
/// becomes the Compressor.
pub fn instance_effect(plugin: &fl::Plugin, suggested: &'static str) -> &'static str {
    let name = plugins::normalize(&plugin.display_name());
    match (name.as_str(), suggested) {
        ("fruity limiter", plugins::LIMITER)
            if native::Limiter::parse(&plugin.state).is_some_and(|l| l.compresses()) =>
        {
            plugins::COMPRESSOR
        }
        _ => suggested,
    }
}

/// Fruity Limiter's limiter: its gain (canopy's only lifts), ceiling (a
/// sample peak's, not a true peak's) and release. The compressor before it
/// isn't carried over here, nor the gate or the saturation.
fn limiter(l: native::Limiter) -> Settings {
    let mut params = vec![
        ("input", l.gain_db.clamp(0.0, 24.0)),
        ("release", l.limiter_release_ms.clamp(1.0, 1000.0)),
        ("auto_release", 0.0),
        ("true_peak", 0.0),
    ];
    // Above 0 dB it doesn't limit; canopy's tops out there.
    params.push(("ceiling", l.ceiling_db.unwrap_or(0.0).clamp(-12.0, 0.0)));
    let full = l.threshold_db.is_none() && l.gain_db >= 0.0;
    Settings::new(&params, if full { Fidelity::Full } else { Fidelity::Partial })
}

/// Fruity Limiter as a compressor: hard-kneed, its gain the make-up.
fn limiter_compressor(l: native::Limiter) -> Settings {
    let params = [
        ("threshold", l.threshold_db.unwrap_or(0.0).clamp(-60.0, 0.0)),
        ("ratio", l.ratio.clamp(1.0, 20.0)),
        ("knee", 0.0),
        ("attack", l.attack_ms.clamp(0.1, 100.0)),
        ("release", l.release_ms.clamp(10.0, 1000.0)),
        ("makeup", l.gain_db.clamp(-12.0, 24.0)),
    ];
    Settings::new(&params, Fidelity::Partial)
}

/// The rate canopy usually runs at, for [`eq2_q`]'s bells.
const EQ2_CANOPY_RATE: f32 = 48_000.0;

/// The Q canopy's EQ needs to draw what an EQ 2 band of type `kind`,
/// bandwidth knob `width` (`0..=1`), at `hz`, with pass order `order`
/// draws: each fitted to FL's renders (4–5 Nov 2026), within 0.1 dB but
/// where said.
///
/// - Bells are the cookbook's (RBJ) peaking filters, their bandwidth `0.1 +
///   4.9 · width` octaves, kept as wide near the Nyquist frequency as at
///   the bottom: canopy's bilinear bells need it widened by `w0 / sin w0`.
/// - Shelves: Q falls straight from 1.18 to 0.38 across the knob, whatever
///   the gain or frequency.
/// - Passes: one resonant section, its Q from the knob's own curve (FL's
///   default, 0.388, peaks 4.5 dB at the corner); a positive order adds
///   Butterworth sections, `1/√2` each at the corner, and a negative one
///   steeper sections that canopy's draw within 0.3 to 1 dB at these Qs.
///   Neither depends on the frequency. The knob's curve levels off at
///   0.071 from about 0.95 (fitted against EQ 2 renders at 0.9, 0.95 and
///   1.0, within 0.05 dB once canopy's Q reached below 0.1).
/// - Band passes and notches have curves of their own, on the bell's
///   widened bandwidth.
pub fn eq2_q(kind: Eq2Type, width: f32, hz: f32, order: i32) -> f32 {
    let width = width.clamp(0.0, 1.0);
    // Log-linear between points, and beyond the last two.
    let curve = |points: &[(f32, f32)], x: f32| -> f32 {
        let i = points
            .iter()
            .position(|(at, _)| *at > x)
            .unwrap_or(points.len() - 1)
            .clamp(1, points.len() - 1);
        let ((a, qa), (b, qb)) = (points[i - 1], points[i]);
        (qa.ln() + (x - a) / (b - a) * (qb.ln() - qa.ln())).exp()
    };
    let widened = || {
        let w0 = std::f32::consts::TAU * hz.min(0.45 * EQ2_CANOPY_RATE) / EQ2_CANOPY_RATE;
        let octaves = native::eq2_octaves(f64::from(width)) * w0 / w0.sin();
        (octaves, (octaves - 0.1) / 4.9)
    };
    match kind {
        Eq2Type::Peaking => native::octaves_to_q(widened().0),
        Eq2Type::LowShelf | Eq2Type::HighShelf => 1.18 - 0.8 * width,
        Eq2Type::LowPass | Eq2Type::HighPass => {
            const PASS: [(f32, f32); 12] = [
                (0.0, 7.07),
                (0.1, 5.18),
                (0.2, 3.646),
                (0.3, 2.466),
                (0.388, 1.672),
                (0.5, 0.95),
                (0.6, 0.519),
                (0.7, 0.262),
                (0.8, 0.127),
                (0.9, 0.078),
                (0.95, 0.071),
                (1.0, 0.071),
            ];
            const STEEPER: [f32; 3] = [0.942, 0.81, 0.704];
            let stages = order.unsigned_abs().min(3) as usize;
            let by_order = match order {
                0 => 1.0,
                1.. => std::f32::consts::FRAC_1_SQRT_2.powi(stages as i32),
                _ => STEEPER[stages - 1],
            };
            curve(&PASS, width) * by_order
        }
        Eq2Type::BandPass => curve(&[(0.1, 3.444), (0.388, 0.967), (0.8, 0.434)], widened().1),
        Eq2Type::Notch => curve(&[(0.1, 1.706), (0.388, 0.434), (0.8, 0.14)], widened().1),
        Eq2Type::Off => std::f32::consts::FRAC_1_SQRT_2,
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

/// The phaser, its wet share as the mix (both blend linearly, an even
/// blend making the deepest notches) and its stages: FL's 7 and 11 (the
/// only ones saved here) come in as 8 and 12, whether FL's knob counts
/// from 0 or the count rounds up to canopy's even ones.
fn fruity_phaser(p: native::Phaser) -> Settings {
    let stages = (p.stages + 1).clamp(2, 12) / 2;
    let params = [
        ("mode", PHASER as f32),
        ("stages", (stages - 1) as f32),
        ("mix", 100.0 * p.wet),
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

/// Gross Beat's chosen envelopes as the Shaper's time and volume shapes,
/// a bar long; its filter and pan stay off. Each of the 32 steps takes the
/// envelope's average over it, so gates keep their weight.
fn gross_beat(g: native::GrossBeat) -> Settings {
    use effects::shaper::{POINTS, RATES};
    let bar = RATES
        .iter()
        .position(|(_, quarters)| *quarters == native::GrossBeat::LENGTH)
        .unwrap_or(13) as f32;
    let steps = |points: &[native::EnvelopePoint]| -> Vec<f32> {
        const SUB: usize = 16;
        let step = native::GrossBeat::LENGTH / POINTS as f64;
        (0..POINTS)
            .map(|i| {
                let sum: f64 = (0..SUB)
                    .map(|k| {
                        let beat = (i as f64 + (k as f64 + 0.5) / SUB as f64) * step;
                        native::envelope_at(points, beat)
                    })
                    .sum();
                (sum / SUB as f64) as f32
            })
            .collect()
    };
    let flat = |shape: &[f32]| shape.iter().all(|v| (v - 1.0).abs() < 1e-4);
    let mut params = vec![
        ("flt_on".to_owned(), 0.0),
        ("pan_on".to_owned(), 0.0),
    ];
    for (prefix, points) in [("vol", &g.volume), ("tim", &g.time)] {
        let shape = steps(points);
        let on = !flat(&shape);
        params.push((format!("{prefix}_on"), on_off(on)));
        if !on {
            continue;
        }
        params.extend([
            (format!("{prefix}_rate"), bar),
            (format!("{prefix}_depth"), 100.0),
            (format!("{prefix}_phase"), 0.0),
            (format!("{prefix}_smooth"), 0.0),
        ]);
        for (i, value) in shape.into_iter().enumerate() {
            params.push((format!("{prefix}_p{i}"), value));
        }
    }
    params.push(("tim_range".to_owned(), native::GrossBeat::TIME_RANGE as f32));
    Settings {
        params,
        fidelity: Fidelity::Partial,
        companions: Vec::new(),
    }
}

/// The share of a dry and a wet level that's wet, as a mix.
fn wet_share(dry: f32, wet: f32) -> f32 {
    if dry + wet > 0.0 {
        100.0 * wet / (dry + wet)
    } else {
        0.0
    }
}

/// Separate dry and wet levels (gains) as canopy's mix and output: the
/// mix crossfades, so the output brings back what the two add up to.
fn levels(dry: f32, wet: f32) -> [(&'static str, f32); 2] {
    let total = dry.max(0.0) + wet.max(0.0);
    let output = if total > 0.0 { 20.0 * total.log10() } else { -24.0 };
    [("mix", wet_share(dry, wet)), ("output", output.clamp(-24.0, 12.0))]
}

/// Its model, time, feedback and levels. Its filter, modulation,
/// diffusion and distortion aren't carried over.
fn delay3(d: Delay3) -> Settings {
    let mut params = vec![
        ("mode", if d.model == 2 { 1.0 } else { 0.0 }),
        ("feedback", 100.0 * d.feedback),
        ("input", 100.0 * d.input),
    ];
    // The model "Off" makes no echoes.
    let wet = if d.model == 3 { 0.0 } else { d.wet };
    params.extend(levels(d.dry, wet));
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
        companions: Vec::new(),
    }
}

/// Reeverb 2 as canopy's hall with the Gentle damping curve, each knob
/// turned into what it measurably does (see [`Reeverb2`]): the decay as
/// the tail's real RT60, the bass knob as the bass's real multiple of it,
/// and the wet level made up for how much louder FL's tail is in small
/// rooms and short decays. Its early reflections (15 dB under the tail at
/// most), diffusion and modulation aren't carried over, nor separation
/// wider than canopy's widest.
fn reeverb2(r: Reeverb2) -> Settings {
    let decay = native::reeverb2_rt60(r.decay_s, r.size).clamp(0.1, 20.0);
    let (curve, damping) = match r.damping_hz {
        Some(hz) => (1.0, hz.clamp(500.0, 20_000.0)),
        None => (0.0, 20_000.0),
    };
    // FL's crossover is where the bass's own decay starts to show; the
    // halfway point canopy's knob sets lies above it, further for a
    // shorter bass than a longer one.
    let reach = if r.bass >= 100.0 {
        1.2
    } else {
        1.2 * (r.bass.max(1.0) / 100.0).powf(-0.43)
    };
    let params = [
        ("type", effects::reverb::HALL as f32),
        ("size", r.size),
        ("decay", decay),
        ("predelay", r.predelay_ms),
        ("damping_curve", curve),
        ("damping", damping),
        ("bass", native::reeverb2_bass(r.bass)),
        ("crossover", (r.crossover_hz * reach).clamp(50.0, 5000.0)),
        ("dry", 100.0 * r.dry),
        ("wet", (100.0 * r.wet * reeverb2_tail(&r)).min(200.0)),
        // FL's cuts are gentle, 6 dB/octave: fitted against FL's renders
        // of right-here_2's reverbs.
        ("low_cut", r.low_cut_hz),
        ("high_cut", r.high_cut_hz),
        ("cut_slope", 1.0),
        ("width", (100.0 - 100.0 * r.separation / 64.0).clamp(0.0, 200.0)),
        ("mod_depth", 0.0),
    ];
    Settings {
        params: params
            .iter()
            .map(|(id, value)| ((*id).to_owned(), *value))
            .collect(),
        fidelity: Fidelity::Partial,
        companions: Vec::new(),
    }
}

/// How much louder Reeverb 2's tail is than canopy's hall at the same
/// wet level and RT60, as a gain: from about 6 dB at size 1 to nothing at
/// 100, and more for tails under a second, whose diffusion FL hears more.
fn reeverb2_tail(r: &Reeverb2) -> f32 {
    const BY_SIZE_DB: [(f32, f32); 8] = [
        (1.0, 6.0),
        (10.0, 4.5),
        (25.0, 3.3),
        (35.0, 2.5),
        (44.0, 2.2),
        (50.0, 1.65),
        (75.0, 1.2),
        (100.0, 0.2),
    ];
    let size = r.size.clamp(1.0, 100.0);
    let i = BY_SIZE_DB.iter().position(|(at, _)| *at >= size).unwrap_or(0).max(1);
    let ((a, a_db), (b, b_db)) = (BY_SIZE_DB[i - 1], BY_SIZE_DB[i]);
    let by_size = a_db + (size - a) / (b - a) * (b_db - a_db);
    let decay = native::reeverb2_rt60(r.decay_s, r.size);
    let short = 10.0 * (1.0 + (0.6 / decay).powi(2)).log10();
    10f32.powf((by_size + short) / 20.0)
}

/// How an automated FL value (`0..=1`) becomes a canopy parameter's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Conversion {
    Eq2Gain,
    Eq2Freq,
    /// A bandwidth knob, for a band of this type, frequency and order.
    Eq2Width(Eq2Type, f32, i32),
    /// A slot's dry/wet, scaling the effect's own `mix` (`0..=100`).
    SlotMix(f32),
    /// A wet level against a dry one (as a gain), the wet knob reaching
    /// `top`.
    WetAgainstDry {
        dry: f32,
        top: f32,
    },
    /// Reeverb 2's wet knob on its curve, made up by this gain.
    Reeverb2Wet(f32),
    /// Reeverb 2's dry knob on its curve.
    Reeverb2Dry,
    /// Straight from `from` at 0 to `to` at 1.
    Linear { from: f32, to: f32 },
    /// A Fruity Limiter or Maximus gain knob (`0..=2000` for the host's
    /// `0..=1`) in dB, plus `offset` dB.
    LimiterGain { offset: f32 },
}

impl Conversion {
    pub fn apply(self, x: f64) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Conversion::Eq2Gain => native::eq2_gain_db(x),
            Conversion::Eq2Freq => native::eq2_hz(x),
            Conversion::Eq2Width(kind, hz, order) => eq2_q(kind, x as f32, hz, order),
            Conversion::SlotMix(own) => own * x as f32,
            Conversion::WetAgainstDry { dry, top } => wet_share(dry, top * x as f32),
            Conversion::Reeverb2Wet(tail) => {
                let knob = native::REEVERB2_LEVEL_TOP * x as f32;
                (100.0 * tail * native::reeverb2_wet_gain(knob)).min(200.0)
            }
            Conversion::Reeverb2Dry => {
                let knob = native::REEVERB2_LEVEL_TOP * x as f32;
                (100.0 * native::reeverb2_dry_gain(knob)).min(100.0)
            }
            Conversion::Linear { from, to } => from + (to - from) * x as f32,
            Conversion::LimiterGain { offset } => {
                offset + native::limiter_gain_db(2000.0 * x as f32)
            }
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
                14..21 => {
                    let band = Eq2::parse(&plugin.state)
                        .and_then(|eq| eq.bands.get(usize::from(param - EQ2_WIDTH)).copied());
                    let (kind, hz, order) =
                        band.map_or((Eq2Type::Peaking, 1000.0, 0), |b| (b.kind, b.hz, b.order));
                    ("q", EQ2_WIDTH, Conversion::Eq2Width(kind, hz, order))
                }
                _ => return None,
            };
            Some((format!("b{}_{which}", param - base + 1), conversion))
        }
        // Its level knobs run on curves of their own (`native`); the wet
        // is made up as the settings' is.
        ("fruity reeverb 2", plugins::REVERB) if param == REEVERB2_WET => {
            let tail = Reeverb2::parse(&plugin.state).map_or(1.0, |r| reeverb2_tail(&r));
            Some(("wet".to_owned(), Conversion::Reeverb2Wet(tail)))
        }
        ("fruity reeverb 2", plugins::REVERB) if param == REEVERB2_DRY => {
            Some(("dry".to_owned(), Conversion::Reeverb2Dry))
        }
        // A band's gains on the Multiband's band gain (the other of the
        // two staying as it was), the master's on its input and output.
        ("maximus", plugins::MULTIBAND) => {
            let m = native::Maximus::parse(&plugin.state)?;
            let (band, at) = native::MAXIMUS_BAND
                .iter()
                .enumerate()
                .find_map(|(band, &start)| {
                    let at = param.checked_sub(start)?;
                    (at == native::MAXIMUS_PRE || at == native::MAXIMUS_POST).then_some((band, at))
                })?;
            let b = m.bands[band];
            let pre = at == native::MAXIMUS_PRE;
            let (id, offset) = match (band, pre) {
                (3, true) => ("input".to_owned(), 0.0),
                (3, false) => ("output".to_owned(), 0.0),
                (_, true) => (format!("{}_gain", MULTIBAND_BANDS[band]), b.post_db),
                (_, false) => (format!("{}_gain", MULTIBAND_BANDS[band]), b.pre_db),
            };
            Some((id, Conversion::LimiterGain { offset }))
        }
        // Its input wet as the Delay's input, for throws.
        ("fruity delay 3", plugins::DELAY) if param == DELAY3_INPUT => Some((
            "input".to_owned(),
            Conversion::Linear {
                from: 0.0,
                to: 100.0,
            },
        )),
        // The scratching clock: at the top the sound as it comes, lower
        // down from further back.
        ("gross beat", plugins::SHAPER) if param == GROSS_BEAT_SCRATCH => Some((
            "tim_offset".to_owned(),
            Conversion::Linear {
                from: 100.0,
                to: 0.0,
            },
        )),
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

/// Fruity WaveShaper on the Waveshaper's drawn curve: its curve sampled at
/// the drawn curve's points, with the pre gain worked into it (so the
/// Waveshaper's drive stays at 0 dB, which it can't go below), unipolar
/// curves mirrored below zero; its mix, post gain and oversampling. The
/// laws of its three knobs are guesses until measured.
fn fruity_waveshaper(w: &native::FruityWaveShaper) -> Settings {
    use effects::waveshaper::DRAWN;
    let mut params = vec![
        ("shape".to_owned(), DRAWN as f32),
        ("drive".to_owned(), 0.0),
        ("bias".to_owned(), 0.0),
        ("mix".to_owned(), 100.0 * w.mix.min(1.0)),
        ("output".to_owned(), 20.0 * w.post.max(1e-3).log10()),
        ("oversampling".to_owned(), f32::from(w.oversampling.min(3))),
    ];
    params.extend(drawn_points(&|x| w.curve(x), f64::from(w.pre), w.bipolar));
    Settings {
        params,
        fidelity: Fidelity::Partial,
        companions: Vec::new(),
    }
}

/// The Waveshaper's drawn curve for a plugin's curve `curve` (`0..=1` to
/// `0..=1`) after `gain` on the input: over -1 to 1 when `bipolar`, else
/// from silence to full scale and mirrored below zero.
pub(crate) fn drawn_points(curve: &dyn Fn(f64) -> f64, gain: f64, bipolar: bool) -> Vec<(String, f32)> {
    use effects::waveshaper::CURVE_POINTS;
    (0..CURVE_POINTS)
        .map(|k| {
            let input = gain * (-1.0 + 2.0 * k as f64 / (CURVE_POINTS - 1) as f64);
            let output = if bipolar {
                2.0 * curve((input.clamp(-1.0, 1.0) + 1.0) / 2.0) - 1.0
            } else {
                input.signum() * curve(input.abs().min(1.0))
            };
            (format!("curve_p{k}"), (0.5 * (output.clamp(-1.0, 1.0) + 1.0)) as f32)
        })
        .collect()
}

/// The Multiband's bands, in Maximus's order.
const MULTIBAND_BANDS: [&str; 3] = ["low", "mid", "high"];

/// Maximus on the Multiband, compressing nothing (its curves aren't read):
/// each band's pre and post gains as the band's gain (a muted band as low
/// as it goes), the master's as the input and output.
fn maximus(m: native::Maximus) -> Settings {
    // Ratios of 1 both ways compress nothing; Depth stays, as it scales
    // the bands' gains too.
    let mut params = Vec::new();
    for (band, b) in m.bands.iter().take(3).enumerate() {
        let id = MULTIBAND_BANDS[band];
        let gain = if b.state == native::MAXIMUS_MUTED {
            -24.0
        } else {
            b.pre_db + b.post_db
        };
        params.push((format!("{id}_gain"), gain));
        params.push((format!("{id}_ratio"), 1.0));
        params.push((format!("{id}_up_ratio"), 1.0));
    }
    let master = m.bands[3];
    params.push(("input".to_owned(), master.pre_db));
    params.push(("output".to_owned(), master.post_db));
    Settings {
        params,
        fidelity: Fidelity::Partial,
        companions: Vec::new(),
    }
}

/// Gross Beat's "Time offset (scratching)", in automation's numbering.
const GROSS_BEAT_SCRATCH: u16 = 8;

/// What automation of a mixer track's stereo separation drives: the width
/// of a Utility at the end of its chain. FL's knob leaves the sound as it
/// is in the middle, widens it to the left and merges it to mono to the
/// right; how wide its left end is hasn't been measured (taken as 200%).
pub fn separation_automation() -> (&'static str, Conversion) {
    (
        "width",
        Conversion::Linear {
            from: 200.0,
            to: 0.0,
        },
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a / b - 1.0).abs() < 0.01
    }

    /// A Maximus state as FL saves one at its defaults, a band's state and
    /// gains changed as given (band, state, pre, post knobs).
    fn maximus_state(changes: &[(usize, i32, i32, i32)]) -> Vec<u8> {
        let header = [19_800, 1, 48_065, 1, 1115, 1000, 32_768];
        let block = [0, 1000, 1000, 0, 0, 1000, 1115, 0, 2, 5000, 3, 2882, 0];
        let mut fields: Vec<i32> = header.to_vec();
        for _ in 0..4 {
            fields.extend(block);
        }
        for &(band, state, pre, post) in changes {
            let at = usize::from(native::MAXIMUS_BAND[band]);
            fields[at] = state;
            fields[at + 1] = pre;
            fields[at + 2] = post;
        }
        let copy = fields.clone();
        fields.extend(copy);
        let mut out = vec![3, 0, 10, 0];
        for f in fields {
            out.extend(f.to_le_bytes());
        }
        out.extend([0u8; 866]);
        out
    }

    /// A Fruity WaveShaper state: the knobs, bipolar or not, the points
    /// `(x, y, tension)`, and the newer format's longer end.
    fn waveshaper_state(pre: i32, mix: i32, post: i32, bipolar: bool, points: &[(f64, f64, f32)]) -> Vec<u8> {
        let mut out = vec![3, 0, 4, 0];
        for v in [pre, mix, post] {
            out.extend(v.to_le_bytes());
        }
        out.extend([u8::from(bipolar), 0, 0, 0, 0, 0]);
        out.extend(3i32.to_le_bytes());
        out.extend(3i32.to_le_bytes());
        out.extend((points.len() as i32).to_le_bytes());
        let mut last = 0.0;
        for &(x, y, tension) in points {
            out.extend((x - last).to_le_bytes());
            out.extend(y.to_le_bytes());
            out.extend(tension.to_le_bytes());
            let sign = if tension > 0.0 { 1 } else if tension < 0.0 { 255 } else { 2 };
            out.extend([0, 0, 0, sign]);
            last = x;
        }
        out.extend([0u8; 26]);
        out
    }

    fn curve_point(s: &Settings, k: usize) -> f32 {
        let id = format!("curve_p{k}");
        s.params.iter().find(|(p, _)| *p == id).unwrap().1
    }

    #[test]
    fn a_fruity_waveshaper_curve_becomes_the_drawn_curve() {
        // A hard clip drawn by hand (as one in the corpus): full output
        // from 0.268 of full scale up, mirrored below zero.
        let state = waveshaper_state(128, 128, 128, false, &[(0.0, 0.0, 0.0), (0.268, 1.0, 0.0), (1.0, 1.0, 0.0)]);
        let s = fruity_waveshaper(&native::FruityWaveShaper::parse(&state).unwrap());
        let value = |id: &str| s.params.iter().find(|(p, _)| p == id).map(|(_, v)| *v);
        assert_eq!(value("shape"), Some(effects::waveshaper::DRAWN as f32));
        assert_eq!(value("drive"), Some(0.0));
        assert_eq!(value("mix"), Some(100.0));
        assert!(value("output").unwrap().abs() < 1e-5);
        // Points 32 and 64 are inputs 0 and 1; 40 is a quarter of full
        // scale, 0.25 / 0.268 of the way up the rise; 16 is -0.5.
        assert!((curve_point(&s, 32) - 0.5).abs() < 1e-6);
        assert!((curve_point(&s, 40) - 0.5 * (1.0 + 0.25 / 0.268)).abs() < 1e-4);
        assert!((curve_point(&s, 48) - 1.0).abs() < 1e-6);
        assert!(curve_point(&s, 16).abs() < 1e-6);

        // Bipolar and straight, with the pre gain at half: half as loud,
        // the pre gain worked into the curve.
        let state = waveshaper_state(64, 128, 128, true, &[(0.0, 0.0, 0.0), (1.0, 1.0, 0.0)]);
        let s = fruity_waveshaper(&native::FruityWaveShaper::parse(&state).unwrap());
        for k in [0, 16, 32, 50, 64] {
            let input = -1.0 + 2.0 * k as f32 / 64.0;
            assert!((curve_point(&s, k) - 0.5 * (1.0 + 0.5 * input)).abs() < 1e-5, "{k}");
        }
        // A bent segment follows the tension as automation does.
        let state = waveshaper_state(128, 128, 128, false, &[(0.0, 0.0, 0.0), (1.0, 1.0, 0.6)]);
        let w = native::FruityWaveShaper::parse(&state).unwrap();
        assert!((w.curve(0.5) - f64::from(project::segment_value(0.0, 1.0, 0.6, 0.5))).abs() < 1e-6);
    }

    #[test]
    fn maximus_keeps_its_gains_and_compresses_nothing() {
        // The mid band +4.8 dB after compression, the high muted, the
        // master's output down 5.2 dB.
        let state = maximus_state(&[(1, 0, 1000, 1250), (2, 2, 1000, 1000), (3, 0, 1000, 750)]);
        let s = maximus(native::Maximus::parse(&state).unwrap());
        let value = |id: &str| s.params.iter().find(|(p, _)| p == id).map(|(_, v)| *v);
        assert_eq!(value("low_gain"), Some(0.0));
        assert!((value("mid_gain").unwrap() - 4.8).abs() < 1e-4);
        assert_eq!(value("high_gain"), Some(-24.0));
        assert_eq!(value("mid_ratio"), Some(1.0));
        assert_eq!(value("mid_up_ratio"), Some(1.0));
        assert!((value("output").unwrap() + 5.2).abs() < 1e-4);
        assert_eq!(value("depth"), None);

        // The master's post gain, automated (a real song's resting value,
        // 0.5, is 0 dB), and the mid's pre gain on top of its post.
        let plugin = fl::Plugin {
            internal_name: "Maximus".to_owned(),
            name: None,
            vst: None,
            state,
        };
        let (id, conversion) = plugin_automation(&plugin, plugins::MULTIBAND, 48).unwrap();
        assert_eq!(id, "output");
        assert!(conversion.apply(0.5).abs() < 1e-4);
        let (id, conversion) = plugin_automation(&plugin, plugins::MULTIBAND, 21).unwrap();
        assert_eq!(id, "mid_gain");
        assert!((conversion.apply(0.5) - 4.8).abs() < 1e-4);
        assert!(plugin_automation(&plugin, plugins::MULTIBAND, 25).is_none());
    }

    #[test]
    fn eq2_bands_get_the_q_fitted_to_fl() {
        // FL's default bell, two octaves wide, is the cookbook's at 1 kHz
        // and needs widening near the top.
        let bell = eq2_q(Eq2Type::Peaking, 0.388, 1000.0, 0);
        assert!(close(bell, native::octaves_to_q(2.0)), "{bell}");
        assert!(eq2_q(Eq2Type::Peaking, 0.388, 10_000.0, 0) < 0.8 * bell);
        // Shelves straight down the knob.
        assert!(close(eq2_q(Eq2Type::LowShelf, 0.612, 300.0, 0), 0.69));
        assert!(close(eq2_q(Eq2Type::HighShelf, 1.0, 3000.0, 0), 0.38));
        // Passes: 1.672 at the default, then by order.
        let pass = eq2_q(Eq2Type::HighPass, 0.388, 1000.0, 0);
        assert!(close(pass, 1.672), "{pass}");
        assert!(close(eq2_q(Eq2Type::LowPass, 0.388, 5000.0, 3), 1.672 * 0.353_6));
        assert!(close(eq2_q(Eq2Type::HighPass, 0.388, 1000.0, -3), 1.672 * 0.704));
        assert!(close(eq2_q(Eq2Type::HighPass, 0.0, 1000.0, 0), 7.07));
        // Band passes and notches on their own curves.
        assert!(close(eq2_q(Eq2Type::BandPass, 0.388, 1000.0, 0), 0.967));
        assert!(close(eq2_q(Eq2Type::Notch, 0.388, 1000.0, 0), 0.434));
    }
}
