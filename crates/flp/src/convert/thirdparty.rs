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
//! - **Valhalla Supermassive**: XML, `0..=1` for the host.
//! - **Kilohearts** snapins: JSON in natural units (seconds, Hz, `0..=1`),
//!   numbers kept as strings.

use effects::chorus::{FLANGER, PHASER};
use effects::eq::BandType;
use effects::multiband::{self, BAND_PARAMS, BANDS};
use serde_json::{Map, Value};

use super::{Fidelity, Settings};
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
            vst::xml_attributes(state, "ValhallaSupermassive").map(|a| supermassive(&a))
        }
        ("khs compactor", plugins::COMPRESSOR) => kilohearts().map(|m| compactor(&m)),
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
    Settings { params, fidelity }
}

/// Its mix and width; its modes, delays and feedback are a different kind
/// of reverb from canopy's, so the rest starts at canopy's defaults.
fn supermassive(a: &[(String, String)]) -> Settings {
    let mut params = Vec::new();
    if let Some(mix) = vst::attribute(a, "Mix") {
        params.push(("mix", 100.0 * mix));
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
