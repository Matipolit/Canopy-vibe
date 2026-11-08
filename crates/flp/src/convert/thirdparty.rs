//! Third-party plugins' settings, where their states can be read (see
//! [`crate::vst`]).
//!
//! - **Xfer OTT**: its parameters as a host sees them, `0..=1`. Their laws
//!   are those Faust's model of OTT measured (`co.xfer_ott`, faustlibraries
//!   pull request 257): Upwd%, Dnwd%, the thresholds and the band input
//!   gains read 0 to 200% (0.5 is 100%); a threshold moves its band's
//!   curve about 24 dB per 100%; time scales the bands' attacks and
//!   releases up to ten times; the in and out gains reach ±19.1 dB.
//! - **TDR Kotelnikov** and **TDR Nova**: XML with every parameter in its
//!   own units.
//! - **Valhalla Supermassive**: XML, `0..=1` for the host, onto Expanse,
//!   which is built after it: its modes in its order, mix, warp,
//!   feedback, density and mod depth as shares. The laws of the rest are
//!   guesses until FL renders measure them: width `-100..=100%`, the cuts
//!   on Expanse's own log scales (10 Hz to 2 kHz, 200 Hz to 20 kHz), the
//!   mod rate on 0.01 to 10 Hz, Mode in twelfths, the time in ms up to
//!   2 s, and a synced time as `1/64` to 4 bars in nine steps of
//!   `DelayNote`, straight, dotted or triplet as `DelaySync` says (a
//!   quarter, half, three quarters).
//! - **Kilohearts** snapins: JSON in natural units (seconds, Hz, `0..=1`),
//!   numbers kept as strings.
//! - **MeldaProduction**: zlib-compressed XML, only what's away from the
//!   defaults, in the plugin's own units (dB, shares, log10 of Hz). The
//!   parameters' meanings, ranges and defaults are from Melda's manuals;
//!   how their curves compare with canopy's hasn't been measured.
//! - **Cymatics Diablo Lite**: JUCE's `<PARAM id value>` XML, in its units.

use effects::chorus::{FLANGER, PHASER};
use effects::eq::BandType;
use effects::multiband::{self, BAND_PARAMS, BANDS};
use serde_json::{Map, Value};

use super::{Companion, Fidelity, Settings, drawn_points};
use crate::model as fl;
use crate::plugins;
use crate::vst::{self, OTT_PARAMS};

/// The settings of third-party `plugin` (its name `name`, normalized) for
/// canopy effect `effect`, if its state can be read.
pub fn settings(plugin: &fl::Plugin, name: &str, effect: &str) -> Option<Settings> {
    let state = &plugin.state;
    let kilohearts = || vst::kilohearts(state);
    match (name, effect) {
        ("ott" | "xfer ott", plugins::MULTIBAND) => vst::ott(state).map(|p| ott(&p)),
        ("tdr kotelnikov" | "tdr kotelnikov ge", plugins::COMPRESSOR) => {
            vst::xml_attributes(state, "logical").map(|a| kotelnikov(&a))
        }
        ("tdr nova" | "tdr nova ge", plugins::EQ) => {
            vst::xml_attributes(state, "TDRNova").map(|a| nova(&a))
        }
        ("valhallasupermassive", plugins::REVERB) => {
            vst::xml_attributes(state, "ValhallaSupermassive").map(|a| supermassive_reverb(&a))
        }
        ("valhallasupermassive", plugins::EXPANSE) => {
            vst::xml_attributes(state, "ValhallaSupermassive").map(|a| supermassive(&a))
        }
        ("khs compactor", plugins::COMPRESSOR) => kilohearts().map(|m| compactor(&m)),
        ("msaturator", plugins::WAVESHAPER) => vst::melda(state, "MSaturator").map(|a| msaturator(&a)),
        ("mwaveshaper", plugins::WAVESHAPER) => {
            vst::melda_elements(state, "MWaveShaper").map(|e| mwaveshaper(&e))
        }
        ("mwavefolder", plugins::WAVESHAPER) => {
            vst::melda(state, "MWaveFolder").map(|a| mwavefolder(&a))
        }
        ("mstereoexpander", plugins::UTILITY) => {
            vst::melda(state, "MStereoExpander").map(|a| mstereoexpander(&a))
        }
        ("mautopan", plugins::SHAPER) => vst::melda(state, "MAutopan").map(|a| mautopan(&a)),
        ("cymatics diablo lite" | "diablo lite", plugins::CLIPPER) => {
            let params = vst::juce_params(state);
            (!params.is_empty()).then(|| diablo_lite(&params))
        }
        ("khs flanger", plugins::CHORUS) => kilohearts().map(|m| khs_flanger(&m)),
        ("khs phaser", plugins::CHORUS) => kilohearts().map(|m| khs_phaser(&m)),
        ("khs transient shaper", plugins::TRANSIENT) => {
            kilohearts().map(|m| khs_transient_shaper(&m))
        }
        _ => None,
    }
}

fn db(gain: f32) -> f32 {
    20.0 * gain.max(1e-6).log10()
}

const OTT_DEPTH: usize = 0;
const OTT_TIME: usize = 1;
const OTT_IN: usize = 2;
const OTT_OUT: usize = 3;
const OTT_THRESHOLD: usize = 5;
const OTT_BAND_GAIN: usize = 8;
const OTT_BYPASS_UP: usize = 11;
const OTT_BYPASS_DOWN: usize = 14;
const OTT_UPWARD: usize = 17;
const OTT_DOWNWARD: usize = 18;

