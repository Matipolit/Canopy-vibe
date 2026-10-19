//! Presets: an effect's or instrument's settings saved under a name, as a
//! small RON file in the app's data folder (`presets/<device id>/<name>.ron`),
//! to load into any copy of that device in any project.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use project::PresetFile;

/// A saved preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub path: PathBuf,
}

/// Where presets live: the app's data folder, or `None` if there's none.
pub fn default_dir() -> Option<PathBuf> {
    eframe::storage_dir("canopy").map(|dir| dir.join("presets"))
}

fn folder(dir: &Path, device: &str) -> PathBuf {
    dir.join(file_safe(device))
}

/// `name` without characters file systems dislike.
fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The presets saved for `device`, by name.
pub fn list(dir: &Path, device: &str) -> Vec<Preset> {
    let Ok(entries) = std::fs::read_dir(folder(dir, device)) else {
        return Vec::new();
    };
    let mut presets: Vec<Preset> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ron"))
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(Preset { name, path })
        })
        .collect();
    presets.sort_by_key(|p| p.name.to_lowercase());
    presets
}

/// Saves `params` for `device` as `name`, replacing a preset of that name;
/// `state` is a plugin's own (empty for built-ins).
pub fn save(
    dir: &Path,
    device: &str,
    name: &str,
    params: &BTreeMap<String, f32>,
    state: &project::Blob,
) -> Result<PathBuf, String> {
    let name = file_safe(name);
    if name.is_empty() {
        return Err("a preset needs a name".into());
    }
    let folder = folder(dir, device);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let file = PresetFile {
        device: device.to_owned(),
        params: params.clone(),
        state: state.clone(),
    };
    let text = file.to_text()?;
    let path = folder.join(format!("{name}.ron"));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// A preset's settings and plugin state, if it's for `device`.
pub fn load(path: &Path, device: &str) -> Result<(BTreeMap<String, f32>, project::Blob), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file = PresetFile::from_text(&text)?;
    if file.device != device {
        return Err(format!("it's a preset for {}", file.device));
    }
    Ok((file.params, file.state))
}

/// The presets that come with canopy for `device`: a name and the settings
/// that differ from the defaults.
pub fn factory(device: &str) -> Vec<(&'static str, BTreeMap<String, f32>)> {
    let set = |pairs: &[(&str, f32)]| -> BTreeMap<String, f32> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    };
    match device {
        "canopy.reverb" => vec![
            (
                "Small room",
                set(&[
                    ("type", 2.0),
                    ("size", 25.0),
                    ("decay", 0.6),
                    ("predelay", 5.0),
                    ("damping", 8000.0),
                    ("low_cut", 150.0),
                    ("width", 80.0),
                    ("mix", 20.0),
                ]),
            ),
            (
                "Plate",
                set(&[
                    ("type", 1.0),
                    ("size", 50.0),
                    ("decay", 1.8),
                    ("predelay", 15.0),
                    ("damping", 12_000.0),
                    ("low_cut", 200.0),
                    ("mix", 25.0),
                ]),
            ),
            (
                "Big hall",
                set(&[
                    ("size", 90.0),
                    ("decay", 4.5),
                    ("predelay", 30.0),
                    ("damping", 5000.0),
                    ("low_cut", 120.0),
                    ("mix", 30.0),
                ]),
            ),
            (
                "Ambient wash",
                set(&[
                    ("size", 100.0),
                    ("decay", 12.0),
                    ("predelay", 60.0),
                    ("damping", 4000.0),
                    ("low_cut", 250.0),
                    ("mix", 45.0),
                ]),
            ),
            (
                "Vocal plate",
                set(&[
                    ("type", 1.0),
                    ("size", 70.0),
                    ("decay", 2.4),
                    ("predelay", 40.0),
                    ("damping", 9000.0),
                    ("low_cut", 250.0),
                    ("mix", 22.0),
                ]),
            ),
            (
                "Drum room",
                set(&[
                    ("type", 2.0),
                    ("size", 55.0),
                    ("decay", 0.9),
                    ("predelay", 0.0),
                    ("damping", 7000.0),
                    ("low_cut", 120.0),
                    ("mix", 25.0),
                ]),
            ),
            (
                "Vocal booth",
                set(&[
                    ("type", 2.0),
                    ("size", 0.0),
                    ("decay", 0.3),
                    ("predelay", 0.0),
                    ("damping", 10_000.0),
                    ("mix", 15.0),
                ]),
            ),
            (
                "Lush chorused hall",
                set(&[
                    ("size", 85.0),
                    ("decay", 5.0),
                    ("predelay", 25.0),
                    ("damping", 6000.0),
                    ("mod_depth", 85.0),
                    ("mod_rate", 0.8),
                    ("mix", 35.0),
                ]),
            ),
        ],
        "canopy.delay" => vec![
            (
                "Slapback",
                set(&[
                    ("sync", 1.0),
                    ("time", 110.0),
                    ("feedback", 10.0),
                    ("mode", 0.0),
                    ("low_cut", 200.0),
                    ("high_cut", 6000.0),
                    ("mix", 25.0),
                ]),
            ),
            (
                "Dotted eighth",
                set(&[("note", 6.0), ("feedback", 35.0), ("mode", 1.0)]),
            ),
            (
                "Quarter ping-pong",
                set(&[
                    ("note", 8.0),
                    ("feedback", 45.0),
                    ("mode", 1.0),
                    ("mix", 30.0),
                ]),
            ),
            (
                "Dub",
                set(&[
                    ("note", 9.0),
                    ("feedback", 70.0),
                    ("low_cut", 300.0),
                    ("high_cut", 3000.0),
                    ("mix", 35.0),
                ]),
            ),
        ],
        "canopy.utility" => vec![
            (
                "Mono bass",
                set(&[("bass_mono", 1.0), ("mono_below", 120.0)]),
            ),
            (
                "Wide",
                set(&[("width", 150.0), ("bass_mono", 1.0), ("mono_below", 150.0)]),
            ),
            ("Mono", set(&[("width", 0.0)])),
            (
                "Flip phase",
                set(&[("invert_left", 1.0), ("invert_right", 1.0)]),
            ),
            ("Side only", set(&[("channels", 4.0)])),
            ("Clean up DC", set(&[("dc", 1.0)])),
            (
                "Haas wide",
                set(&[
                    ("delay", 15.0),
                    ("delay_side", 2.0),
                    ("bass_mono", 1.0),
                    ("mono_below", 150.0),
                ]),
            ),
        ],
        "canopy.compressor" => vec![
            (
                "Vocal",
                set(&[
                    ("model", 2.0),
                    ("threshold", -22.0),
                    ("ratio", 3.0),
                    ("attack", 5.0),
                    ("release", 80.0),
                    ("makeup", 4.0),
                ]),
            ),
            (
                "Drum bus glue",
                set(&[
                    ("threshold", -14.0),
                    ("ratio", 4.0),
                    ("attack", 30.0),
                    ("release", 100.0),
                    ("makeup", 2.0),
                ]),
            ),
            (
                "Parallel smash",
                set(&[
                    ("model", 1.0),
                    ("threshold", -35.0),
                    ("ratio", 10.0),
                    ("attack", 1.0),
                    ("release", 60.0),
                    ("makeup", 8.0),
                    ("mix", 40.0),
                ]),
            ),
            (
                "Bass leveler",
                set(&[
                    ("model", 2.0),
                    ("threshold", -20.0),
                    ("ratio", 4.0),
                    ("attack", 10.0),
                    ("release", 150.0),
                    ("makeup", 3.0),
                ]),
            ),
        ],
        "canopy.clipper" => vec![
            (
                "Transparent -0.3",
                set(&[
                    ("input", 1.5),
                    ("shape", 1.0),
                    ("softness", 10.0),
                    ("oversampling", 3.0),
                ]),
            ),
            (
                "Hot master",
                set(&[
                    ("input", 5.0),
                    ("shape", 1.0),
                    ("softness", 35.0),
                    ("oversampling", 3.0),
                ]),
            ),
            (
                "Drum bus smash",
                set(&[
                    ("input", 9.0),
                    ("ceiling", -3.0),
                    ("shape", 2.0),
                    ("softness", 60.0),
                    ("safety", 1.0),
                ]),
            ),
            (
                "Zero latency",
                set(&[("oversampling", 0.0), ("safety", 1.0)]),
            ),
        ],
        "canopy.limiter" => vec![
            ("Master, -1 dB", set(&[("input", 3.0), ("ceiling", -1.0)])),
            ("Loud", set(&[("input", 8.0), ("ceiling", -0.3)])),
        ],
        "canopy.multiband" => vec![
            ("OTT", set(&[])),
            ("OTT, half", set(&[("depth", 50.0)])),
            ("OTT, a touch", set(&[("depth", 25.0)])),
        ],
        "canopy.ducker" => vec![
            (
                "Kick pump",
                set(&[("depth", 18.0), ("hold", 20.0), ("release", 180.0)]),
            ),
            (
                "Subtle duck",
                set(&[("depth", 6.0), ("release", 120.0), ("curve", 0.0)]),
            ),
            (
                "Quarter pump (no sidechain)",
                set(&[
                    ("mode", 2.0),
                    ("rate", 2.0),
                    ("depth", 24.0),
                    ("release", 250.0),
                ]),
            ),
        ],
        "canopy.transient" => vec![
            (
                "Punchy drums",
                set(&[("attack", 45.0), ("sustain", -15.0), ("clip", 1.0)]),
            ),
            ("Tighten tail", set(&[("attack", 10.0), ("sustain", -60.0)])),
            (
                "Soften attack",
                set(&[("attack", -50.0), ("sustain", 10.0)]),
            ),
            (
                "Room boost",
                set(&[("sustain", 60.0), ("speed", 0.6), ("mix", 80.0)]),
            ),
            (
                "Snap without the boom",
                set(&[
                    ("attack", 70.0),
                    ("split", 1.0),
                    ("split_freq", 1500.0),
                    ("clip", 1.0),
                ]),
            ),
            (
                "Pumping punch",
                set(&[("attack", 25.0), ("pump", 60.0), ("speed", 1.5)]),
            ),
        ],
        "canopy.waveshaper" => vec![
            (
                "Warm saturation",
                set(&[("shape", 2.0), ("drive", 6.0), ("tone", 12_000.0)]),
            ),
            ("Hard clip", set(&[("shape", 1.0), ("drive", 12.0)])),
            (
                "Fold",
                set(&[("shape", 4.0), ("drive", 20.0), ("mix", 60.0)]),
            ),
            (
                "Bitcrush",
                set(&[("shape", 6.0), ("drive", 30.0), ("mix", 70.0)]),
            ),
        ],
        "canopy.eq" => vec![
            (
                "Low cut 80 Hz",
                set(&[("b1_on", 1.0), ("b1_type", 0.0), ("b1_freq", 80.0)]),
            ),
            (
                "Vocal presence",
                set(&[
                    ("b1_on", 1.0),
                    ("b1_type", 0.0),
                    ("b1_freq", 100.0),
                    ("b3_freq", 300.0),
                    ("b3_gain", -2.0),
                    ("b4_freq", 3000.0),
                    ("b4_gain", 3.0),
                    ("b5_freq", 10_000.0),
                    ("b5_gain", 2.0),
                ]),
            ),
            (
                "De-mud",
                set(&[("b3_freq", 350.0), ("b3_gain", -4.0), ("b3_q", 1.4)]),
            ),
        ],
        "canopy.shaper" => {
            // A 1/16 gate: on for the first half of each step.
            let mut gate: Vec<(String, f32)> = (0..32)
                .map(|i| (format!("vol_p{i}"), if i < 16 { 1.0 } else { 0.0 }))
                .collect();
            gate.extend([
                ("vol_rate".to_owned(), 2.0),
                ("vol_smooth".to_owned(), 10.0),
            ]);
            // A slow sine from side to side.
            let mut pan: Vec<(String, f32)> = (0..32)
                .map(|i| {
                    let t = i as f32 / 32.0;
                    (
                        format!("pan_p{i}"),
                        0.5 - 0.5 * (std::f32::consts::TAU * t).cos(),
                    )
                })
                .collect();
            pan.extend([
                ("vol_on".to_owned(), 0.0),
                ("pan_on".to_owned(), 1.0),
                ("pan_rate".to_owned(), 13.0),
                ("pan_smooth".to_owned(), 100.0),
            ]);
            vec![
                ("Quarter pump", set(&[])),
                ("16th gate", gate.into_iter().collect()),
                ("Auto-pan", pan.into_iter().collect()),
            ]
        }
        // Voices and stages are menus, which take their entry's index.
        "canopy.chorus" => vec![
            (
                "Subtle chorus",
                set(&[
                    ("rate", 0.5),
                    ("depth", 30.0),
                    ("delay", 8.0),
                    ("mix", 35.0),
                ]),
            ),
            (
                "Wide chorus",
                set(&[
                    ("rate", 0.7),
                    ("depth", 60.0),
                    ("delay", 15.0),
                    ("voices", 3.0),
                    ("spread", 180.0),
                ]),
            ),
            (
                "Vibrato",
                set(&[
                    ("rate", 5.0),
                    ("depth", 40.0),
                    ("delay", 5.0),
                    ("voices", 0.0),
                    ("spread", 0.0),
                    ("mix", 100.0),
                ]),
            ),
            (
                "Jet flanger",
                set(&[
                    ("mode", 1.0),
                    ("rate", 0.12),
                    ("shape", 1.0),
                    ("depth", 90.0),
                    ("delay", 2.5),
                    ("feedback", 75.0),
                    ("spread", 30.0),
                ]),
            ),
            (
                "Slow phaser",
                set(&[
                    ("mode", 2.0),
                    ("rate", 0.15),
                    ("depth", 70.0),
                    ("center", 700.0),
                    ("feedback", 40.0),
                    ("stages", 2.0),
                ]),
            ),
            (
                "Phaser, a bar a sweep",
                set(&[
                    ("mode", 2.0),
                    ("sync", 0.0),
                    ("note", 13.0),
                    ("depth", 80.0),
                    ("feedback", 55.0),
                    ("stages", 3.0),
                ]),
            ),
        ],
        "canopy.filter" => vec![
            (
                "Low-pass, warm",
                set(&[("type", 1.0), ("cutoff", 900.0), ("resonance", 35.0)]),
            ),
            (
                "Ladder bass",
                set(&[
                    ("type", 11.0),
                    ("cutoff", 380.0),
                    ("resonance", 55.0),
                    ("drive", 6.0),
                    ("var", 60.0),
                ]),
            ),
            (
                "Acid squelch",
                set(&[
                    ("type", 15.0),
                    ("cutoff", 650.0),
                    ("resonance", 85.0),
                    ("drive", 12.0),
                    ("var", 40.0),
                ]),
            ),
            (
                "Screaming band",
                set(&[
                    ("type", 17.0),
                    ("cutoff", 1500.0),
                    ("resonance", 80.0),
                    ("drive", 18.0),
                    ("var", 60.0),
                ]),
            ),
            (
                "Telephone",
                set(&[("type", 5.0), ("cutoff", 1400.0), ("resonance", 30.0)]),
            ),
            (
                "Comb resonator",
                set(&[
                    ("type", 19.0),
                    ("cutoff", 220.0),
                    ("resonance", 85.0),
                    ("var", 30.0),
                ]),
            ),
            (
                "Flanged notches",
                set(&[("type", 22.0), ("cutoff", 1200.0), ("resonance", 70.0)]),
            ),
            (
                "Phaser filter",
                set(&[
                    ("type", 23.0),
                    ("cutoff", 900.0),
                    ("resonance", 60.0),
                    ("var", 50.0),
                ]),
            ),
            (
                "Talking (O)",
                set(&[("type", 25.0), ("var", 75.0), ("resonance", 40.0)]),
            ),
            (
                "Wide stereo notch",
                set(&[("type", 6.0), ("cutoff", 1800.0), ("spread", 12.0)]),
            ),
            (
                "Air shelf",
                set(&[("type", 10.0), ("cutoff", 8000.0), ("gain", 4.5)]),
            ),
        ],
        "canopy.distortion" => vec![
            (
                "Warm tube",
                set(&[("mode", 0.0), ("drive", 30.0), ("mix", 70.0)]),
            ),
            (
                "Diode crunch",
                set(&[("mode", 4.0), ("drive", 55.0), ("output", -4.0)]),
            ),
            (
                "Fuzz pedal",
                set(&[
                    ("mode", 12.0),
                    ("drive", 75.0),
                    ("filter", 2.0),
                    ("filter_freq", 4500.0),
                    ("output", -6.0),
                ]),
            ),
            (
                "Tape warmth",
                set(&[("mode", 13.0), ("drive", 35.0), ("mix", 80.0)]),
            ),
            (
                "Wavefolder",
                set(&[
                    ("mode", 6.0),
                    ("drive", 45.0),
                    ("filter", 1.0),
                    ("filter_freq", 1200.0),
                ]),
            ),
            (
                "Lo-fi sampler",
                set(&[("mode", 8.0), ("drive", 45.0), ("oversampling", 0.0)]),
            ),
            (
                "8-bit",
                set(&[("mode", 16.0), ("drive", 57.0), ("oversampling", 0.0)]),
            ),
            (
                "Bass grit (parallel)",
                set(&[
                    ("mode", 14.0),
                    ("drive", 60.0),
                    ("filter", 1.0),
                    ("filter_type", 100.0),
                    ("filter_freq", 250.0),
                    ("mix", 50.0),
                ]),
            ),
        ],
        "canopy.hyper" => vec![
            (
                "Supersaw spread",
                set(&[
                    ("voices", 6.0),
                    ("detune", 45.0),
                    ("rate", 0.8),
                    ("hyper_mix", 70.0),
                    ("dimension", 40.0),
                ]),
            ),
            (
                "Subtle doubler",
                set(&[
                    ("voices", 1.0),
                    ("detune", 20.0),
                    ("hyper_mix", 40.0),
                    ("dimension", 20.0),
                ]),
            ),
            (
                "Wide pad",
                set(&[
                    ("voices", 4.0),
                    ("detune", 30.0),
                    ("rate", 0.3),
                    ("hyper_mix", 60.0),
                    ("size", 80.0),
                    ("dimension", 70.0),
                ]),
            ),
            (
                "Dimension only",
                set(&[("hyper_mix", 0.0), ("size", 55.0), ("dimension", 80.0)]),
            ),
            (
                "Seasick",
                set(&[
                    ("voices", 2.0),
                    ("detune", 85.0),
                    ("rate", 3.5),
                    ("hyper_mix", 100.0),
                ]),
            ),
        ],
        "canopy.frequency_shifter" => vec![
            (
                "Slow swirl",
                set(&[("shift", 30.0), ("range", 0.0), ("mix", 50.0)]),
            ),
            (
                "Metallic",
                set(&[("shift", 35.0), ("range", 1.0), ("mix", 70.0)]),
            ),
            (
                "Barber-pole up",
                set(&[
                    ("shift", 15.0),
                    ("range", 1.0),
                    ("delay", 120.0),
                    ("feedback", 75.0),
                    ("mix", 60.0),
                ]),
            ),
            (
                "Ring mod bell",
                set(&[
                    ("shift", 32.0),
                    ("range", 2.0),
                    ("ring", 100.0),
                    ("mix", 80.0),
                ]),
            ),
            (
                "Stereo detune",
                set(&[
                    ("shift", 40.0),
                    ("range", 0.0),
                    ("stereo", 100.0),
                    ("mix", 50.0),
                ]),
            ),
            (
                "Spiral echo",
                set(&[
                    ("shift", -8.0),
                    ("range", 2.0),
                    ("delay", 300.0),
                    ("feedback", 65.0),
                    ("blur", 40.0),
                    ("mix", 45.0),
                ]),
            ),
        ],
        "canopy.convolve" => vec![
            ("Room", set(&[("impulse", 0.0), ("mix", 25.0)])),
            (
                "Concert hall",
                set(&[
                    ("impulse", 1.0),
                    ("size", 70.0),
                    ("predelay", 20.0),
                    ("mix", 30.0),
                ]),
            ),
            (
                "Bright plate",
                set(&[("impulse", 2.0), ("tone", 30.0), ("mix", 25.0)]),
            ),
            (
                "Surf spring",
                set(&[("impulse", 3.0), ("size", 40.0), ("mix", 35.0)]),
            ),
            ("Guitar cab 1x12", set(&[("impulse", 4.0), ("mix", 100.0)])),
            (
                "Guitar cab 4x12",
                set(&[("impulse", 5.0), ("size", 60.0), ("mix", 100.0)]),
            ),
            (
                "Old radio",
                set(&[("impulse", 6.0), ("mix", 100.0), ("ir_gain", 3.0)]),
            ),
            (
                "Reverse swell",
                set(&[("impulse", 7.0), ("size", 40.0), ("mix", 40.0)]),
            ),
            (
                "Dark ambience",
                set(&[
                    ("impulse", 1.0),
                    ("size", 100.0),
                    ("attack", 30.0),
                    ("damp", 70.0),
                    ("tone", -40.0),
                    ("mix", 40.0),
                ]),
            ),
        ],
        "canopy.drums" => drums(set),
        "canopy.guitar" => guitar(set),
        "canopy.amp" => amp(set),
        "canopy.wavetable" => wavetable(set),
        _ => Vec::new(),
    }
}