/// OTT onto canopy's Multiband, whose defaults are OTT's at 100%.
fn ott(p: &[f32; OTT_PARAMS]) -> Settings {
    let mut params: Vec<(String, f32)> = Vec::new();
    let mut set = |id: String, value: f32| params.push((id, value));
    // 0.5 is 100%, 1 is 200%.
    let percent = |x: f32| 2.0 * x;
    // 0.5 is 1, 1 is 10.
    let time = 10.0 * p[OTT_TIME].powf(std::f32::consts::LOG2_10);
    let gain_db = |x: f32| 38.2 * (x - 0.5);
    let up = percent(p[OTT_UPWARD]);
    let down = percent(p[OTT_DOWNWARD]);
    for (band, prefix) in ["low", "mid", "high"].into_iter().enumerate() {
        let default = |which: usize| {
            multiband::DESCRIPTOR.params[multiband::param(band, which)].default_value()
        };
        let id = |which: &str| format!("{prefix}_{which}");
        // A band's input gain is the same as its thresholds lowered and its
        // output raised by as much; at 0% it's silent.
        let input = percent(p[OTT_BAND_GAIN + band]);
        let input_db = db(input).max(-40.0);
        let shift = 24.0 * (percent(p[OTT_THRESHOLD + band]) - 1.0) - input_db;
        set(id("threshold"), default(multiband::THRESHOLD) + shift);
        set(id("up_threshold"), default(multiband::UP_THRESHOLD) + shift);
        set(id("gain"), default(multiband::GAIN) + input_db);
        // Upwd% and Dnwd% scale how hard each side squeezes.
        let ratio = |ratio: f32, amount: f32, bypassed: f32| {
            let slope = (1.0 - 1.0 / ratio) * amount;
            if bypassed >= 0.5 || slope <= 0.0 {
                1.0
            } else if (amount - 1.0).abs() < 1e-4 {
                ratio
            } else if slope >= 0.99 {
                100.0
            } else {
                1.0 / (1.0 - slope)
            }
        };
        let up_amount = if input == 0.0 { 0.0 } else { up };
        set(
            id("up_ratio"),
            ratio(
                default(multiband::UP_RATIO),
                up_amount,
                p[OTT_BYPASS_UP + band],
            ),
        );
        set(
            id("ratio"),
            ratio(default(multiband::RATIO), down, p[OTT_BYPASS_DOWN + band]),
        );
        set(id("attack"), default(multiband::ATTACK) * time);
        set(id("release"), default(multiband::RELEASE) * time);
    }
    set("depth".to_owned(), 100.0 * p[OTT_DEPTH]);
    set("input".to_owned(), gain_db(p[OTT_IN]));
    set("output".to_owned(), gain_db(p[OTT_OUT]));
    debug_assert_eq!(BANDS * BAND_PARAMS + 6, multiband::DESCRIPTOR.params.len());
    Settings {
        params,
        fidelity: Fidelity::Partial,
        companions: Vec::new(),
    }
}

/// Its own units throughout. It releases a peak and an RMS detector
/// separately; canopy's one release takes the RMS one's, the slower.
fn kotelnikov(a: &[(String, String)]) -> Settings {
    let get = |name: &str| vst::attribute(a, name);
    let mut params = Vec::new();
    for (id, name) in [
        ("threshold", "thresholdParam"),
        ("ratio", "ratioParam"),
        ("attack", "attackParam"),
        ("release", "releaseRMSParam"),
        ("knee", "softKneeParam"),
        ("sc_hpf", "keyHPFrequencyParam"),
    ] {
        if let Some(value) = get(name) {
            params.push((id, value));
        }
    }
    let makeup = get("makeUpParam").unwrap_or(0.0) + get("outGainParam").unwrap_or(0.0);
    params.push(("makeup", makeup));
    // Its dry level (in dB, -60 for off) on top of the compressed sound.
    let dry = get("dryMixParam").unwrap_or(-60.0);
    if dry > -60.0 {
        params.push(("mix", 100.0 / (1.0 + 10f32.powf(dry / 20.0))));
    }
    Settings::new(&params, Fidelity::Partial)
}

/// Nova's four bands and its high and low passes onto EQ bands 1 to 6.
/// Bands that are off, or flat with no dynamics, stay off; what the
/// dynamics do isn't carried over.
fn nova(a: &[(String, String)]) -> Settings {
    let get = |name: String| vst::attribute(a, &name);
    let text = |name: String| vst::text(a, &name).unwrap_or_default().to_owned();
    let on = |name: String| text(name) == "On";
    let type_index = |kind: BandType| BandType::ALL.iter().position(|k| *k == kind).unwrap_or(2);
    let mut params: Vec<(String, f32)> = Vec::new();
    let mut fidelity = Fidelity::Full;
    let mut band = 0;
    let mut add = |params: &mut Vec<(String, f32)>, values: &[(&str, f32)]| {
        band += 1;
        for (which, value) in values {
            params.push((format!("b{band}_{which}"), *value));
        }
    };
    // FL's EQ order: low cut first.
    for (which, kind) in [("hp", BandType::LowCut), ("lp", BandType::HighCut)] {
        let active = on(format!("{which}Active_master"));
        let freq = get(format!("{which}Freq_master")).unwrap_or(1000.0);
        let slope_db: f32 = text(format!("{which}Type_master"))
            .trim_end_matches("dB/oct")
            .parse()
            .unwrap_or(12.0);
        let slope = (slope_db / 12.0 - 1.0).round().clamp(0.0, 3.0);
        if active {
            add(
                &mut params,
                &[
                    ("on", 1.0),
                    ("type", type_index(kind) as f32),
                    ("freq", freq),
                    ("q", std::f32::consts::FRAC_1_SQRT_2),
                    ("slope", slope),
                ],
            );
        }
    }
    for n in 1..=4 {
        let gain = get(format!("bandGain_{n}")).unwrap_or(0.0);
        let dynamic = on(format!("bandDynActive_{n}"));
        if !on(format!("bandActive_{n}")) || (gain.abs() < 0.05 && !dynamic) {
            continue;
        }
        if dynamic {
            fidelity = Fidelity::Partial;
        }
        let kind = match text(format!("bandType_{n}")).as_str() {
            "Low S" | "Low Shelf" => BandType::LowShelf,
            "High S" | "High Shelf" => BandType::HighShelf,
            _ => BandType::Bell,
        };
        add(
            &mut params,
            &[
                ("on", 1.0),
                ("type", type_index(kind) as f32),
                ("freq", get(format!("bandFreq_{n}")).unwrap_or(1000.0)),
                ("gain", gain),
                ("q", get(format!("bandQ_{n}")).unwrap_or(0.7)),
            ],
        );
    }
    for n in band + 1..=effects::eq::BANDS {
        params.push((format!("b{n}_on"), 0.0));
    }
    if let Some(gain) = get("gain_master".to_owned()) {
        params.push(("output".to_owned(), gain));
    }
    if on("bandDynActive_wide".to_owned()) {
        fidelity = Fidelity::Partial;
    }
    Settings {
        params,
        fidelity,
        companions: Vec::new(),
    }
}

/// A Melda knob's value, or its default when the file leaves it out.
fn melda_value(a: &[(String, String)], name: &str, default: f32) -> f32 {
    vst::attribute(a, name).unwrap_or(default)
}

/// MSaturator on the Waveshaper: input gain and threshold as the drive
/// (saturation starts at the threshold; at its default, silence, the drive
/// is taken as 6 dB), the threshold taken back off the output, the Clip modes as hard clipping and the Soft ones as
/// soft, Analog (its even harmonics, 0–500%, 50% by default) as a little
/// bias, its dry/wet and output gain. Its harmonics panel and compander
/// aren't carried over.
fn msaturator(a: &[(String, String)]) -> Settings {
    let gain = melda_value(a, "#Gain", 0.0);
    let threshold = vst::attribute(a, "#threshold")
        .filter(|t| *t > 0.0)
        .map_or(6.0, |t| -20.0 * t.log10());
    let mode = vst::text(a, "#mode").unwrap_or("Soft 1");
    let shape = if mode.starts_with("Clip") { 1.0 } else { 0.0 };
    let analog = melda_value(a, "#evenharmonics", 0.5);
    // The threshold lowers where the curve bends, not the level of what
    // stays below it: driven by it and brought back down by as much, quiet
    // signals pass at the input gain, as MSaturator's do.
    Settings::new(
        &[
            ("drive", gain + threshold),
            ("shape", shape),
            ("bias", 12.0 * analog),
            ("mix", 100.0 * melda_value(a, "#depth", 1.0)),
            ("output", melda_value(a, "#OutputGain", 0.0) - threshold),
        ],
        Fidelity::Partial,
    )
}

/// MWaveShaper on the Waveshaper's drawn curve: its graph's points (`x`
/// in, `y` out, `0..=1`, 0 where left out; `c` bends the segment after a
/// point, taken as a tension), mirrored below zero unless Asymmetric, with
/// its input gain worked in; its dry/wet (50% by default) and output gain.
/// Its modes besides Squared read the graph differently; not carried over.
fn mwaveshaper(elements: &[(String, Vec<(String, String)>)]) -> Settings {
    let root = &elements[0].1;
    let graph = elements
        .iter()
        .position(|(name, _)| name == "graph")
        .unwrap_or(elements.len());
    let mut points: Vec<(f64, f64, f32)> = elements[graph..]
        .iter()
        .filter(|(name, _)| name == "Point")
        .map(|(_, a)| {
            let get = |k: &str| vst::attribute(a, k).unwrap_or(0.0);
            (f64::from(get("x")).clamp(0.0, 1.0), f64::from(get("y")).clamp(0.0, 1.0), get("c").clamp(-1.0, 1.0))
        })
        .collect();
    if points.len() < 2 {
        points = vec![(0.0, 0.0, 0.0), (1.0, 1.0, 0.0)];
    }
    let curve = |x: f64| {
        for pair in points.windows(2) {
            let ((x0, y0, c), (x1, y1, _)) = (pair[0], pair[1]);
            if x <= x1 {
                if x1 <= x0 {
                    return y1;
                }
                let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) as f32;
                return f64::from(project::segment_value(y0 as f32, y1 as f32, c, t));
            }
        }
        points.last().map_or(0.0, |p| p.1)
    };
    let gain = 10f64.powf(f64::from(melda_value(root, "#gain", 0.0)) / 20.0);
    let bipolar = melda_value(root, "#Asymmetric", 0.0) >= 0.5;
    let mut params = vec![
        ("shape".to_owned(), effects::waveshaper::DRAWN as f32),
        ("drive".to_owned(), 0.0),
        ("mix".to_owned(), 100.0 * melda_value(root, "#Depth", 0.5)),
        ("output".to_owned(), melda_value(root, "#outputgain", 0.0)),
    ];
    params.extend(drawn_points(&curve, gain, bipolar));
    Settings {
        params,
        fidelity: Fidelity::Partial,
        companions: Vec::new(),
    }
}