/// The Guitar's presets: models are the `model` parameter's index (steel,
/// nylon, electric), pickups the switch's (neck, neck + middle, middle,
/// middle + bridge, bridge), techniques auto, pick, fingers.
fn guitar(
    set: impl Fn(&[(&str, f32)]) -> BTreeMap<String, f32>,
) -> Vec<(&'static str, BTreeMap<String, f32>)> {
    const STEEL: f32 = 0.0;
    const NYLON: f32 = 1.0;
    const ELECTRIC: f32 = 2.0;
    vec![
        ("Acoustic", set(&[])),
        (
            "Acoustic, fingerpicked",
            set(&[
                ("technique", 2.0),
                ("pick", 0.4),
                ("pluck_position", 0.5),
                ("strum", 45.0),
            ]),
        ),
        (
            "Nylon classical",
            set(&[("model", NYLON), ("pluck_position", 0.45), ("tone", 0.45)]),
        ),
        (
            "Nylon, flamenco",
            set(&[
                ("model", NYLON),
                ("pick", 0.9),
                ("pluck_position", 0.25),
                ("strum", 15.0),
                ("noises", 0.7),
            ]),
        ),
        (
            "Clean electric",
            set(&[
                ("model", ELECTRIC),
                ("pickup", 0.0),
                ("amp", 1.0),
                ("drive", 0.3),
            ]),
        ),
        (
            "Electric, straight in",
            set(&[("model", ELECTRIC), ("pickup", 2.0)]),
        ),
        (
            "Funk",
            set(&[
                ("model", ELECTRIC),
                ("pickup", 3.0),
                ("amp", 1.0),
                ("drive", 0.25),
                ("strum", 15.0),
                ("pick", 0.7),
            ]),
        ),
        (
            "Crunch",
            set(&[
                ("model", ELECTRIC),
                ("pickup", 4.0),
                ("pickup_type", 1.0),
                ("amp", 2.0),
                ("drive", 0.6),
                ("mute", 0.3),
            ]),
        ),
        (
            "Palm-muted rock",
            set(&[
                ("model", ELECTRIC),
                ("pickup", 4.0),
                ("pickup_type", 1.0),
                ("amp", 2.0),
                ("drive", 0.75),
                ("mute", 0.7),
                ("strum", 10.0),
            ]),
        ),
        (
            "Lead",
            set(&[
                ("model", ELECTRIC),
                ("pickup", 4.0),
                ("pickup_type", 1.0),
                ("amp", 3.0),
                ("drive", 0.6),
                ("vibrato", 40.0),
                ("legato", 0.0),
            ]),
        ),
        (
            "Steel, dull old strings",
            set(&[("model", STEEL), ("tone", 0.15), ("sustain", 6.0)]),
        ),
    ]
}

/// The Amp's presets: knobs 0 to 10 as on an amp; voicings clean,
/// crunch, lead; cabinets off, 1x12 open, 2x12 open, 4x12 closed.
fn amp(
    set: impl Fn(&[(&str, f32)]) -> BTreeMap<String, f32>,
) -> Vec<(&'static str, BTreeMap<String, f32>)> {
    vec![
        (
            "Clean, Fender style",
            set(&[
                ("voicing", 0.0),
                ("gain", 3.0),
                ("mid", 4.0),
                ("treble", 6.0),
                ("cabinet", 2.0),
            ]),
        ),
        (
            "Bluesy break-up",
            set(&[
                ("voicing", 0.0),
                ("gain", 8.0),
                ("mid", 6.0),
                ("sag", 6.0),
                ("cabinet", 1.0),
                ("mic", 40.0),
            ]),
        ),
        (
            "Jazz, dark",
            set(&[
                ("voicing", 0.0),
                ("gain", 2.0),
                ("bass", 6.0),
                ("treble", 2.0),
                ("presence", 1.0),
                ("cabinet", 1.0),
                ("mic", 70.0),
            ]),
        ),
        (
            "Crunch, Marshall style",
            set(&[
                ("voicing", 1.0),
                ("gain", 6.0),
                ("mid", 7.0),
                ("treble", 6.0),
                ("presence", 5.0),
                ("cabinet", 3.0),
                ("mic", 25.0),
            ]),
        ),
        (
            "High-gain lead",
            set(&[
                ("voicing", 2.0),
                ("gain", 8.0),
                ("bass", 4.0),
                ("mid", 6.0),
                ("presence", 6.0),
                ("sag", 2.0),
                ("cabinet", 3.0),
                ("mic", 20.0),
            ]),
        ),
    ]
}