/// MWaveFolder on the Waveshaper's folds: drive, character (digital is a
/// straight fold, analog a sine's), symmetry as bias, the post low-pass
/// as the tone, dry/wet and output. Its non-linearity and pre filters
/// aren't carried over.
fn mwavefolder(a: &[(String, String)]) -> Settings {
    let character = melda_value(a, "#Character", 1.0);
    let mut params = vec![
        ("drive", melda_value(a, "#GainIn", 0.0)),
        ("shape", if character >= 0.5 { 4.0 } else { 3.0 }),
        ("bias", 100.0 * melda_value(a, "#Symmetry", 0.0)),
        ("mix", 100.0 * melda_value(a, "#DryWet", 1.0)),
        ("output", melda_value(a, "#GainOut", 0.0)),
    ];
    if let Some(log_hz) = vst::attribute(a, "#PostLP") {
        params.push(("tone", 10f32.powf(log_hz)));
    }
    Settings::new(&params, Fidelity::Partial)
}

/// MStereoExpander's depth (`-1..=1`, 0 by default) as the Utility's width:
/// the whole band, where it widens only between its filters.
fn mstereoexpander(a: &[(String, String)]) -> Settings {
    let depth = melda_value(a, "#depth", 0.0);
    Settings::new(&[("width", 100.0 + 100.0 * depth)], Fidelity::Partial)
}

/// MAutopan as the Shaper's pan module alone (the Shaper's volume pump off):
/// its depth (a dry/wet, 50% by default), its synced length (in whole
/// notes, triplets or dotted) as the nearest rate, its phase (a quarter
/// turn by default). A free rate (Hz) can't be synced, so stays at
/// a quarter note; its pan law isn't carried over.
fn mautopan(a: &[(String, String)]) -> Settings {
    use effects::shaper::RATES;
    let mut params = vec![
        ("vol_on", 0.0),
        ("pan_on", 1.0),
        ("pan_depth", 100.0 * melda_value(a, "#depth", 0.5)),
        ("pan_phase", 360.0 * melda_value(a, "#syncphase", 0.25)),
    ];
    if melda_value(a, "#sync", 0.0) >= 0.5 {
        let length = vst::text(a, "#synclength__").map_or(0.25, whole_notes);
        let kind = match vst::text(a, "#synctype") {
            Some("Triplets") => 2.0 / 3.0,
            Some("Dotted") => 1.5,
            _ => 1.0,
        };
        let quarters = 4.0 * length * kind;
        let distance = |q: f64| (q / quarters.max(1e-6)).ln().abs();
        let rate = (0..RATES.len())
            .min_by(|&x, &y| distance(RATES[x].1).total_cmp(&distance(RATES[y].1)))
            .unwrap_or(8);
        params.push(("pan_rate", rate as f32));
    }
    Settings::new(&params, Fidelity::Partial)
}

/// A Melda sync length ("1 / 8", "2") in whole notes.
fn whole_notes(text: &str) -> f64 {
    match text.split_once('/') {
        Some((n, d)) => {
            let (n, d): (f64, f64) = (n.trim().parse().unwrap_or(1.0), d.trim().parse().unwrap_or(4.0));
            n / d.max(1.0)
        }
        None => text.trim().parse().unwrap_or(0.25),
    }
}

/// Diablo Lite: its clipper as the Clipper (Clip, 0–100%, drives into a
/// 0 dB ceiling, taken as up to 24 dB; hard or soft), and its Punch, a fast
/// transient shaper ahead of it, as a Transient Shaper's attack. Its mix
/// (of the whole) goes on the transient shaper, as the Clipper has none.
fn diablo_lite(p: &[(String, f32)]) -> Settings {
    let get = |id: &str, default: f32| p.iter().find(|(i, _)| i == id).map_or(default, |(_, v)| *v);
    let clip_on = get("paramClipBypass", 1.0) >= 0.5;
    let punch_on = get("paramPunchBypass", 1.0) >= 0.5;
    let input = get("paramIn", 0.0) + if clip_on { 0.24 * get("paramClip", 0.0) } else { 0.0 };
    let mut settings = Settings::new(
        &[
            ("input", input),
            ("ceiling", 0.0),
            // Hard, or Smooth.
            ("shape", if get("paramClipMode", 0.0) >= 0.5 { 1.0 } else { 0.0 }),
            ("output", get("paramOut", 0.0)),
        ],
        Fidelity::Partial,
    );
    if punch_on && get("paramPunch", 0.0) > 0.0 {
        settings = settings.with(Companion::new(
            plugins::TRANSIENT,
            true,
            &[
                ("attack", get("paramPunch", 0.0)),
                ("mix", get("paramMix", 100.0)),
            ],
        ));
    }
    settings
}

/// Supermassive on Expanse (see the module's notes for which laws are
/// known and which guessed).
fn supermassive(a: &[(String, String)]) -> Settings {
    let get = |name: &str| vst::attribute(a, name).map(|v| v.clamp(0.0, 1.0));
    let mut params = Vec::new();
    for (name, id) in [
        ("Mix", "mix"),
        ("DelayWarp", "warp"),
        ("Feedback", "feedback"),
        ("Density", "density"),
        ("ModDepth", "mod_depth"),
    ] {
        if let Some(v) = get(name) {
            params.push((id, 100.0 * v));
        }
    }
    if let Some(mode) = get("Mode") {
        let last = effects::expanse::MODES.len() - 1;
        params.push(("mode", (mode * 12.0).round().min(last as f32)));
    }
    if let Some(width) = get("Width") {
        params.push(("width", 200.0 * width - 100.0));
    }
    let log = |x: f32, low: f32, high: f32| low * (high / low).powf(x);
    if let Some(cut) = get("LowCut") {
        params.push(("low_cut", log(cut, 10.0, 2000.0)));
    }
    if let Some(cut) = get("HighCut") {
        params.push(("high_cut", log(cut, 200.0, 20_000.0)));
    }
    if let Some(rate) = get("ModRate") {
        params.push(("mod_rate", log(rate, 0.01, 10.0)));
    }
    match get("DelaySync") {
        Some(sync) if sync >= 0.125 => {
            // 1/64 of a bar to 4 bars, in quarter notes, then the kind.
            let step = (get("DelayNote").unwrap_or(0.5) * 8.0).round();
            let kind = match (sync * 4.0).round() as i32 {
                2 => 1.5,
                3.. => 2.0 / 3.0,
                _ => 1.0,
            };
            let quarters = 4.0 * 2f64.powf(f64::from(step) - 6.0) * kind;
            params.push(("sync", 0.0));
            params.push(("note", nearest_note(quarters) as f32));
        }
        _ => {
            params.push(("sync", 1.0));
            if let Some(ms) = get("Delay_Ms") {
                params.push(("time", (2000.0 * ms).max(1.0)));
            }
        }
    }
    Settings::new(&params, Fidelity::Partial)
}

/// The delay's note value nearest `quarters` long, on a log scale.
fn nearest_note(quarters: f64) -> usize {
    let distance = |q: f64| (q / quarters).ln().abs();
    (0..effects::delay::NOTES.len())
        .min_by(|&a, &b| {
            distance(effects::delay::NOTES[a].1).total_cmp(&distance(effects::delay::NOTES[b].1))
        })
        .unwrap_or(0)
}

/// Its mix and width, for whoever picks the Reverb for it instead; its
/// modes, delays and feedback are a different kind of reverb from the
/// Reverb's, so the rest starts at its defaults. Its mix crossfades dry to
/// wet, as the Reverb did before its levels were split, so it becomes the
/// two levels the same way.
fn supermassive_reverb(a: &[(String, String)]) -> Settings {
    let mut params = Vec::new();
    if let Some(mix) = vst::attribute(a, "Mix") {
        params.push(("dry", 100.0 * (1.0 - mix)));
        params.push(("wet", 100.0 * mix));
    }
    if let Some(width) = vst::attribute(a, "Width") {
        params.push(("width", 100.0 * width));
    }
    Settings::new(&params, Fidelity::Partial)
}

/// Compactor squeezes everything above its threshold (a level, `0..=1`)
/// down to it, its times in seconds: canopy's compressor at its highest
/// ratio, hard-kneed. Its range and hold aren't carried over.
fn compactor(m: &Map<String, Value>) -> Settings {
    let get = |key: &str| vst::number(m, key);
    let mut params = vec![
        ("ratio", 20.0),
        ("knee", 0.0),
        ("threshold", get("threshold").map_or(-18.0, db)),
    ];
    if let Some(attack) = get("env_attack") {
        params.push(("attack", attack * 1000.0));
    }
    if let Some(release) = get("env_release") {
        params.push(("release", release * 1000.0));
    }
    if let Some(mix) = get("mix") {
        params.push(("mix", mix * 100.0));
    }
    Settings::new(&params, Fidelity::Partial)
}

/// Kilohearts' stereo offsets are a share of a cycle; canopy's spread is
/// how far apart, 0 to 180°.
fn fold_degrees(cycles: f32) -> f32 {
    let degrees = (cycles * 360.0).rem_euclid(360.0);
    degrees.min(360.0 - degrees)
}

/// Its delay and sweep in seconds. Barber-pole sweeping isn't carried
/// over.
fn khs_flanger(m: &Map<String, Value>) -> Settings {
    let get = |key: &str| vst::number(m, key);
    let delay_ms = get("delay").map_or(2.5, |s| s * 1000.0);
    let mut params = vec![("mode", FLANGER as f32), ("delay", delay_ms)];
    // Canopy's sweep reaches the delay either way at full depth.
    if let Some(depth) = get("mod_depth") {
        params.push(("depth", 100.0 * depth * 1000.0 / delay_ms.max(0.1)));
    }
    for (id, key, scale) in [
        ("rate", "mod_rate", 1.0),
        ("feedback", "feedback", 100.0),
        ("mix", "mix", 100.0),
    ] {
        if let Some(value) = get(key) {
            params.push((id, value * scale));
        }
    }
    if let Some(spread) = get("spread") {
        params.push(("spread", fold_degrees(spread)));
    }
    params.push(("sync", 1.0));
    Settings::new(&params, Fidelity::Partial)
}