/// The Drum Synth's presets. Sources and filters are menus, which take
/// their entry's index.
fn drums(
    set: impl Fn(&[(&str, f32)]) -> BTreeMap<String, f32>,
) -> Vec<(&'static str, BTreeMap<String, f32>)> {
    use instruments::builtin::drums::{FilterKind, Source};
    let source = |s: Source| s as usize as f32;
    let filter = |f: FilterKind| f as usize as f32;
    vec![
        ("Standard", set(&[])),
        (
            "Long 808",
            set(&[
                ("kick_l1_pitch", 45.0),
                ("kick_l2_pitch", 45.0),
                ("kick_l1_decay", 1600.0),
                ("kick_l1_pitch_env", 12.0),
                ("kick_l2_pitch_env", 12.0),
                ("kick_l1_pitch_time", 60.0),
                ("kick_l2_pitch_time", 60.0),
                ("kick_l2_level", -14.0),
                ("kick_l3_level", -22.0),
                ("snare_l4_level", -5.0),
                ("closed_hat_l1_decay", 30.0),
                ("closed_hat_l2_decay", 30.0),
                ("open_hat_l1_decay", 600.0),
                ("open_hat_l2_decay", 600.0),
            ]),
        ),
        (
            "Punchy",
            set(&[
                ("kick_l1_pitch", 55.0),
                ("kick_l2_pitch", 55.0),
                ("kick_l1_decay", 350.0),
                ("kick_l2_level", -5.0),
                ("kick_l3_level", -8.0),
                ("kick_drive", 0.4),
                ("snare_drive", 0.3),
                ("snare_l4_level", -5.0),
                ("kit_drive", 0.15),
            ]),
        ),
        (
            "Lo-fi",
            set(&[
                ("kit_tune", -3.0),
                ("kit_drive", 0.5),
                ("kick_cutoff", 820.0),
                ("closed_hat_l1_cutoff", 4900.0),
                ("closed_hat_l2_cutoff", 4900.0),
                ("open_hat_l1_cutoff", 4900.0),
                ("open_hat_l2_cutoff", 4900.0),
                ("snare_l3_level", -9.0),
            ]),
        ),
        (
            "Electro",
            set(&[
                // A click from an impulse rather than noise, and a body
                // that squares off.
                ("kick_l3_source", source(Source::Impulse)),
                ("kick_l3_pitch", 3000.0),
                ("kick_l3_filter", filter(FilterKind::Off)),
                ("kick_l3_level", -6.0),
                ("kick_l2_drive", 0.5),
                // Crunchy, pitched wires.
                ("snare_l3_source", source(Source::Digital)),
                ("snare_l3_pitch", 9000.0),
                ("snare_l3_shape", 0.5),
                ("snare_l4_source", source(Source::Digital)),
                ("snare_l4_pitch", 12_000.0),
                // Wider metal, and more hands.
                ("closed_hat_l1_shape", 0.8),
                ("open_hat_l1_shape", 0.8),
                ("clap_l1_bursts", 5.0),
                ("clap_l1_gap", 7.0),
                ("clap_l2_delay", 35.0),
            ]),
        ),
        (
            "Chip",
            set(&[
                // Pulse kick, crushed noise for everything else.
                ("kick_l1_source", source(Source::Square)),
                ("kick_l1_shape", 0.4),
                ("kick_l1_pitch", 70.0),
                ("kick_l1_pitch_env", 30.0),
                ("kick_l1_decay", 220.0),
                ("kick_l1_curve", 1.0),
                ("kick_l2_mode", 0.0),
                ("kick_l3_mode", 0.0),
                ("snare_l1_mode", 0.0),
                ("snare_l2_mode", 0.0),
                ("snare_l3_source", source(Source::Digital)),
                ("snare_l3_pitch", 4000.0),
                ("snare_l3_shape", 0.75),
                ("snare_l3_filter", filter(FilterKind::Off)),
                ("snare_l3_curve", 1.0),
                ("snare_l4_mode", 0.0),
                ("closed_hat_l1_mode", 0.0),
                ("closed_hat_l2_source", source(Source::Digital)),
                ("closed_hat_l2_pitch", 16_000.0),
                ("closed_hat_l2_shape", 0.75),
                ("open_hat_l1_mode", 0.0),
                ("open_hat_l2_source", source(Source::Digital)),
                ("open_hat_l2_pitch", 16_000.0),
                ("open_hat_l2_shape", 0.75),
                ("low_tom_l1_source", source(Source::Triangle)),
                ("mid_tom_l1_source", source(Source::Triangle)),
                ("high_tom_l1_source", source(Source::Triangle)),
                ("kit_drive", 0.2),
            ]),
        ),
    ]
}