/// Its rate in Hz, its centre ("cutoff") in Hz, and two all-passes per
/// order.
fn khs_phaser(m: &Map<String, Value>) -> Settings {
    let get = |key: &str| vst::number(m, key);
    let mut params = vec![("mode", PHASER as f32), ("sync", 1.0)];
    if let Some(order) = get("order") {
        // Stage counts 2, 4, ... 12 are choices 0 to 5.
        params.push(("stages", (order.round() - 1.0).clamp(0.0, 5.0)));
    }
    for (id, key, scale) in [
        ("rate", "rate", 1.0),
        ("center", "cutoff", 1.0),
        ("depth", "depth", 100.0),
        ("mix", "mix", 100.0),
    ] {
        if let Some(value) = get(key) {
            params.push((id, value * scale));
        }
    }
    if let Some(spread) = get("spread") {
        params.push(("spread", fold_degrees(spread)));
    }
    Settings::new(&params, Fidelity::Partial)
}

/// Attack, sustain and pump (`-1..=1`, pump `0..=1`), its speed a
/// multiplier, and its clipper: the shaper was made with these in mind.
/// How many dB kHs's full attack gives isn't known; canopy's is at most
/// 24.
fn khs_transient_shaper(m: &Map<String, Value>) -> Settings {
    let get = |key: &str| vst::number(m, key);
    let mut params = Vec::new();
    for (id, key, scale) in [
        ("attack", "attack", 100.0),
        ("sustain", "sustain", 100.0),
        ("pump", "pump", 100.0),
        ("speed", "speed", 1.0),
    ] {
        if let Some(value) = get(key) {
            params.push((id, value * scale));
        }
    }
    if let Some(Value::String(clip)) = m.get("clip") {
        params.push(("clip", if clip == "true" { 1.0 } else { 0.0 }));
    }
    Settings::new(&params, Fidelity::Partial)
}

#[cfg(test)]
mod tests {
    use effects::EffectRef;

    use super::*;

    fn default_of(effect: &str, id: &str) -> f32 {
        EffectRef::builtin(effect)
            .descriptor()
            .and_then(|d| d.params.iter().find(|p| p.id == id))
            .map_or(0.0, |p| p.default_value())
    }

    fn value(settings: &Settings, id: &str) -> Option<f32> {
        settings
            .params
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, v)| *v)
    }

    #[test]
    fn ott_at_its_defaults_is_the_multiband_at_its() {
        let mut p = [0.5; OTT_PARAMS];
        p[OTT_DEPTH] = 1.0;
        p[4] = 0.0;
        p[OTT_BYPASS_UP..OTT_UPWARD].fill(0.0);
        let s = ott(&p);
        for (id, v) in &s.params {
            let default = default_of(plugins::MULTIBAND, id);
            assert!(
                (v - default).abs() < 0.01 * default.abs().max(1.0),
                "{id}: {v} vs {default}"
            );
        }
    }

    #[test]
    fn ott_upward_off_and_a_killed_band() {
        let mut p = [0.5; OTT_PARAMS];
        p[4] = 0.0;
        p[OTT_BYPASS_UP..OTT_UPWARD].fill(0.0);
        p[OTT_DEPTH] = 0.3;
        p[OTT_UPWARD] = 0.0;
        p[OTT_BAND_GAIN] = 0.0;
        let s = ott(&p);
        assert!((value(&s, "depth").unwrap() - 30.0).abs() < 1e-4);
        assert_eq!(value(&s, "mid_up_ratio"), Some(1.0));
        // The low band, fed nothing, comes out far down.
        assert!(value(&s, "low_gain").unwrap() < -25.0);
    }

    #[test]
    fn kotelnikov_reads_its_logical_values() {
        let xml = br#"<TDRKotelnikov thresholdParam="-30.8"><logical thresholdParam="-30.8" ratioParam="2.2" attackParam="7.0" releasePeakParam="70.0" releaseRMSParam="165.0" softKneeParam="1.5" makeUpParam="1.5" outGainParam="1.9" dryMixParam="-60.0" keyHPFrequencyParam="80.0"/>"#;
        let s = kotelnikov(&vst::xml_attributes(xml, "logical").unwrap());
        assert_eq!(value(&s, "threshold"), Some(-30.8));
        assert_eq!(value(&s, "release"), Some(165.0));
        assert!((value(&s, "makeup").unwrap() - 3.4).abs() < 1e-5);
        assert_eq!(value(&s, "mix"), None);
    }

    #[test]
    fn supermassive_mix_becomes_the_reverbs_dry_and_wet() {
        let xml = br#"<ValhallaSupermassive Mix="0.3" Width="0.8"/>"#;
        let s = supermassive_reverb(&vst::xml_attributes(xml, "ValhallaSupermassive").unwrap());
        let reverb = EffectRef::builtin(plugins::REVERB).descriptor().unwrap();
        for (id, _) in &s.params {
            assert!(reverb.params.iter().any(|p| p.id == id), "no {id}");
        }
        assert!((value(&s, "dry").unwrap() - 70.0).abs() < 1e-4);
        assert!((value(&s, "wet").unwrap() - 30.0).abs() < 1e-4);
        assert!((value(&s, "width").unwrap() - 80.0).abs() < 1e-4);
    }

    #[test]
    fn supermassive_comes_in_on_expanse() {
        // As saved in a real project ("ItsFullOfStars").
        let xml = br#"<ValhallaSupermassive pluginVersion="5.0.0" Mix="0.256" DelaySync="0.75" DelayNote="0.38" Delay_Ms="1.0" DelayWarp="0.0" Feedback="0.608" Density="0.0" Width="1.0" LowCut="0.156" HighCut="1.0" ModRate="0.2738" ModDepth="0.5" Mode="0.1666666716337204"/>"#;
        let s = supermassive(&vst::xml_attributes(xml, "ValhallaSupermassive").unwrap());
        let expanse = EffectRef::builtin(plugins::EXPANSE).descriptor().unwrap();
        for (id, v) in &s.params {
            let param = expanse.params.iter().find(|p| p.id == id).expect(id);
            assert_eq!(param.clamp(*v), *v, "{id} in range");
        }
        assert!((value(&s, "mix").unwrap() - 25.6).abs() < 1e-3);
        assert!((value(&s, "feedback").unwrap() - 60.8).abs() < 1e-3);
        assert_eq!(value(&s, "mode"), Some(2.0));
        assert_eq!(value(&s, "width"), Some(100.0));
        assert_eq!(value(&s, "high_cut"), Some(20_000.0));
        // An eighth-note triplet.
        assert_eq!(value(&s, "sync"), Some(0.0));
        let note = value(&s, "note").unwrap() as usize;
        assert_eq!(effects::delay::NOTES[note].0, "1/8 T");
        // Free time.
        let xml = br#"<ValhallaSupermassive DelaySync="0.0" Delay_Ms="0.25"/>"#;
        let s = supermassive(&vst::xml_attributes(xml, "ValhallaSupermassive").unwrap());
        assert_eq!(value(&s, "sync"), Some(1.0));
        assert_eq!(value(&s, "time"), Some(500.0));
    }

    /// A Melda state: some binary, then the settings zlib-compressed twice.
    fn melda_state(xml: &[u8]) -> Vec<u8> {
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(xml, 9);
        let mut state = vec![1, 0, 0, 0, 1, 0, 0, 0, 64, 0, 0, 0];
        for _ in 0..2 {
            state.extend((packed.len() as u64).to_le_bytes());
            state.extend(&packed);
        }
        state
    }

    #[test]
    fn melda_settings_come_out_of_their_zlib() {
        let state = melda_state(
            br#"<?xml version="1.0" encoding="utf-8"?><MSaturatorsettings #="1" #depth="0.28" #evenharmonics="1.38" #mode="Soft 3" #OutputGain="-1.536" Version="918784"></MSaturatorsettings>"#,
        );
        let a = vst::melda(&state, "MSaturator").unwrap();
        assert_eq!(vst::text(&a, "#mode"), Some("Soft 3"));
        assert_eq!(vst::melda(&state, "MAutopan"), None);
        // As saved in a real project: soft, threshold at silence.
        let s = msaturator(&a);
        assert_eq!(value(&s, "shape"), Some(0.0));
        assert_eq!(value(&s, "drive"), Some(6.0));
        assert!((value(&s, "mix").unwrap() - 28.0).abs() < 1e-4);
        // Quiet signals stay as loud: the threshold's 6 dB come back off.
        assert!((value(&s, "output").unwrap() + 7.536).abs() < 1e-5);
        // A clip mode at a threshold of -5 dB, with 4.56 dB in.
        let a = vec![
            ("#mode".to_owned(), "Clip 1".to_owned()),
            ("#threshold".to_owned(), "0.5615".to_owned()),
            ("#Gain".to_owned(), "4.56".to_owned()),
        ];
        let s = msaturator(&a);
        assert_eq!(value(&s, "shape"), Some(1.0));
        assert!((value(&s, "drive").unwrap() - 9.57).abs() < 0.01);
        assert!((value(&s, "output").unwrap() + 5.01).abs() < 0.01);
        assert_eq!(value(&s, "mix"), Some(100.0));
    }

    #[test]
    fn newer_melda_versions_write_binary_xml() {
        // As MSaturator 17 saves it: a double, a string, small integers.
        let mut xml = b"\0MBXXMSaturatorsettings\0A#\0\x31\x01A#evenharmonics\0d".to_vec();
        xml.extend(2.46f64.to_le_bytes());
        xml.extend(b"A#mode\0sSoft 4\0A#Clip\0\x31\0AVersion\0\x34");
        xml.extend(1_115_904i32.to_le_bytes());
        xml.extend(b"APluginToolbarCollapsed\0\x31\xffXMeters2\0///~");
        let a = vst::melda(&melda_state(&xml), "MSaturator").unwrap();
        assert_eq!(vst::attribute(&a, "#evenharmonics"), Some(2.46));
        assert_eq!(vst::text(&a, "#mode"), Some("Soft 4"));
        assert_eq!(vst::text(&a, "#Clip"), Some("0"));
        assert_eq!(vst::text(&a, "Version"), Some("1115904"));
        assert_eq!(vst::text(&a, "PluginToolbarCollapsed"), Some("-1"));
        assert_eq!(a.len(), 6, "the root's attributes only");
    }

    #[test]
    fn mwaveshaper_brings_its_graph() {
        // As a corpus one saves it ("Overdrive"): a rise to full output at
        // 0.4076, flat after; dry/wet a quarter.
        let state = melda_state(
            br#"<?xml version="1.0" encoding="utf-8"?>
<MWaveShapersettings #="1" #DCBlocker="1" #Depth="0.255" Version="918784"><Meters2><WidthM><Source Prefiltering="0"/></WidthM></Meters2>
<graph><Point FlagsB="141"/><Point FlagsB="140" x="0.4076" y="1"/><Point FlagsB="141" x="1" y="1"/></graph>
</MWaveShapersettings>"#,
        );
        let elements = vst::melda_elements(&state, "MWaveShaper").unwrap();
        let s = mwaveshaper(&elements);
        assert_eq!(value(&s, "shape"), Some(effects::waveshaper::DRAWN as f32));
        assert!((value(&s, "mix").unwrap() - 25.5).abs() < 1e-4);
        let point = |k: usize| value(&s, &format!("curve_p{k}")).unwrap();
        // Input 0.25 (point 40) is 0.25 / 0.4076 of the way up; 0.5 is flat
        // at the top; -0.5 mirrored.
        assert!((point(40) - 0.5 * (1.0 + 0.25 / 0.4076)).abs() < 1e-3);
        assert!((point(48) - 1.0).abs() < 1e-6);
        assert!(point(16).abs() < 1e-6);
        assert!((point(32) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mautopan_is_the_shapers_pan_alone() {
        let a = vec![
            ("#sync".to_owned(), "1".to_owned()),
            ("#synclength__".to_owned(), "1 / 8".to_owned()),
        ];
        let s = mautopan(&a);
        assert_eq!(value(&s, "vol_on"), Some(0.0));
        assert_eq!(value(&s, "pan_on"), Some(1.0));
        assert_eq!(value(&s, "pan_depth"), Some(50.0));
        assert_eq!(value(&s, "pan_phase"), Some(90.0));
        let rate = value(&s, "pan_rate").unwrap() as usize;
        assert_eq!(effects::shaper::RATES[rate].0, "1/8");
        // Free-running: the rate stays as it is.
        assert_eq!(value(&mautopan(&[]), "pan_rate"), None);
        assert_eq!(whole_notes("2"), 2.0);
        assert_eq!(whole_notes("1 / 128"), 1.0 / 128.0);
    }

    #[test]
    fn diablo_lite_is_a_transient_shaper_into_the_clipper() {
        let state = br#"<?xml version="1.0" encoding="UTF-8"?> <cymatics_diablo_lite><PARAM id="paramClip" value="21.0"/><PARAM id="paramClipBypass" value="1.0"/><PARAM id="paramClipMode" value="1.0"/><PARAM id="paramIn" value="0.0"/><PARAM id="paramMix" value="52.0"/><PARAM id="paramOut" value="-4.0"/><PARAM id="paramPunch" value="9.0"/><PARAM id="paramPunchBypass" value="1.0"/></cymatics_diablo_lite>"#;
        let s = diablo_lite(&vst::juce_params(state));
        assert!((value(&s, "input").unwrap() - 5.04).abs() < 1e-4);
        assert_eq!(value(&s, "shape"), Some(1.0));
        assert_eq!(value(&s, "output"), Some(-4.0));
        let [punch] = s.companions.as_slice() else {
            panic!("one companion");
        };
        assert_eq!(punch.effect, plugins::TRANSIENT);
        assert!(punch.before);
        assert_eq!(punch.params, [("attack".to_owned(), 9.0), ("mix".to_owned(), 52.0)]);
    }

    #[test]
    fn nova_keeps_bands_that_do_something() {
        let xml = br#"<TDRNova bandActive_1="On" bandGain_1="0.0" bandQ_1="0.40" bandFreq_1="80" bandType_1="Bell" bandDynActive_1="Off" bandActive_2="On" bandGain_2="-0.9" bandQ_2="0.60" bandFreq_2="330" bandType_2="Bell" bandDynActive_2="On" bandActive_3="On" bandGain_3="2.0" bandQ_3="0.6" bandFreq_3="6000" bandType_3="High S" bandDynActive_3="Off" bandActive_4="Off" hpActive_master="On" hpFreq_master="40" hpType_master="24dB/oct" lpActive_master="Off" gain_master="-1.0"/>"#;
        let s = nova(&vst::xml_attributes(xml, "TDRNova").unwrap());
        assert_eq!(value(&s, "b1_freq"), Some(40.0));
        assert_eq!(value(&s, "b1_slope"), Some(1.0));
        assert_eq!(value(&s, "b2_freq"), Some(330.0));
        assert_eq!(value(&s, "b3_type"), Some(4.0));
        assert_eq!(value(&s, "b4_on"), Some(0.0));
        assert_eq!(value(&s, "output"), Some(-1.0));
        assert_eq!(s.fidelity, Fidelity::Partial);
    }
}