/// The Wavetable's presets, named "Category: Name": the panel's browser
/// sorts them by the part before the colon. Menus take their entry's index
/// (the label lists in `instruments::builtin::wavetable`): octaves count
/// from -4 (so 4 is the note's own), semitones from -12 (12 is none),
/// unison from 1 voice (so 6 is 7 voices). Basic Shapes runs sine,
/// triangle, saw, square in thirds. The default patch's mod slots 1 to 5
/// stay unless a preset changes them (1 is Env 2 on the cutoff, 2 LFO 1 on
/// A's position, 3 to 5 the mod wheel, velocity and vibrato); a preset
/// that wants its own matrix clears them first. Every preset gives its
/// macros something to do.
fn wavetable(
    set: impl Fn(&[(&str, f32)]) -> BTreeMap<String, f32>,
) -> Vec<(&'static str, BTreeMap<String, f32>)> {
    use instruments::builtin::wavetable as wt;

    const SAW: f32 = 2.0 / 3.0;
    const SQUARE: f32 = 1.0;
    const TRIANGLE: f32 = 1.0 / 3.0;

    /// A preset's settings, with helpers for what takes many parameters.
    struct Patch(BTreeMap<String, f32>);

    impl Patch {
        fn put(mut self, id: String, value: f32) -> Self {
            self.0.insert(id, value);
            self
        }

        /// Empties the default patch's five slots.
        fn clear(mut self) -> Self {
            for n in 1..=5 {
                self = self
                    .put(format!("mod{n}_source"), 0.0)
                    .put(format!("mod{n}_dest"), 0.0);
            }
            self
        }

        /// Slot `n` (1-based) routes `source` to `dest` at `amount`.
        fn route(self, n: usize, source: usize, dest: usize, amount: f32) -> Self {
            self.put(format!("mod{n}_source"), source as f32)
                .put(format!("mod{n}_dest"), dest as f32)
                .put(format!("mod{n}_amount"), amount)
        }

        /// Slot `n` scaled by `aux`.
        fn aux(self, n: usize, aux: usize) -> Self {
            self.put(format!("mod{n}_aux"), aux as f32)
        }

        fn curve(self, n: usize, curve: f32) -> Self {
            self.put(format!("mod{n}_curve"), curve)
        }

        fn polarity(self, n: usize, polarity: usize) -> Self {
            self.put(format!("mod{n}_polarity"), polarity as f32)
        }

        /// LFO `lfo`'s (1-based) Points shape: `(x, y, curve)` per node.
        fn nodes(mut self, lfo: usize, nodes: &[(f32, f32, f32)]) -> Self {
            self = self.put(format!("lfo{lfo}_nodes"), (nodes.len() - 2) as f32);
            for (i, (x, y, c)) in nodes.iter().enumerate() {
                self = self
                    .put(format!("lfo{lfo}_x{i}"), *x)
                    .put(format!("lfo{lfo}_y{i}"), *y)
                    .put(format!("lfo{lfo}_c{i}"), *c);
            }
            self
        }

        /// LFO `lfo`'s Points shape as steps: one level per step.
        fn steps(self, lfo: usize, levels: &[f32]) -> Self {
            let n = levels.len() as f32;
            let nodes: Vec<(f32, f32, f32)> = levels
                .iter()
                .enumerate()
                .flat_map(|(i, y)| [(i as f32 / n, *y, 0.0), ((i + 1) as f32 / n, *y, 0.0)])
                .collect();
            self.nodes(lfo, &nodes)
        }

        /// Oscillator `osc`'s ("a", "b" or "c") remap curve: `(x, y, curve)`
        /// per node.
        fn remap(mut self, osc: &str, nodes: &[(f32, f32, f32)]) -> Self {
            self = self.put(format!("osc_{osc}_remap_nodes"), (nodes.len() - 2) as f32);
            for (i, (x, y, c)) in nodes.iter().enumerate() {
                self = self
                    .put(format!("osc_{osc}_rx{i}"), *x)
                    .put(format!("osc_{osc}_ry{i}"), *y)
                    .put(format!("osc_{osc}_rc{i}"), *c);
            }
            self
        }

        /// LFO `lfo`'s custom (32-step) shape.
        fn points(mut self, lfo: usize, points: &[f32; 32]) -> Self {
            for (i, p) in points.iter().enumerate() {
                self = self.put(format!("lfo{lfo}_p{i}"), *p);
            }
            self
        }

        fn done(self) -> BTreeMap<String, f32> {
            self.0
        }
    }

    let patch = |pairs: &[(&str, f32)]| Patch(set(pairs));
    let lfo = wt::lfo_source;
    let env = wt::env_source;
    let mac = wt::macro_source;
    let osc_a = wt::osc_dest(0);
    let osc_b = wt::osc_dest(1);
    // Moves the amp envelope's decay, in octaves of the knob's range.
    let amp_decay = wt::env_dest(0, wt::ENV_DEST_DECAY);
    let osc_c = wt::osc_dest(2);

    vec![
        ("Init: Default", set(&[])),
        // ------------------------------------------------------------ Bass
        (
            "Bass: Reese",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 25.0),
                ("osc_a_blend", 1.0),
                ("osc_a_width", 0.3),
                ("osc_b_position", SAW),
                ("osc_b_octave", 4.0),
                ("osc_b_fine", -12.0),
                ("osc_b_unison", 1.0),
                ("osc_b_detune", 20.0),
                ("osc_b_level", 0.6),
                ("sub_on", 1.0),
                ("filter_cutoff", 700.0),
                ("filter_resonance", 0.25),
                ("filter_drive", 0.5),
                ("filter_keytrack", 0.2),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 120.0),
                ("mod1_amount", 0.1),
                ("lfo1_rate", 0.3),
                ("lfo4_rate", 0.25),
                ("voice_mode", 1.0),
                ("glide", 40.0),
            ])
            .route(2, lfo(0), wt::DEST_CUTOFF, 0.08)
            .route(6, lfo(3), osc_b + wt::DEST_DETUNE, 0.1)
            .route(7, mac(0), wt::DEST_CUTOFF, 0.35)
            .route(8, mac(1), osc_a + wt::DEST_WARP, 0.6)
            .put("osc_a_warp_mode".into(), wt::WARP_ASYM_BI as f32)
            .put("osc_a_warp".into(), 0.5)
            .done(),
        ),
        (
            "Bass: Wobble",
            patch(&[
                ("osc_a_table", 7.0),
                ("osc_a_position", 0.3),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 10.0),
                ("osc_b_position", SAW),
                ("osc_b_octave", 3.0),
                ("osc_b_level", 0.5),
                ("sub_on", 1.0),
                ("sub_level", 0.6),
                ("filter_cutoff", 150.0),
                ("filter_resonance", 0.45),
                ("filter_drive", 0.4),
                ("filter_keytrack", 0.2),
                ("env1_attack", 2.0),
                ("env1_decay", 500.0),
                ("env1_sustain", 1.0),
                ("env1_release", 100.0),
                ("lfo1_mode", 1.0),
                ("lfo1_sync", 1.0),
                ("lfo1_division", 11.0),
                ("mod1_amount", 0.0),
                ("mod2_dest", 20.0),
                ("mod2_amount", 0.6),
                ("mod2_polarity", 1.0),
                ("voice_mode", 1.0),
                ("glide", 30.0),
            ])
            .route(6, lfo(0), osc_a + wt::DEST_POSITION, 0.3)
            .route(7, mac(0), wt::lfo_rate_dest(0), 0.25)
            .route(8, mac(1), wt::DEST_DRIVE, 0.5)
            .done(),
        ),
        (
            "Bass: Sub",
            patch(&[
                ("osc_a_position", TRIANGLE * 0.5),
                ("osc_a_unison", 0.0),
                ("osc_a_level", 0.55),
                ("osc_b_on", 0.0),
                ("sub_on", 1.0),
                ("sub_octave", 4.0),
                ("sub_level", 0.8),
                ("filter_type", 0.0),
                ("filter_cutoff", 500.0),
                ("filter_resonance", 0.05),
                ("filter_drive", 0.3),
                ("filter_keytrack", 0.5),
                ("env1_attack", 3.0),
                ("env1_sustain", 1.0),
                ("env1_release", 90.0),
                ("voice_mode", 2.0),
                ("glide", 25.0),
            ])
            .clear()
            .route(1, mac(0), osc_a + wt::DEST_POSITION, 0.3)
            .route(2, mac(1), wt::DEST_CUTOFF, 0.4)
            .route(3, wt::SRC_VELOCITY, wt::DEST_DRIVE, 0.3)
            .done(),
        ),
        (
            "Bass: Growl",
            patch(&[
                ("osc_a_table", 7.0),
                ("osc_a_position", 0.25),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 12.0),
                ("osc_a_warp_mode", wt::WARP_FM as f32),
                ("osc_a_warp", 0.2),
                ("osc_a_morph_mode", wt::MORPH_SMEAR as f32),
                ("osc_a_morph", 0.2),
                ("osc_b_position", 0.0),
                ("osc_b_octave", 3.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.3),
                ("sub_on", 1.0),
                ("sub_level", 0.5),
                ("filter_type", wt::DIRTY as f32),
                ("filter_cutoff", 900.0),
                ("filter_resonance", 0.35),
                ("filter_drive", 0.45),
                ("filter_keytrack", 0.3),
                ("filter2_on", 1.0),
                ("filter2_type", 2.0),
                ("filter2_cutoff", 60.0),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 150.0),
                ("lfo5_sync", 1.0),
                ("lfo5_division", 7.0),
                ("lfo5_mode", 1.0),
                ("voice_mode", 1.0),
                ("glide", 35.0),
            ])
            .clear()
            .nodes(
                5,
                &[
                    (0.0, 0.0, -0.4),
                    (0.3, 1.0, 0.3),
                    (0.55, 0.35, -0.5),
                    (0.8, 0.85, 0.4),
                    (1.0, 0.0, 0.0),
                ],
            )
            .route(1, lfo(4), osc_a + wt::DEST_POSITION, 0.6)
            .polarity(1, wt::POLARITY_UNIPOLAR)
            .route(2, lfo(4), wt::DEST_CUTOFF, 0.25)
            .route(3, lfo(4), wt::DEST_MORPH_A, 0.4)
            .polarity(3, wt::POLARITY_UNIPOLAR)
            .route(4, mac(0), wt::lfo_rate_dest(4), 0.25)
            .route(5, mac(1), osc_a + wt::DEST_WARP, 0.5)
            .route(6, mac(2), wt::DEST_DRIVE, 0.5)
            .done(),
        ),
        (
            "Bass: Acid",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_b_on", 0.0),
                ("filter_type", wt::ACID as f32),
                ("filter_cutoff", 380.0),
                ("filter_resonance", 0.72),
                ("filter_drive", 0.35),
                ("filter_keytrack", 0.3),
                ("env1_attack", 1.0),
                ("env1_decay", 400.0),
                ("env1_sustain", 0.7),
                ("env1_release", 60.0),
                ("env2_attack", 0.5),
                ("env2_decay", 220.0),
                ("env2_sustain", 0.0),
                ("env2_release", 120.0),
                ("mod1_amount", 0.4),
                ("mod2_source", 0.0),
                ("mod4_amount", 0.25),
                ("voice_mode", 2.0),
                ("glide", 70.0),
                ("glide_always", 0.0),
            ])
            .route(6, mac(0), wt::DEST_CUTOFF, 0.35)
            .route(7, mac(1), wt::DEST_RESONANCE, 0.25)
            .route(8, mac(2), wt::DEST_DRIVE, 0.6)
            .done(),
        ),
        (
            "Bass: Neuro",
            patch(&[
                ("osc_a_table", 4.0),
                ("osc_a_position", 0.4),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 8.0),
                ("osc_a_warp_mode", wt::WARP_BEND_BI as f32),
                ("osc_a_warp", 0.5),
                ("osc_a_morph_mode", wt::MORPH_HARMONIC_STRETCH as f32),
                ("osc_a_morph", 0.1),
                ("osc_b_table", 8.0),
                ("osc_b_position", 0.3),
                ("osc_b_octave", 3.0),
                ("osc_b_level", 0.35),
                ("sub_on", 1.0),
                ("sub_level", 0.45),
                ("filter_type", wt::COMB_NEG as f32),
                ("filter_cutoff", 700.0),
                ("filter_resonance", 0.55),
                ("filter_keytrack", 1.0),
                ("filter_mix", 0.6),
                ("filter2_on", 1.0),
                ("filter2_type", 1.0),
                ("filter2_cutoff", 3500.0),
                ("filter2_resonance", 0.2),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 120.0),
                ("lfo6_sync", 1.0),
                ("lfo6_division", 10.0),
                ("voice_mode", 1.0),
                ("glide", 20.0),
            ])
            .clear()
            .route(1, lfo(5), osc_a + wt::DEST_WARP, 0.45)
            .route(2, lfo(5), wt::DEST_MORPH_A, 0.4)
            .polarity(2, wt::POLARITY_UNIPOLAR)
            .route(3, lfo(5), wt::DEST_CUTOFF, 0.15)
            .route(4, mac(0), osc_a + wt::DEST_POSITION, 0.5)
            .route(5, mac(1), wt::DEST_FILTER_MIX, 0.4)
            .route(6, mac(2), wt::lfo_rate_dest(5), 0.25)
            .done(),
        ),
        (
            "Bass: FM",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 0.0),
                ("osc_a_warp_mode", wt::WARP_FM as f32),
                ("osc_a_warp", 0.15),
                ("osc_b_position", 0.0),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.0),
                ("sub_on", 1.0),
                ("sub_level", 0.4),
                ("filter_on", 0.0),
                ("env1_attack", 1.0),
                ("env1_decay", 700.0),
                ("env1_sustain", 0.6),
                ("env1_release", 120.0),
                ("env4_decay", 180.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_WARP, 0.55)
            .curve(1, -0.4)
            .route(2, wt::SRC_VELOCITY, osc_a + wt::DEST_WARP, 0.2)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.5)
            .route(4, mac(1), osc_b + wt::DEST_PITCH, 0.5)
            .done(),
        ),
        (
            "Bass: Detuned Square",
            patch(&[
                ("osc_a_table", 1.0),
                ("osc_a_position", 0.2),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 16.0),
                ("osc_a_detune_mode", wt::DETUNE_SUPER as f32),
                ("osc_b_on", 0.0),
                ("sub_on", 1.0),
                ("sub_shape", 3.0),
                ("sub_level", 0.3),
                ("filter_type", wt::LADDER as f32),
                ("filter_cutoff", 1100.0),
                ("filter_resonance", 0.3),
                ("filter_drive", 0.3),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 150.0),
                ("lfo1_rate", 0.4),
            ])
            .route(2, lfo(0), osc_a + wt::DEST_POSITION, 0.25)
            .route(6, mac(0), osc_a + wt::DEST_POSITION, 0.6)
            .route(7, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Bass: Remap Hollow",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_a_warp_mode", wt::WARP_REMAP_X4 as f32),
                ("osc_a_warp", 0.35),
                ("osc_b_on", 0.0),
                ("sub_on", 1.0),
                ("sub_level", 0.45),
                ("filter_type", wt::LP12 as f32),
                ("filter_cutoff", 1800.0),
                ("filter_resonance", 0.1),
                ("filter_drive", 0.3),
                ("filter_keytrack", 0.5),
                ("env1_attack", 2.0),
                ("env1_decay", 500.0),
                ("env1_sustain", 0.85),
                ("env1_release", 90.0),
                ("env2_attack", 0.5),
                ("env2_decay", 280.0),
                ("env2_sustain", 0.15),
                ("env2_release", 100.0),
                ("voice_mode", 2.0),
                ("glide", 30.0),
            ])
            .clear()
            .remap("a", &[(0.0, 0.0, 0.6), (0.5, 0.5, -0.6), (1.0, 1.0, 0.0)])
            .route(1, env(1), osc_a + wt::DEST_WARP, 0.45)
            .route(2, wt::SRC_VELOCITY, osc_a + wt::DEST_WARP, 0.15)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.5)
            .route(4, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Bass: Tearout",
            patch(&[
                ("osc_a_table", 7.0),
                ("osc_a_position", 0.5),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 10.0),
                ("osc_a_warp_mode", wt::WARP_BEND_BI as f32),
                ("osc_a_warp", 0.5),
                ("osc_a_warp2_mode", wt::WARP_DRIVE as f32),
                ("osc_a_warp2", 0.3),
                ("osc_a_morph_mode", wt::MORPH_HARMONIC_STRETCH as f32),
                ("osc_a_morph", 0.15),
                ("osc_b_on", 0.0),
                ("sub_on", 1.0),
                ("sub_level", 0.5),
                ("filter_sub", 0.0),
                ("filter_type", wt::DIRTY as f32),
                ("filter_cutoff", 1400.0),
                ("filter_resonance", 0.4),
                ("filter_drive", 0.55),
                ("filter_keytrack", 0.3),
                ("filter2_on", 1.0),
                ("filter2_type", wt::HP12 as f32),
                ("filter2_cutoff", 35.0),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 110.0),
                ("lfo5_sync", 1.0),
                ("lfo5_division", 11.0),
                ("lfo5_mode", 1.0),
                ("voice_mode", 1.0),
                ("glide", 20.0),
                ("quality", 1.0),
            ])
            .clear()
            .nodes(
                5,
                &[
                    (0.0, 1.0, 0.5),
                    (0.25, 0.15, -0.3),
                    (0.5, 0.8, 0.5),
                    (0.75, 0.0, -0.4),
                    (1.0, 1.0, 0.0),
                ],
            )
            .route(1, lfo(4), osc_a + wt::DEST_POSITION, 0.5)
            .polarity(1, wt::POLARITY_UNIPOLAR)
            .route(2, lfo(4), wt::DEST_CUTOFF, 0.3)
            .route(3, lfo(4), osc_a + wt::DEST_WARP, 0.3)
            .route(4, mac(0), wt::lfo_rate_dest(4), 0.25)
            .route(5, mac(1), wt::warp2_dest(0), 0.6)
            .route(6, mac(2), wt::DEST_DRIVE, 0.4)
            .done(),
        ),
        (
            "Bass: Phaser Sweep",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 12.0),
                ("osc_b_position", SQUARE),
                ("osc_b_octave", 3.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.4),
                ("filter_type", wt::PHASER as f32),
                ("filter_cutoff", 800.0),
                ("filter_resonance", 0.6),
                ("filter_blend", 0.5),
                ("filter_keytrack", 0.3),
                ("filter2_on", 1.0),
                ("filter2_type", wt::LP24 as f32),
                ("filter2_cutoff", 2500.0),
                ("filter2_resonance", 0.1),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 120.0),
                ("env2_attack", 1.0),
                ("env2_decay", 300.0),
                ("env2_sustain", 0.3),
                ("lfo1_rate", 0.25),
                ("voice_mode", 1.0),
                ("glide", 25.0),
            ])
            .clear()
            .route(1, lfo(0), wt::DEST_CUTOFF, 0.2)
            .route(2, env(1), wt::DEST_CUTOFF2, 0.2)
            .route(3, mac(0), wt::DEST_CUTOFF2, 0.4)
            .route(4, mac(1), wt::DEST_RESONANCE, 0.3)
            .done(),
        ),
        // ------------------------------------------------------------ Lead
        (
            "Lead: Supersaw",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 8.0),
                ("osc_a_detune", 22.0),
                ("osc_a_detune_mode", wt::DETUNE_SUPER as f32),
                ("osc_a_blend", 0.8),
                ("osc_a_width", 1.0),
                ("osc_a_level", 0.75),
                ("osc_b_position", SAW),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 6.0),
                ("osc_b_detune", 18.0),
                ("osc_b_width", 1.0),
                ("osc_b_level", 0.35),
                ("filter_type", 0.0),
                ("filter_cutoff", 6000.0),
                ("filter_resonance", 0.15),
                ("filter_keytrack", 0.5),
                ("env1_attack", 5.0),
                ("env1_decay", 400.0),
                ("env1_sustain", 0.85),
                ("env1_release", 300.0),
                ("mod1_amount", 0.15),
            ])
            .route(6, mac(0), osc_a + wt::DEST_DETUNE, 0.3)
            .route(7, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Lead: Glide",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 12.0),
                ("osc_b_position", SQUARE),
                ("osc_b_octave", 5.0),
                ("osc_b_level", 0.3),
                ("filter_cutoff", 2500.0),
                ("filter_resonance", 0.35),
                ("filter_drive", 0.3),
                ("env1_attack", 5.0),
                ("env1_decay", 600.0),
                ("env1_sustain", 0.9),
                ("env1_release", 200.0),
                ("mod2_source", 0.0),
                ("voice_mode", 2.0),
                ("glide", 80.0),
                ("glide_curve", -0.5),
            ])
            .route(6, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(7, mac(1), wt::lfo_dest(1, wt::LFO_DEST_RISE), 0.15)
            .done(),
        ),
        (
            "Lead: Sync Scream",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 10.0),
                ("osc_a_warp_mode", wt::WARP_SYNC as f32),
                ("osc_a_warp", 0.2),
                ("osc_b_position", SQUARE),
                ("osc_b_octave", 3.0),
                ("osc_b_level", 0.25),
                ("filter_type", 1.0),
                ("filter_cutoff", 4000.0),
                ("filter_resonance", 0.25),
                ("filter_drive", 0.45),
                ("env1_attack", 3.0),
                ("env1_sustain", 0.9),
                ("env1_release", 250.0),
                ("env4_attack", 1.0),
                ("env4_decay", 450.0),
                ("env4_sustain", 0.2),
                ("lfo2_rise", 0.8),
                ("lfo2_delay", 0.25),
                ("voice_mode", 2.0),
                ("glide", 45.0),
                ("quality", 1.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_WARP, 0.55)
            .route(2, wt::SRC_MOD_WHEEL, osc_a + wt::DEST_WARP, 0.3)
            .route(3, lfo(1), wt::DEST_GLOBAL_PITCH, 0.3 / 24.0)
            .route(4, mac(0), osc_a + wt::DEST_WARP, 0.4)
            .route(5, mac(1), wt::DEST_DRIVE, 0.5)
            .done(),
        ),
        (
            "Lead: Hoover",
            patch(&[
                ("osc_a_table", 1.0),
                ("osc_a_position", 0.5),
                ("osc_a_unison", 6.0),
                ("osc_a_detune", 35.0),
                ("osc_a_stack", 7.0),
                ("osc_a_warp_mode", wt::WARP_PWM as f32),
                ("osc_a_warp", 0.3),
                ("osc_a_width", 1.0),
                ("osc_b_position", SAW),
                ("osc_b_unison", 4.0),
                ("osc_b_detune", 30.0),
                ("osc_b_level", 0.45),
                ("filter_cutoff", 5000.0),
                ("filter_resonance", 0.1),
                ("env1_attack", 10.0),
                ("env1_sustain", 1.0),
                ("env1_release", 400.0),
                ("env4_attack", 0.5),
                ("env4_decay", 300.0),
                ("env4_sustain", 0.0),
                ("lfo1_rate", 1.5),
                ("voice_mode", 2.0),
                ("glide", 120.0),
            ])
            .clear()
            .route(1, env(3), wt::DEST_GLOBAL_PITCH, -0.25)
            .route(2, lfo(0), osc_a + wt::DEST_WARP, 0.25)
            .route(3, mac(0), osc_a + wt::DEST_DETUNE, 0.4)
            .route(4, mac(1), wt::DEST_CUTOFF, -0.4)
            .done(),
        ),
        (
            "Lead: Chip",
            patch(&[
                ("osc_a_table", 9.0),
                ("osc_a_position", 0.6),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_a_warp_mode", wt::WARP_QUANTIZE as f32),
                ("osc_a_warp", 0.35),
                ("osc_b_on", 0.0),
                ("filter_on", 0.0),
                ("env1_attack", 1.0),
                ("env1_decay", 200.0),
                ("env1_sustain", 0.8),
                ("env1_release", 80.0),
                ("lfo2_rate", 6.0),
                ("lfo2_rise", 0.2),
                ("lfo2_delay", 0.3),
                ("voice_mode", 1.0),
                ("glide", 15.0),
            ])
            .clear()
            .route(1, lfo(1), wt::DEST_GLOBAL_PITCH, 0.4 / 24.0)
            .route(2, mac(0), osc_a + wt::DEST_WARP, 0.6)
            .route(3, mac(1), osc_a + wt::DEST_POSITION, 0.4)
            .done(),
        ),
        (
            "Lead: Vocal",
            patch(&[
                ("osc_a_level", 1.0),
                ("master_volume", 6.0),
                ("osc_a_table", 4.0),
                ("osc_a_position", 0.2),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 9.0),
                ("osc_a_morph_mode", wt::MORPH_VOCODE as f32),
                ("osc_a_morph", 0.5),
                ("osc_b_position", SAW),
                ("osc_b_level", 0.2),
                ("filter_type", wt::FORMANT as f32),
                ("filter_cutoff", 400.0),
                ("filter_resonance", 0.4),
                ("filter_mix", 0.7),
                ("env1_attack", 25.0),
                ("env1_sustain", 0.9),
                ("env1_release", 300.0),
                ("lfo1_rate", 0.6),
                ("lfo1_mode", 1.0),
                ("voice_mode", 2.0),
                ("glide", 60.0),
            ])
            .clear()
            .route(1, lfo(0), wt::DEST_CUTOFF, 0.18)
            .route(2, lfo(0), osc_a + wt::DEST_POSITION, 0.3)
            .route(3, mac(0), wt::DEST_CUTOFF, 0.4)
            .route(4, mac(1), wt::DEST_MORPH_A, 0.4)
            .route(5, lfo(1), wt::DEST_GLOBAL_PITCH, 0.3 / 24.0)
            .aux(5, wt::SRC_MOD_WHEEL)
            .done(),
        ),
        (
            "Lead: Fold",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 8.0),
                ("osc_a_warp_mode", wt::WARP_FOLD as f32),
                ("osc_a_warp", 0.3),
                ("osc_a_warp2_mode", wt::WARP_DRIVE as f32),
                ("osc_a_warp2", 0.2),
                ("osc_b_on", 0.0),
                ("filter_cutoff", 5000.0),
                ("filter_resonance", 0.2),
                ("env1_attack", 4.0),
                ("env1_sustain", 0.85),
                ("env1_release", 300.0),
                ("env4_attack", 2.0),
                ("env4_decay", 600.0),
                ("env4_sustain", 0.3),
                ("quality", 1.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_WARP, 0.5)
            .route(2, wt::SRC_VELOCITY, osc_a + wt::DEST_WARP, 0.2)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.5)
            .route(4, mac(1), wt::warp2_dest(0), 0.6)
            .done(),
        ),
        (
            "Lead: Three-Osc Stack",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 4.0),
                ("osc_a_detune", 16.0),
                ("osc_a_level", 0.6),
                ("osc_b_position", 1.0),
                ("osc_b_octave", 3.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.35),
                ("osc_c_on", 1.0),
                ("osc_c_position", SAW),
                ("osc_c_semi", 19.0),
                ("osc_c_unison", 3.0),
                ("osc_c_detune", 20.0),
                ("osc_c_width", 1.0),
                ("osc_c_level", 0.35),
                ("filter_cutoff", 3000.0),
                ("filter_resonance", 0.25),
                ("env1_attack", 10.0),
                ("env1_sustain", 0.85),
                ("env1_release", 350.0),
            ])
            .route(6, lfo(0), osc_c + wt::DEST_POSITION, 0.08)
            .route(7, mac(0), osc_c + wt::DEST_LEVEL, 0.4)
            .route(8, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Lead: Remap Razor",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 14.0),
                ("osc_a_width", 0.8),
                ("osc_a_warp_mode", wt::WARP_REMAP_MIRROR as f32),
                ("osc_a_warp", 0.55),
                ("osc_b_position", SAW),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 1.0),
                ("osc_b_detune", 10.0),
                ("osc_b_level", 0.25),
                ("filter_cutoff", 5000.0),
                ("filter_resonance", 0.2),
                ("filter_keytrack", 0.5),
                ("env1_attack", 3.0),
                ("env1_decay", 600.0),
                ("env1_sustain", 0.85),
                ("env1_release", 250.0),
                ("env4_attack", 1.0),
                ("env4_decay", 400.0),
                ("env4_sustain", 0.25),
                ("lfo2_rate", 5.5),
                ("lfo2_rise", 0.3),
                ("lfo2_delay", 0.2),
                ("voice_mode", 2.0),
                ("glide", 35.0),
            ])
            .clear()
            .remap("a", &[(0.0, 0.0, -0.6), (1.0, 1.0, 0.0)])
            .route(1, env(3), osc_a + wt::DEST_WARP, 0.35)
            .route(2, wt::SRC_MOD_WHEEL, osc_a + wt::DEST_WARP, 0.3)
            .route(3, lfo(1), wt::DEST_GLOBAL_PITCH, 0.3 / 24.0)
            .aux(3, wt::SRC_MOD_WHEEL)
            .route(4, mac(0), osc_a + wt::DEST_WARP, 0.4)
            .route(5, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Lead: Phaser Saw",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 12.0),
                ("osc_b_on", 0.0),
                ("filter_type", wt::PHASER as f32),
                ("filter_cutoff", 1200.0),
                ("filter_resonance", 0.5),
                ("filter_blend", 0.7),
                ("filter2_on", 1.0),
                ("filter2_type", wt::LP12 as f32),
                ("filter2_cutoff", 6000.0),
                ("env1_attack", 5.0),
                ("env1_decay", 500.0),
                ("env1_sustain", 0.9),
                ("env1_release", 300.0),
                ("lfo1_mode", 0.0),
                ("lfo1_mono", 1.0),
                ("lfo1_rate", 0.2),
                ("lfo2_rate", 5.0),
                ("voice_mode", 2.0),
                ("glide", 40.0),
            ])
            .clear()
            .route(1, lfo(0), wt::DEST_CUTOFF, 0.25)
            .route(2, mac(0), wt::DEST_CUTOFF, 0.4)
            .route(3, mac(1), wt::DEST_RESONANCE, 0.3)
            .route(4, lfo(1), wt::DEST_GLOBAL_PITCH, 0.25 / 24.0)
            .aux(4, wt::SRC_MOD_WHEEL)
            .done(),
        ),
        (
            "Lead: Squeeze Vox",
            patch(&[
                ("master_volume", 6.0),
                ("osc_a_table", 4.0),
                ("osc_a_position", 0.3),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 8.0),
                ("osc_a_warp_mode", wt::WARP_SQUEEZE as f32),
                ("osc_a_warp", 0.3),
                ("osc_b_on", 0.0),
                ("filter_type", wt::FORMANT as f32),
                ("filter_cutoff", 600.0),
                ("filter_resonance", 0.45),
                ("filter_mix", 0.6),
                ("env1_attack", 15.0),
                ("env1_sustain", 0.9),
                ("env1_release", 280.0),
                ("lfo1_mode", 1.0),
                ("lfo1_rate", 0.4),
                ("voice_mode", 2.0),
                ("glide", 50.0),
            ])
            .clear()
            .route(1, lfo(0), osc_a + wt::DEST_WARP, 0.3)
            .route(2, lfo(0), wt::DEST_CUTOFF, 0.15)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.5)
            .route(4, mac(1), wt::DEST_CUTOFF, 0.4)
            .route(5, lfo(1), wt::DEST_GLOBAL_PITCH, 0.3 / 24.0)
            .aux(5, wt::SRC_MOD_WHEEL)
            .done(),
        ),
        // ----------------------------------------------------------- Brass
        (
            "Brass: Synth Brass",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 9.0),
                ("osc_b_position", SAW),
                ("osc_b_octave", 4.0),
                ("osc_b_fine", 7.0),
                ("osc_b_unison", 1.0),
                ("osc_b_detune", 6.0),
                ("osc_b_level", 0.5),
                ("filter_cutoff", 700.0),
                ("filter_resonance", 0.15),
                ("filter_drive", 0.2),
                ("filter_keytrack", 0.6),
                ("env1_attack", 35.0),
                ("env1_decay", 800.0),
                ("env1_sustain", 0.8),
                ("env1_release", 280.0),
                ("env2_attack", 70.0),
                ("env2_decay", 700.0),
                ("env2_sustain", 0.45),
                ("env2_release", 300.0),
                ("mod1_amount", 0.35),
                ("mod2_source", 0.0),
                ("velocity", 0.7),
            ])
            .route(6, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(7, mac(1), osc_a + wt::DEST_DETUNE, 0.2)
            .done(),
        ),
        (
            "Brass: Stab",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 12.0),
                ("osc_a_width", 0.7),
                ("osc_b_position", SQUARE),
                ("osc_b_level", 0.3),
                ("filter_type", wt::LADDER as f32),
                ("filter_cutoff", 500.0),
                ("filter_resonance", 0.25),
                ("filter_drive", 0.35),
                ("filter_keytrack", 0.5),
                ("env1_attack", 8.0),
                ("env1_decay", 350.0),
                ("env1_sustain", 0.4),
                ("env1_release", 200.0),
                ("env2_attack", 10.0),
                ("env2_decay", 250.0),
                ("env2_sustain", 0.2),
                ("mod1_amount", 0.45),
                ("mod2_source", 0.0),
            ])
            .route(6, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(7, mac(1), wt::DEST_DRIVE, 0.4)
            .done(),
        ),
        // ------------------------------------------------------------- Pad
        (
            "Pad: Warm",
            patch(&[
                ("osc_a_table", 3.0),
                ("osc_a_position", 0.4),
                ("osc_a_unison", 6.0),
                ("osc_a_detune", 18.0),
                ("osc_a_width", 1.0),
                ("osc_b_table", 2.0),
                ("osc_b_position", 0.3),
                ("osc_b_octave", 5.0),
                ("osc_b_level", 0.25),
                ("filter_type", 0.0),
                ("filter_cutoff", 2500.0),
                ("filter_resonance", 0.1),
                ("env1_attack", 900.0),
                ("env1_decay", 2000.0),
                ("env1_sustain", 0.8),
                ("env1_release", 2500.0),
                ("env2_attack", 1500.0),
                ("env2_decay", 3000.0),
                ("env2_sustain", 0.5),
                ("mod1_amount", 0.15),
                ("lfo1_rate", 0.15),
                ("mod2_amount", 0.15),
            ])
            .route(6, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(7, mac(1), osc_a + wt::DEST_POSITION, 0.4)
            .route(8, wt::SRC_RANDOM, wt::lfo_dest(0, wt::LFO_DEST_PHASE), 0.5)
            .done(),
        ),
        (
            "Pad: Glass Choir",
            patch(&[
                ("osc_a_table", 12.0),
                ("osc_a_position", 0.3),
                ("osc_a_unison", 4.0),
                ("osc_a_detune", 12.0),
                ("osc_a_width", 1.0),
                ("osc_a_morph_mode", wt::MORPH_FORMANT_SCALE as f32),
                ("osc_a_morph", 0.55),
                ("osc_b_table", 4.0),
                ("osc_b_position", 0.5),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 3.0),
                ("osc_b_detune", 15.0),
                ("osc_b_level", 0.3),
                ("filter_type", 0.0),
                ("filter_cutoff", 6000.0),
                ("filter_resonance", 0.15),
                ("env1_attack", 700.0),
                ("env1_decay", 2000.0),
                ("env1_sustain", 0.9),
                ("env1_release", 3000.0),
                ("lfo1_rate", 0.1),
                ("lfo7_rate", 0.17),
            ])
            .clear()
            .route(1, lfo(0), osc_a + wt::DEST_POSITION, 0.25)
            .route(2, lfo(6), wt::DEST_MORPH_A, 0.15)
            .route(3, lfo(6), osc_b + wt::DEST_POSITION, 0.2)
            .route(4, mac(0), wt::DEST_MORPH_A, 0.4)
            .route(5, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Pad: Evolving",
            patch(&[
                ("osc_a_table", 3.0),
                ("osc_a_position", 0.5),
                ("osc_a_unison", 5.0),
                ("osc_a_detune", 16.0),
                ("osc_a_width", 1.0),
                ("osc_a_position_spread", 0.3),
                ("osc_a_morph_mode", wt::MORPH_PHASE_DISPERSE as f32),
                ("osc_a_morph", 0.3),
                ("osc_b_table", 11.0),
                ("osc_b_position", 0.2),
                ("osc_b_level", 0.3),
                ("filter_type", wt::MORPH as f32),
                ("filter_cutoff", 1500.0),
                ("filter_resonance", 0.3),
                ("filter_blend", 0.2),
                ("env1_attack", 1200.0),
                ("env1_decay", 3000.0),
                ("env1_sustain", 0.85),
                ("env1_release", 3500.0),
                ("lfo8_mono", 1.0),
                ("lfo8_rate", 0.12),
                ("lfo4_rate", 0.2),
            ])
            .clear()
            .route(1, lfo(7), osc_a + wt::DEST_POSITION, 0.35)
            .route(2, lfo(7), wt::DEST_FILTER_BLEND, 0.3)
            .route(3, lfo(3), wt::DEST_MORPH_A, 0.3)
            .route(4, lfo(3), osc_b + wt::DEST_POSITION, 0.3)
            .route(5, mac(0), wt::DEST_CUTOFF, 0.35)
            .route(6, mac(1), wt::DEST_FILTER_BLEND, 0.6)
            .done(),
        ),
        (
            "Pad: Dark Strings",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 6.0),
                ("osc_a_detune", 20.0),
                ("osc_a_detune_mode", wt::DETUNE_SUPER as f32),
                ("osc_a_width", 1.0),
                ("osc_b_position", SAW),
                ("osc_b_octave", 3.0),
                ("osc_b_unison", 4.0),
                ("osc_b_detune", 14.0),
                ("osc_b_level", 0.35),
                ("filter_type", 1.0),
                ("filter_cutoff", 900.0),
                ("filter_resonance", 0.15),
                ("filter_keytrack", 0.5),
                ("env1_attack", 450.0),
                ("env1_decay", 1500.0),
                ("env1_sustain", 0.85),
                ("env1_release", 1800.0),
                ("env3_attack", 1800.0),
                ("env3_decay", 2500.0),
                ("env3_sustain", 0.6),
            ])
            .clear()
            .route(1, env(2), wt::DEST_CUTOFF, 0.2)
            .route(2, wt::SRC_MOD_WHEEL, wt::DEST_CUTOFF, 0.25)
            .route(3, wt::SRC_RANDOM, osc_a + wt::DEST_FINE, 0.05)
            .route(4, mac(0), wt::DEST_CUTOFF, 0.35)
            .route(5, mac(1), osc_a + wt::DEST_DETUNE, 0.3)
            .route(6, wt::SRC_AFTERTOUCH, wt::DEST_VOLUME, 4.0 / 66.0)
            .done(),
        ),
        (
            "Pad: Harmonic Drift",
            patch(&[
                ("master_volume", 3.0),
                ("osc_a_table", 2.0),
                ("osc_a_position", 0.35),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 10.0),
                ("osc_a_morph_mode", wt::MORPH_SKEW as f32),
                ("osc_a_morph", 0.3),
                ("osc_b_on", 0.0),
                ("noise_on", 1.0),
                ("noise_type", 1.0),
                ("noise_level", 0.08),
                ("filter_cutoff", 4000.0),
                ("filter_resonance", 0.2),
                ("env1_attack", 1500.0),
                ("env1_decay", 2000.0),
                ("env1_sustain", 1.0),
                ("env1_release", 4000.0),
                ("lfo1_rate", 0.07),
                ("lfo4_rate", 0.3),
            ])
            .clear()
            .route(1, lfo(0), wt::DEST_MORPH_A, 0.3)
            .route(2, lfo(3), osc_a + wt::DEST_POSITION, 0.2)
            .route(3, mac(0), wt::DEST_MORPH_A, 0.5)
            .route(4, mac(1), wt::DEST_NOISE_LEVEL, 0.3)
            .done(),
        ),
        // ----------------------------------------------------------- Pluck
        (
            "Pluck: Classic",
            patch(&[
                ("master_volume", 3.0),
                ("osc_a_position", SAW),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 10.0),
                ("osc_b_position", 1.0),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.3),
                ("filter_cutoff", 400.0),
                ("filter_resonance", 0.3),
                ("filter_keytrack", 0.6),
                ("env1_attack", 1.0),
                ("env1_decay", 350.0),
                ("env1_sustain", 0.0),
                ("env1_release", 300.0),
                ("env1_decay_curve", -0.7),
                ("env2_attack", 0.5),
                ("env2_decay", 250.0),
                ("env2_sustain", 0.0),
                ("env2_release", 250.0),
                ("mod1_amount", 0.45),
                ("mod2_source", 0.0),
            ])
            .route(6, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(7, mac(1), wt::DEST_RESONANCE, 0.3)
            .route(
                8,
                wt::SRC_VELOCITY,
                wt::env_dest(1, wt::ENV_DEST_DECAY),
                0.08,
            )
            .done(),
        ),
        (
            "Pluck: String",
            patch(&[
                ("noise_type", 1.0),
                ("master_volume", 6.0),
                ("osc_a_on", 0.0),
                ("osc_b_on", 0.0),
                ("noise_on", 1.0),
                ("noise_level", 1.0),
                ("filter_type", wt::COMB_POS as f32),
                ("filter_cutoff", 261.63),
                ("filter_resonance", 0.97),
                ("filter_drive", 0.3),
                ("filter_keytrack", 1.0),
                ("env1_attack", 0.5),
                ("env1_decay", 1500.0),
                ("env1_sustain", 0.0),
                ("env1_release", 600.0),
                ("env4_attack", 0.5),
                ("env4_decay", 60.0),
                ("env4_sustain", 0.0),
                ("env4_release", 20.0),
            ])
            .clear()
            .route(1, env(3), wt::DEST_NOISE_LEVEL, 1.0)
            .route(2, wt::SRC_FIXED, wt::DEST_NOISE_LEVEL, -0.9)
            .route(3, mac(0), wt::DEST_RESONANCE, -0.3)
            .route(4, wt::SRC_VELOCITY, wt::DEST_AMP, 0.3)
            .done(),
        ),
        (
            "Pluck: Marimba",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 0.0),
                ("osc_a_warp_mode", wt::WARP_FM as f32),
                ("osc_a_warp", 0.0),
                ("osc_b_position", 0.0),
                ("osc_b_octave", 6.0),
                ("osc_b_semi", 16.0),
                ("osc_b_level", 0.0),
                ("osc_b_unison", 0.0),
                ("filter_on", 0.0),
                ("env1_attack", 0.5),
                ("env1_decay", 600.0),
                ("env1_sustain", 0.0),
                ("env1_release", 500.0),
                ("env1_decay_curve", -0.8),
                ("env4_decay", 90.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_WARP, 0.35)
            .curve(1, -0.5)
            .route(2, wt::SRC_VELOCITY, osc_a + wt::DEST_WARP, 0.15)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.4)
            .route(4, wt::SRC_NOTE, amp_decay, -0.1)
            .done(),
        ),
        (
            "Pluck: Chord Stab",
            patch(&[
                ("master_volume", 6.0),
                ("osc_a_table", 11.0),
                ("osc_a_position", 0.2),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 12.0),
                ("osc_a_width", 0.9),
                ("osc_b_on", 0.0),
                ("filter_type", 1.0),
                ("filter_cutoff", 900.0),
                ("filter_resonance", 0.25),
                ("filter_keytrack", 0.5),
                ("env1_attack", 1.0),
                ("env1_decay", 450.0),
                ("env1_sustain", 0.0),
                ("env1_release", 300.0),
                ("env2_decay", 300.0),
                ("env2_sustain", 0.0),
                ("mod1_amount", 0.5),
                ("mod2_source", 0.0),
            ])
            .route(6, mac(0), osc_a + wt::DEST_POSITION, 0.8)
            .route(7, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Pluck: Decaying Table",
            patch(&[
                ("master_volume", 2.0),
                ("osc_a_table", wt::choice_of(16) as f32),
                ("osc_a_position", 0.0),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 8.0),
                ("osc_b_table", wt::choice_of(18) as f32),
                ("osc_b_position", 0.6),
                ("osc_b_octave", 5.0),
                ("osc_b_level", 0.2),
                ("filter_type", 0.0),
                ("filter_cutoff", 7000.0),
                ("filter_resonance", 0.1),
                ("env1_attack", 0.5),
                ("env1_decay", 900.0),
                ("env1_sustain", 0.0),
                ("env1_release", 500.0),
                ("env4_decay", 700.0),
                ("env4_sustain", 0.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_POSITION, -1.0)
            .route(2, wt::SRC_FIXED, osc_a + wt::DEST_POSITION, 1.0)
            .route(3, mac(0), osc_b + wt::DEST_LEVEL, 0.5)
            .route(4, wt::SRC_VELOCITY, wt::DEST_CUTOFF, 0.2)
            .done(),
        ),
        (
            "Pluck: Glass Remap",
            patch(&[
                ("master_volume", 3.0),
                ("osc_a_table", 12.0),
                ("osc_a_position", 0.3),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 6.0),
                ("osc_a_warp_mode", wt::WARP_REMAP as f32),
                ("osc_a_warp", 0.0),
                ("osc_b_on", 0.0),
                ("filter_cutoff", 3000.0),
                ("filter_resonance", 0.25),
                ("filter_keytrack", 0.6),
                ("env1_attack", 0.5),
                ("env1_decay", 700.0),
                ("env1_sustain", 0.0),
                ("env1_release", 400.0),
                ("env2_decay", 300.0),
                ("env2_sustain", 0.0),
                ("env4_attack", 0.5),
                ("env4_decay", 250.0),
                ("env4_sustain", 0.0),
                ("mod1_amount", 0.35),
            ])
            .remap("a", &[(0.0, 0.0, 0.0), (0.5, 1.0, 0.0), (1.0, 0.0, 0.0)])
            .route(6, env(3), osc_a + wt::DEST_WARP, 0.7)
            .route(7, mac(0), osc_a + wt::DEST_WARP, 0.4)
            .route(8, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Pluck: Dirty",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 8.0),
                ("osc_b_position", SQUARE),
                ("osc_b_octave", 3.0),
                ("osc_b_level", 0.3),
                ("filter_type", wt::DIRTY as f32),
                ("filter_cutoff", 300.0),
                ("filter_resonance", 0.55),
                ("filter_drive", 0.6),
                ("filter_keytrack", 0.5),
                ("env1_attack", 0.5),
                ("env1_decay", 450.0),
                ("env1_sustain", 0.0),
                ("env1_release", 250.0),
                ("env2_attack", 0.5),
                ("env2_decay", 220.0),
                ("env2_sustain", 0.0),
                ("mod1_amount", 0.5),
                ("mod2_source", 0.0),
            ])
            .route(6, mac(0), wt::DEST_RESONANCE, 0.3)
            .route(7, mac(1), wt::DEST_DRIVE, 0.4)
            .done(),
        ),
        (
            "Pluck: Kalimba",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_a_warp_mode", wt::WARP_FM as f32),
                ("osc_a_warp", 0.05),
                ("osc_b_position", 0.0),
                ("osc_b_octave", 6.0),
                ("osc_b_semi", 17.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.0),
                ("filter_on", 0.0),
                ("env1_attack", 0.5),
                ("env1_decay", 900.0),
                ("env1_sustain", 0.0),
                ("env1_release", 500.0),
                ("env1_decay_curve", -0.7),
                ("env4_attack", 0.5),
                ("env4_decay", 120.0),
                ("env4_sustain", 0.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_WARP, 0.5)
            .route(2, wt::SRC_VELOCITY, osc_a + wt::DEST_WARP, 0.2)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.3)
            .route(4, wt::SRC_VELOCITY, amp_decay, 0.06)
            .route(5, mac(1), amp_decay, 0.1)
            .done(),
        ),
        // ----------------------------------------------------------- Chord
        (
            "Chord: Fifth Stack",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 10.0),
                ("osc_a_stack", 4.0),
                ("osc_b_position", SQUARE),
                ("osc_b_octave", 3.0),
                ("osc_b_level", 0.3),
                ("filter_cutoff", 2500.0),
                ("filter_resonance", 0.2),
                ("env1_attack", 1.0),
                ("env1_decay", 400.0),
                ("env1_sustain", 0.2),
                ("env1_release", 300.0),
                ("env2_decay", 250.0),
                ("env2_sustain", 0.1),
                ("mod1_amount", 0.35),
                ("mod2_source", 0.0),
            ])
            .route(6, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(7, mac(1), osc_a + wt::DEST_DETUNE, 0.2)
            .done(),
        ),
        // ------------------------------------------------------------ Keys
        (
            "Keys: FM Bells",
            patch(&[
                ("osc_a_table", wt::choice_of(14) as f32),
                ("osc_a_position", 0.42),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 4.0),
                ("osc_b_on", 0.0),
                ("filter_type", 0.0),
                ("filter_cutoff", 9000.0),
                ("env1_attack", 1.0),
                ("env1_decay", 2500.0),
                ("env1_sustain", 0.0),
                ("env1_release", 1500.0),
                ("env4_decay", 1200.0),
                ("env4_sustain", 0.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_POSITION, 0.12)
            .route(2, mac(0), osc_a + wt::DEST_POSITION, 0.5)
            .route(3, wt::SRC_VELOCITY, osc_a + wt::DEST_POSITION, 0.06)
            .done(),
        ),
        (
            "Keys: Bell",
            patch(&[
                ("osc_a_table", 12.0),
                ("osc_a_position", 0.5),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 6.0),
                ("osc_b_table", 2.0),
                ("osc_b_position", 0.6),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.3),
                ("filter_type", 0.0),
                ("filter_cutoff", 5000.0),
                ("filter_resonance", 0.1),
                ("filter_keytrack", 0.6),
                ("env1_attack", 1.0),
                ("env1_decay", 1800.0),
                ("env1_sustain", 0.0),
                ("env1_release", 1200.0),
                ("env2_attack", 1.0),
                ("env2_decay", 600.0),
                ("env2_sustain", 0.1),
                ("mod2_source", 0.0),
            ])
            .route(6, mac(0), osc_a + wt::DEST_POSITION, 0.4)
            .route(7, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Keys: E-Piano",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 0.0),
                ("osc_a_warp_mode", wt::WARP_FM as f32),
                ("osc_a_warp", 0.05),
                ("osc_b_position", 0.0),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 0.0),
                ("osc_b_level", 0.0),
                ("osc_c_on", 1.0),
                ("osc_c_position", 0.0),
                ("osc_c_octave", 6.0),
                ("osc_c_level", 0.05),
                ("filter_on", 0.0),
                ("env1_attack", 1.0),
                ("env1_decay", 2500.0),
                ("env1_sustain", 0.25),
                ("env1_release", 450.0),
                ("env4_decay", 500.0),
                ("env4_sustain", 0.0),
                ("lfo1_rate", 4.5),
                ("lfo1_mono", 1.0),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_WARP, 0.2)
            .route(2, wt::SRC_VELOCITY, osc_a + wt::DEST_WARP, 0.2)
            .curve(2, 0.4)
            .route(3, lfo(0), wt::DEST_VOICE_PAN, 0.3)
            .aux(3, mac(0))
            .route(4, lfo(0), wt::DEST_AMP, 0.2)
            .aux(4, mac(0))
            .route(5, mac(1), osc_a + wt::DEST_WARP, 0.3)
            .done(),
        ),
        (
            "Keys: Organ",
            patch(&[
                ("osc_a_table", 10.0),
                ("osc_a_position", 0.4),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_b_on", 0.0),
                ("filter_on", 0.0),
                ("env1_attack", 5.0),
                ("env1_decay", 200.0),
                ("env1_sustain", 1.0),
                ("env1_release", 60.0),
                ("lfo1_rate", 6.5),
                ("lfo1_mono", 1.0),
            ])
            .clear()
            .route(1, mac(0), osc_a + wt::DEST_POSITION, 0.6)
            .route(2, lfo(0), osc_a + wt::DEST_FINE, 0.06)
            .aux(2, wt::SRC_MOD_WHEEL)
            .route(3, lfo(0), wt::DEST_VOICE_PAN, 0.4)
            .aux(3, wt::SRC_MOD_WHEEL)
            .route(4, mac(1), wt::lfo_rate_dest(0), -0.25)
            .done(),
        ),
        (
            "Keys: Clav",
            patch(&[
                ("master_volume", 1.0),
                ("osc_a_table", 1.0),
                ("osc_a_position", 0.8),
                ("osc_a_unison", 0.0),
                ("osc_a_warp_mode", wt::WARP_FLIP as f32),
                ("osc_a_warp", 0.3),
                ("osc_a_level", 1.0),
                ("osc_b_on", 0.0),
                ("filter_type", wt::LP12 as f32),
                ("filter_cutoff", 2200.0),
                ("filter_resonance", 0.35),
                ("filter_keytrack", 0.7),
                ("filter_mix", 0.85),
                ("env1_attack", 0.5),
                ("env1_decay", 700.0),
                ("env1_sustain", 0.3),
                ("env1_release", 90.0),
                ("env2_decay", 150.0),
                ("env2_sustain", 0.0),
                ("mod1_amount", 0.35),
                ("mod2_source", 0.0),
            ])
            .route(6, mac(0), osc_a + wt::DEST_WARP, 0.5)
            .route(7, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Keys: Wurli",
            patch(&[
                ("osc_a_position", TRIANGLE),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_a_warp_mode", wt::WARP_ASYM_POS as f32),
                ("osc_a_warp", 0.3),
                ("osc_a_warp2_mode", wt::WARP_DRIVE as f32),
                ("osc_a_warp2", 0.25),
                ("osc_b_on", 0.0),
                ("filter_type", wt::LP12 as f32),
                ("filter_cutoff", 2500.0),
                ("filter_resonance", 0.1),
                ("filter_keytrack", 0.6),
                ("env1_attack", 2.0),
                ("env1_decay", 1800.0),
                ("env1_sustain", 0.3),
                ("env1_release", 250.0),
                ("env2_decay", 500.0),
                ("env2_sustain", 0.2),
                ("lfo1_rate", 5.0),
                ("lfo1_mono", 1.0),
                ("macro1", 0.5),
            ])
            .clear()
            .route(1, env(1), wt::DEST_CUTOFF, 0.3)
            .route(2, wt::SRC_VELOCITY, osc_a + wt::DEST_WARP, 0.3)
            .route(3, lfo(0), wt::DEST_AMP, 0.25)
            .aux(3, mac(0))
            .route(4, mac(1), wt::warp2_dest(0), 0.5)
            .route(5, wt::SRC_VELOCITY, wt::DEST_CUTOFF, 0.15)
            .done(),
        ),
        (
            "Keys: Digital",
            patch(&[
                ("osc_a_table", 14.0),
                ("osc_a_position", 0.3),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 5.0),
                ("osc_b_position", 0.0),
                ("osc_b_octave", 5.0),
                ("osc_b_level", 0.2),
                ("filter_cutoff", 6000.0),
                ("filter_resonance", 0.1),
                ("env1_attack", 1.0),
                ("env1_decay", 2500.0),
                ("env1_sustain", 0.25),
                ("env1_release", 400.0),
                ("env4_decay", 800.0),
                ("env4_sustain", 0.1),
            ])
            .clear()
            .route(1, env(3), osc_a + wt::DEST_POSITION, 0.3)
            .route(2, wt::SRC_VELOCITY, wt::DEST_CUTOFF, 0.2)
            .route(3, mac(0), osc_a + wt::DEST_POSITION, 0.5)
            .route(4, mac(1), wt::DEST_CUTOFF, -0.3)
            .done(),
        ),
        // ------------------------------------------------------------ Drum
        (
            "Drum: Kick",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_a_phase", 0.0),
                ("osc_a_warp_mode", wt::WARP_DRIVE as f32),
                ("osc_a_warp", 0.15),
                ("osc_a_level", 0.9),
                ("osc_b_on", 0.0),
                ("filter_on", 0.0),
                ("env1_attack", 0.5),
                ("env1_decay", 450.0),
                ("env1_sustain", 0.0),
                ("env1_release", 80.0),
                ("env1_decay_curve", -0.4),
                ("env4_attack", 0.5),
                ("env4_decay", 60.0),
                ("env4_sustain", 0.0),
                ("env4_decay_curve", -0.7),
                ("voice_mode", 1.0),
                ("glide", 0.0),
                ("velocity", 0.4),
            ])
            .clear()
            .route(1, env(3), wt::DEST_GLOBAL_PITCH, 1.0)
            .route(2, env(3), osc_a + wt::DEST_PITCH, 0.5)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.5)
            .route(4, mac(1), osc_a + wt::DEST_PITCH, 0.25)
            .done(),
        ),
        (
            "Drum: Snare",
            patch(&[
                ("master_volume", 3.0),
                ("osc_a_position", TRIANGLE),
                ("osc_a_unison", 0.0),
                ("osc_a_random", 0.0),
                ("osc_a_phase", 0.0),
                ("osc_a_semi", 18.0),
                ("osc_a_level", 0.6),
                ("osc_b_on", 0.0),
                ("noise_on", 1.0),
                ("noise_level", 0.7),
                ("filter_osc_a", 0.0),
                ("filter_type", wt::HP12 as f32),
                ("filter_cutoff", 1200.0),
                ("filter_resonance", 0.15),
                ("filter_keytrack", 0.0),
                ("env1_attack", 0.5),
                ("env1_decay", 220.0),
                ("env1_sustain", 0.0),
                ("env1_release", 120.0),
                ("env4_attack", 0.5),
                ("env4_decay", 40.0),
                ("env4_sustain", 0.0),
                ("voice_mode", 1.0),
                ("glide", 0.0),
            ])
            .clear()
            .route(1, env(3), wt::DEST_GLOBAL_PITCH, 0.5)
            .route(2, mac(0), wt::DEST_NOISE_LEVEL, 0.3)
            .route(3, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
        (
            "Drum: Hat",
            patch(&[
                ("master_volume", 1.0),
                ("osc_a_on", 0.0),
                ("osc_b_on", 0.0),
                ("noise_on", 1.0),
                ("noise_type", 3.0),
                ("noise_level", 1.0),
                ("filter_type", wt::HP24 as f32),
                ("filter_cutoff", 7000.0),
                ("filter_resonance", 0.2),
                ("filter_keytrack", 0.0),
                ("env1_attack", 0.5),
                ("env1_decay", 150.0),
                ("env1_sustain", 0.0),
                ("env1_release", 60.0),
                ("env1_decay_curve", -0.3),
            ])
            .clear()
            .route(1, mac(0), wt::DEST_CUTOFF, -0.2)
            .route(2, wt::SRC_VELOCITY, wt::DEST_NOISE_LEVEL, 0.3)
            .done(),
        ),
        // -------------------------------------------------------------- FX
        (
            "FX: Riser",
            patch(&[
                ("master_volume", 6.0),
                ("osc_a_position", SAW),
                ("osc_a_unison", 6.0),
                ("osc_a_detune", 30.0),
                ("osc_a_width", 1.0),
                ("osc_b_on", 0.0),
                ("noise_on", 1.0),
                ("noise_level", 0.3),
                ("filter_type", 0.0),
                ("filter_cutoff", 300.0),
                ("filter_resonance", 0.4),
                ("env1_attack", 20.0),
                ("env1_sustain", 1.0),
                ("env1_release", 1500.0),
                ("lfo7_shape", 2.0),
                ("lfo7_mode", 2.0),
                ("lfo7_sync", 1.0),
                ("lfo7_division", 1.0),
            ])
            .clear()
            .route(1, lfo(6), wt::DEST_CUTOFF, 0.35)
            .route(2, lfo(6), wt::DEST_GLOBAL_PITCH, 0.25)
            .route(3, lfo(6), osc_a + wt::DEST_DETUNE, 0.3)
            .route(4, mac(0), wt::DEST_NOISE_LEVEL, 0.5)
            .done(),
        ),
        (
            "FX: Shepard",
            patch(&[
                ("osc_a_table", 2.0),
                ("osc_a_position", 0.1),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 6.0),
                ("osc_a_morph_mode", wt::MORPH_SHEPARD as f32),
                ("osc_b_on", 0.0),
                ("filter_type", 0.0),
                ("filter_cutoff", 6000.0),
                ("env1_attack", 300.0),
                ("env1_sustain", 1.0),
                ("env1_release", 2000.0),
                ("lfo6_shape", 2.0),
                ("lfo6_mode", 0.0),
                ("lfo6_sync", 0.0),
                ("lfo6_rate", 0.125),
                ("lfo6_mono", 1.0),
            ])
            .clear()
            .route(1, lfo(5), wt::DEST_MORPH_A, 0.5)
            .polarity(1, wt::POLARITY_UNIPOLAR)
            .route(2, mac(0), wt::lfo_rate_dest(5), 0.4)
            .done(),
        ),
        (
            "FX: Chaos Drone",
            patch(&[
                ("osc_a_table", 13.0),
                ("osc_a_position", 0.5),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 20.0),
                ("osc_a_morph_mode", wt::MORPH_RANDOM_AMPLITUDES as f32),
                ("osc_a_morph", 0.3),
                ("osc_b_table", 6.0),
                ("osc_b_octave", 3.0),
                ("osc_b_level", 0.3),
                ("filter_type", wt::COMB_NEG as f32),
                ("filter_cutoff", 400.0),
                ("filter_resonance", 0.6),
                ("filter_mix", 0.6),
                ("env1_attack", 800.0),
                ("env1_sustain", 1.0),
                ("env1_release", 3000.0),
                ("lfo7_shape", 10.0),
                ("lfo7_mono", 1.0),
                ("lfo7_rate", 0.3),
                ("lfo8_rate", 0.2),
            ])
            .clear()
            .route(1, lfo(6), osc_a + wt::DEST_POSITION, 0.4)
            .route(2, lfo(7), wt::DEST_CUTOFF, 0.2)
            .route(3, lfo(7), wt::DEST_MORPH_A, 0.3)
            .route(4, lfo(6), osc_b + wt::DEST_POSITION, 0.5)
            .route(5, mac(0), wt::lfo_rate_dest(6), 0.5)
            .route(6, mac(1), wt::DEST_FILTER_MIX, 0.4)
            .done(),
        ),
        (
            "FX: Laser",
            patch(&[
                ("osc_a_position", SQUARE),
                ("osc_a_unison", 3.0),
                ("osc_a_detune", 20.0),
                ("osc_a_warp_mode", wt::WARP_SYNC as f32),
                ("osc_a_warp", 0.3),
                ("osc_b_on", 0.0),
                ("filter_cutoff", 8000.0),
                ("env1_attack", 0.5),
                ("env1_decay", 500.0),
                ("env1_sustain", 0.0),
                ("env1_release", 400.0),
                ("env4_attack", 0.5),
                ("env4_decay", 350.0),
                ("env4_sustain", 0.0),
                ("env4_decay_curve", -0.8),
            ])
            .clear()
            .route(1, env(3), wt::DEST_GLOBAL_PITCH, 1.0)
            .curve(1, -0.4)
            .route(2, env(3), osc_a + wt::DEST_WARP, 0.6)
            .route(3, mac(0), wt::DEST_GLOBAL_PITCH, 0.5)
            .done(),
        ),
        (
            "FX: Random Bleeps",
            patch(&[
                ("osc_a_position", 0.0),
                ("osc_a_unison", 0.0),
                ("osc_a_warp_mode", wt::WARP_FM as f32),
                ("osc_a_warp", 0.25),
                ("osc_b_position", 0.0),
                ("osc_b_octave", 6.0),
                ("osc_b_level", 0.0),
                ("filter_type", wt::BP12 as f32),
                ("filter_cutoff", 1500.0),
                ("filter_resonance", 0.5),
                ("env1_attack", 1.0),
                ("env1_sustain", 1.0),
                ("env1_release", 300.0),
                ("lfo3_shape", 5.0),
                ("lfo3_sync", 1.0),
                ("lfo3_division", 14.0),
            ])
            .clear()
            .route(1, lfo(2), wt::DEST_GLOBAL_PITCH, 0.5)
            .route(2, lfo(2), wt::DEST_CUTOFF, 0.25)
            .route(3, lfo(2), osc_a + wt::DEST_WARP, 0.3)
            .route(4, mac(0), wt::DEST_GLOBAL_PITCH, 0.4)
            .done(),
        ),
        (
            "FX: Downlifter",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 6.0),
                ("osc_a_detune", 30.0),
                ("osc_a_width", 1.0),
                ("osc_b_on", 0.0),
                ("noise_on", 1.0),
                ("noise_level", 0.35),
                ("filter_cutoff", 8000.0),
                ("filter_resonance", 0.3),
                ("filter_keytrack", 0.0),
                ("env1_attack", 5.0),
                ("env1_sustain", 1.0),
                ("env1_release", 1500.0),
                ("lfo7_shape", 2.0),
                ("lfo7_mode", 2.0),
                ("lfo7_sync", 1.0),
                ("lfo7_division", 3.0),
            ])
            .clear()
            .route(1, lfo(6), wt::DEST_GLOBAL_PITCH, -1.0)
            .polarity(1, wt::POLARITY_UNIPOLAR)
            .route(2, lfo(6), wt::DEST_CUTOFF, -0.5)
            .polarity(2, wt::POLARITY_UNIPOLAR)
            .route(3, lfo(6), osc_a + wt::DEST_DETUNE, 0.3)
            .polarity(3, wt::POLARITY_UNIPOLAR)
            .route(4, mac(0), wt::DEST_NOISE_LEVEL, 0.5)
            .done(),
        ),
        (
            "FX: Alien Talk",
            patch(&[
                ("osc_a_table", 4.0),
                ("osc_a_position", 0.5),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 10.0),
                ("osc_a_morph_mode", wt::MORPH_VOCODE as f32),
                ("osc_a_morph", 0.5),
                ("osc_b_on", 0.0),
                ("filter_type", wt::FORMANT as f32),
                ("filter_cutoff", 700.0),
                ("filter_resonance", 0.6),
                ("filter_mix", 0.9),
                ("env1_attack", 10.0),
                ("env1_sustain", 1.0),
                ("env1_release", 300.0),
                ("lfo3_shape", 5.0),
                ("lfo3_sync", 1.0),
                ("lfo3_division", 14.0),
                ("lfo3_smooth", 0.3),
            ])
            .clear()
            .route(1, lfo(2), wt::DEST_CUTOFF, 0.3)
            .route(2, lfo(2), wt::DEST_MORPH_A, 0.4)
            .route(3, lfo(2), osc_a + wt::DEST_POSITION, 0.3)
            .route(4, mac(0), wt::lfo_rate_dest(2), 0.3)
            .done(),
        ),
        // ------------------------------------------------------------- Seq
        (
            "Seq: Trance Gate",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 6.0),
                ("osc_a_detune", 20.0),
                ("osc_a_width", 1.0),
                ("osc_b_position", SAW),
                ("osc_b_octave", 5.0),
                ("osc_b_unison", 4.0),
                ("osc_b_level", 0.3),
                ("filter_cutoff", 3500.0),
                ("filter_resonance", 0.2),
                ("env1_attack", 5.0),
                ("env1_sustain", 1.0),
                ("env1_release", 600.0),
                ("lfo5_sync", 1.0),
                ("lfo5_division", 3.0),
                ("lfo5_smooth", 0.15),
                ("lfo5_mode", 0.0),
            ])
            .clear()
            .steps(5, &[0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0])
            .route(1, lfo(4), wt::DEST_AMP, -1.0)
            .polarity(1, wt::POLARITY_UNIPOLAR)
            .aux(1, mac(0))
            .route(2, mac(1), wt::DEST_CUTOFF, 0.3)
            .put("macro1".into(), 1.0)
            .done(),
        ),
        (
            "Seq: Stepper",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 8.0),
                ("osc_b_on", 0.0),
                ("filter_type", 1.0),
                ("filter_cutoff", 400.0),
                ("filter_resonance", 0.55),
                ("filter_drive", 0.3),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 200.0),
                ("lfo3_shape", 7.0),
                ("lfo3_sync", 1.0),
                ("lfo3_division", 2.0),
                ("lfo3_glide", 0.15),
            ])
            .clear()
            .points(
                3,
                &[
                    0.9, 0.2, 0.5, 0.2, 0.7, 0.2, 0.4, 1.0, 0.3, 0.2, 0.8, 0.2, 0.5, 0.6, 0.2, 0.4,
                    0.9, 0.2, 0.5, 0.2, 0.7, 0.2, 0.4, 1.0, 0.3, 0.2, 0.8, 0.2, 0.6, 0.3, 0.9, 0.2,
                ],
            )
            .route(1, lfo(2), wt::DEST_CUTOFF, 0.35)
            .polarity(1, wt::POLARITY_UNIPOLAR)
            .route(2, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(3, mac(1), wt::DEST_RESONANCE, 0.3)
            .done(),
        ),
        (
            "Seq: Pulse Arp",
            patch(&[
                ("master_volume", 5.0),
                ("osc_a_table", 1.0),
                ("osc_a_position", 0.4),
                ("osc_a_unison", 1.0),
                ("osc_a_detune", 6.0),
                ("osc_b_on", 0.0),
                ("filter_type", wt::DIRTY as f32),
                ("filter_cutoff", 1800.0),
                ("filter_resonance", 0.3),
                ("env1_attack", 1.0),
                ("env1_decay", 180.0),
                ("env1_sustain", 0.0),
                ("env1_release", 150.0),
                ("env2_decay", 120.0),
                ("env2_sustain", 0.0),
                ("mod1_amount", 0.3),
                ("mod2_source", 0.0),
                ("lfo4_rate", 0.15),
            ])
            .route(6, lfo(3), osc_a + wt::DEST_POSITION, 0.3)
            .route(7, mac(0), wt::DEST_CUTOFF, 0.3)
            .route(8, mac(1), osc_a + wt::DEST_POSITION, 0.5)
            .done(),
        ),
        (
            "Seq: Remap Groove",
            patch(&[
                ("osc_a_position", SAW),
                ("osc_a_unison", 2.0),
                ("osc_a_detune", 8.0),
                ("osc_a_warp_mode", wt::WARP_REMAP as f32),
                ("osc_a_warp", 0.0),
                ("osc_b_on", 0.0),
                ("sub_on", 1.0),
                ("sub_level", 0.4),
                ("filter_cutoff", 2200.0),
                ("filter_resonance", 0.3),
                ("filter_keytrack", 0.4),
                ("env1_attack", 2.0),
                ("env1_sustain", 1.0),
                ("env1_release", 150.0),
                ("lfo5_sync", 1.0),
                ("lfo5_division", 5.0),
                ("lfo5_mode", 1.0),
            ])
            .clear()
            .remap("a", &[(0.0, 0.0, 0.6), (0.5, 0.5, -0.6), (1.0, 1.0, 0.0)])
            .steps(5, &[1.0, 0.2, 0.6, 0.0, 0.9, 0.3, 0.7, 0.1])
            .route(1, lfo(4), osc_a + wt::DEST_WARP, 0.8)
            .polarity(1, wt::POLARITY_UNIPOLAR)
            .route(2, lfo(4), wt::DEST_CUTOFF, 0.15)
            .polarity(2, wt::POLARITY_UNIPOLAR)
            .route(3, mac(0), osc_a + wt::DEST_WARP, 0.3)
            .route(4, mac(1), wt::DEST_CUTOFF, 0.3)
            .done(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every factory preset names only settings its device has, with
    /// values in their ranges.
    #[test]
    fn factory_presets_fit_their_devices() {
        for device in effects::builtins() {
            for (name, values) in factory(device.id) {
                for (id, value) in &values {
                    let param = device
                        .params
                        .iter()
                        .chain(device.settings)
                        .find(|p| p.id == id)
                        .unwrap_or_else(|| panic!("{} {name}: no {id}", device.id));
                    assert_eq!(param.clamp(*value), *value, "{} {name}: {id}", device.id);
                }
            }
        }
        for instrument in instruments::builtins() {
            for (name, values) in factory(instrument.id) {
                for (id, value) in &values {
                    let param = instrument
                        .params
                        .iter()
                        .find(|p| p.id == id)
                        .unwrap_or_else(|| panic!("{} {name}: no {id}", instrument.id));
                    assert_eq!(
                        param.clamp(*value),
                        *value,
                        "{} {name}: {id}",
                        instrument.id
                    );
                }
            }
        }
    }

    /// Every instrument preset plays a chord audibly, without blowing up.
    #[test]
    fn instrument_presets_sound() {
        use midi::{NoteEvent, NoteEventKind, Pitch, Velocity};
        const BLOCK: usize = 256;
        for instrument in instruments::builtins() {
            for (name, values) in factory(instrument.id) {
                let mut synth = (instrument.create)();
                synth.prepare(48_000.0, BLOCK);
                for (id, value) in &values {
                    let index = instrument.params.iter().position(|p| p.id == id).unwrap();
                    synth.set_param(index, *value);
                }
                synth.reset();
                let chord: Vec<NoteEvent> = [36, 43, 48, 52, 55]
                    .into_iter()
                    .map(|pitch| NoteEvent {
                        frame: 0,
                        kind: NoteEventKind::On {
                            pitch: Pitch::new(pitch).unwrap(),
                            velocity: Velocity::from_midi1(100),
                        },
                    })
                    .collect();
                let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
                let mut peak = 0f32;
                // Long enough for the slowest attack (the pad's).
                for block in 0..(2 * 48_000 / BLOCK) {
                    let events = if block == 0 { &chord[..] } else { &[] };
                    synth.process(events, &mut left, &mut right);
                    for sample in left.iter().chain(&right) {
                        assert!(sample.is_finite(), "{} {name}", instrument.id);
                        peak = peak.max(sample.abs());
                    }
                }
                assert!(
                    (0.05..3.0).contains(&peak),
                    "{} {name}: peak {peak}",
                    instrument.id
                );
            }
        }
    }

    #[test]
    fn save_list_and_load() {
        let dir = std::env::temp_dir().join(format!("canopy-presets-{}", std::process::id()));
        let params = BTreeMap::from([("decay".to_owned(), 4.5), ("mix".to_owned(), 30.0)]);
        let none = project::Blob::default();
        save(&dir, "canopy.reverb", "Big hall", &params, &none).unwrap();
        save(&dir, "canopy.reverb", "a/room", &params, &none).unwrap();
        let names: Vec<String> = list(&dir, "canopy.reverb")
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["a_room", "Big hall"]);
        let path = &list(&dir, "canopy.reverb")[1].path;
        assert_eq!(
            load(path, "canopy.reverb").unwrap(),
            (params.clone(), none.clone())
        );
        assert!(load(path, "canopy.delay").is_err());
        assert!(list(&dir, "canopy.delay").is_empty());
        assert!(save(&dir, "canopy.reverb", " / ", &params, &none).is_ok());
        assert!(save(&dir, "canopy.reverb", "  ", &params, &none).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_plugins_preset_keeps_its_state() {
        let dir = std::env::temp_dir().join(format!("canopy-plugin-state-{}", std::process::id()));
        let params = BTreeMap::from([("0".to_owned(), -6.0)]);
        let state = project::Blob::from(vec![1, 2, 3]);
        let path = save(&dir, "org.example.gain", "Quiet", &params, &state).unwrap();
        assert_eq!(load(&path, "org.example.gain").unwrap(), (params, state));
        let _ = std::fs::remove_dir_all(dir);
    }
}
