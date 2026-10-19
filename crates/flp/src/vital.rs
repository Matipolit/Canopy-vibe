//! Vital presets, which Vital saves inside its plugin state as JSON: its
//! settings onto canopy's Wavetable synth, its wavetables rendered to
//! single-cycle frames for the synth's user tables, and its effects as
//! canopy devices.
//!
//! Units follow Vital's parameter definitions: times are stored as the
//! fourth root of seconds, filter cutoffs and the like as MIDI notes, LFO
//! and effect rates as powers of two in Hz, and a modulation's amount as a
//! share of its destination's whole range.

use serde_json::{Map, Value};

/// Samples in one of Vital's wavetable frames.
pub const FRAME: usize = 2048;
/// Frames in a Vital wavetable.
pub const FRAMES: usize = 256;

/// A preset, as Vital saved it.
#[derive(Debug, Clone)]
pub struct Preset {
    pub name: String,
    settings: Map<String, Value>,
}

impl Preset {
    /// The preset in a Vital plugin state: the JSON object holding
    /// `"settings"`, wherever it starts.
    pub fn from_state(state: &[u8]) -> Option<Preset> {
        let mut from = 0;
        // A handful of tries: the JSON follows the wrapper's binary header.
        for _ in 0..64 {
            let start = from + find(&state[from..], b"{\"")?;
            let mut values =
                serde_json::Deserializer::from_slice(&state[start..]).into_iter::<Value>();
            if let Some(Ok(Value::Object(mut object))) = values.next()
                && let Some(Value::Object(settings)) = object.remove("settings")
            {
                let name = object
                    .get("preset_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                return Some(Preset { name, settings });
            }
            from = start + 2;
        }
        None
    }

    /// A numeric setting.
    pub fn num(&self, key: &str) -> Option<f32> {
        self.settings.get(key)?.as_f64().map(|v| v as f32)
    }

    fn on(&self, key: &str) -> bool {
        self.num(key).is_some_and(|v| v >= 0.5)
    }

    /// Oscillator `osc`'s (0 to 2) wavetable: its name and frames.
    pub fn wavetable(&self, osc: usize) -> Option<Wavetable> {
        let table = self.settings.get("wavetables")?.as_array()?.get(osc)?;
        render_wavetable(table)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A rendered wavetable: single-cycle frames of [`FRAME`] samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Wavetable {
    pub name: String,
    pub frames: Vec<Vec<f32>>,
}

/// Standard base64, as Vital writes its sample data.
fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = text
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= value(c)? << (18 - 6 * i);
        }
        let n = chunk.len() * 6 / 8;
        out.extend(&acc.to_be_bytes()[1..1 + n]);
    }
    Some(out)
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// Renders a Vital wavetable definition: each group's components summed,
/// frame by frame, with DC removed and the whole table normalized.
fn render_wavetable(table: &Value) -> Option<Wavetable> {
    let name = table
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Wavetable")
        .to_owned();
    let mut frames = vec![vec![0.0f32; FRAME]; FRAMES];
    let mut any = false;
    for group in table.get("groups")?.as_array()? {
        for component in group.get("components")?.as_array()? {
            let Some(rendered) = render_component(component) else {
                continue;
            };
            any = true;
            for (frame, add) in frames.iter_mut().zip(&rendered) {
                for (s, a) in frame.iter_mut().zip(add) {
                    *s += a;
                }
            }
        }
    }
    if !any {
        return None;
    }
    for frame in &mut frames {
        let mean = frame.iter().sum::<f32>() / FRAME as f32;
        frame.iter_mut().for_each(|s| *s -= mean);
    }
    let peak = frames.iter().flatten().fold(0.0f32, |p, s| p.max(s.abs()));
    if peak > 0.0 {
        frames.iter_mut().flatten().for_each(|s| *s /= peak);
    }
    // A table that never changes is one frame.
    if frames.windows(2).all(|w| w[0] == w[1]) {
        frames.truncate(1);
    }
    Some(Wavetable { name, frames })
}

/// A component's keyframes, sorted by where they sit in the table.
fn keyframes(component: &Value) -> Vec<&Value> {
    let mut frames: Vec<&Value> = component
        .get("keyframes")
        .and_then(Value::as_array)
        .map(|k| k.iter().collect())
        .unwrap_or_default();
    frames.sort_by_key(|k| k.get("position").and_then(Value::as_i64).unwrap_or(0));
    frames
}

/// Which keyframes frame `frame` lies between, and how far: `(a, b, t)`.
/// Without interpolation the frame is keyframe `a`'s.
fn between(positions: &[i64], frame: usize, interpolate: bool) -> (usize, usize, f32) {
    let frame = frame as i64;
    let a = positions.iter().rposition(|p| *p <= frame).unwrap_or(0);
    let b = (a + 1).min(positions.len() - 1);
    if !interpolate || a == b || positions[b] <= positions[a] {
        return (a, a, 0.0);
    }
    let t = (frame - positions[a]) as f32 / (positions[b] - positions[a]) as f32;
    (a, b, t.clamp(0.0, 1.0))
}

fn render_component(component: &Value) -> Option<Vec<Vec<f32>>> {
    let kind = component.get("type")?.as_str()?;
    let keys = keyframes(component);
    if keys.is_empty() {
        return None;
    }
    let positions: Vec<i64> = keys
        .iter()
        .map(|k| k.get("position").and_then(Value::as_i64).unwrap_or(0))
        .collect();
    // 0 holds each keyframe; time and spectral interpolation both morph
    // (spectral as a crossfade, which is close for similar frames).
    let interpolate = component
        .get("interpolation_style")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        != 0;
    match kind {
        "Wave Source" => {
            let waves: Vec<Vec<f32>> = keys
                .iter()
                .map(|k| {
                    let data = k.get("wave_data").and_then(Value::as_str).unwrap_or("");
                    let mut wave = base64(data).map(|b| floats(&b)).unwrap_or_default();
                    wave.resize(FRAME, 0.0);
                    wave
                })
                .collect();
            Some(crossfaded(&waves, &positions, interpolate))
        }
        "Line Source" => {
            let waves: Vec<Vec<f32>> = keys
                .iter()
                .map(|k| k.get("line").map_or_else(|| vec![0.0; FRAME], render_line))
                .collect();
            Some(crossfaded(&waves, &positions, interpolate))
        }
        "Audio File Source" => render_audio(component, &keys, &positions, interpolate),
        _ => None,
    }
}

/// Every frame from keyframe waves, crossfaded between them.
fn crossfaded(waves: &[Vec<f32>], positions: &[i64], interpolate: bool) -> Vec<Vec<f32>> {
    (0..FRAMES)
        .map(|frame| {
            let (a, b, t) = between(positions, frame, interpolate);
            waves[a]
                .iter()
                .zip(&waves[b])
                .map(|(x, y)| x + (y - x) * t)
                .collect()
        })
        .collect()
}

/// A Vital line shape (an LFO's, or a line source's): points `(x, y)` with
/// `y` 0 at the top, and a power bending each segment.
pub fn line_value(line: &Value, x: f32) -> f32 {
    let points: Vec<f32> = line
        .get("points")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(Value::as_f64)
                .map(|v| v as f32)
                .collect()
        })
        .unwrap_or_default();
    let powers: Vec<f32> = line
        .get("powers")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(Value::as_f64)
                .map(|v| v as f32)
                .collect()
        })
        .unwrap_or_default();
    let pairs: Vec<(f32, f32)> = points
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| (p[0], p[1]))
        .collect();
    if pairs.is_empty() {
        return 0.5;
    }
    let level = |y: f32| 1.0 - y;
    let after = pairs.iter().position(|p| p.0 > x).unwrap_or(pairs.len());
    if after == 0 {
        return level(pairs[0].1);
    }
    if after == pairs.len() {
        return level(pairs[after - 1].1);
    }
    let (a, b) = (pairs[after - 1], pairs[after]);
    let span = (b.0 - a.0).max(1e-9);
    let t = power_scale(
        (x - a.0) / span,
        powers.get(after - 1).copied().unwrap_or(0.0),
    );
    level(a.1 + (b.1 - a.1) * t)
}

/// Vital's segment bend: `t` pushed towards one end by `power`.
fn power_scale(t: f32, power: f32) -> f32 {
    if power.abs() < 0.001 {
        return t;
    }
    ((power * t).exp() - 1.0) / (power.exp() - 1.0)
}

/// A line as one cycle of audio, `-1..=1`.
fn render_line(line: &Value) -> Vec<f32> {
    (0..FRAME)
        .map(|i| 2.0 * line_value(line, i as f32 / FRAME as f32) - 1.0)
        .collect()
}

/// An audio file source: a window moving through a sample, each frame a
/// window's worth resampled to one cycle, its ends faded so it loops.
fn render_audio(
    component: &Value,
    keys: &[&Value],
    positions: &[i64],
    interpolate: bool,
) -> Option<Vec<Vec<f32>>> {
    let data = base64(component.get("audio_file")?.as_str()?)?;
    let audio: Vec<f32> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0)
        .collect();
    if audio.is_empty() {
        return None;
    }
    let field = |key: &Value, name: &str, default: f32| {
        key.get(name)
            .and_then(Value::as_f64)
            .map_or(default, |v| v as f32)
    };
    let default_window = component
        .get("window_size")
        .and_then(Value::as_f64)
        .map_or(FRAME as f32, |v| v as f32);
    Some(
        (0..FRAMES)
            .map(|frame| {
                let (a, b, t) = between(positions, frame, interpolate);
                let lerp = |name: &str, default: f32| {
                    let x = field(keys[a], name, default);
                    x + (field(keys[b], name, default) - x) * t
                };
                let start = lerp("start_position", 0.0).max(0.0);
                let size = lerp("window_size", default_window).max(2.0);
                let fade = lerp("window_fade", 1.0).clamp(0.0, 1.0);
                (0..FRAME)
                    .map(|i| {
                        let u = i as f32 / FRAME as f32;
                        let at = start + u * size;
                        let index = at as usize;
                        let frac = at - index as f32;
                        let s = |n: usize| audio.get(n).copied().unwrap_or(0.0);
                        let sample = s(index) + (s(index + 1) - s(index)) * frac;
                        // A raised-cosine taper over `fade` of each half.
                        let edge = u.min(1.0 - u) * 2.0;
                        let window = if fade > 0.0 && edge < fade {
                            0.5 - 0.5 * (std::f32::consts::PI * edge / fade).cos()
                        } else {
                            1.0
                        };
                        sample * window
                    })
                    .collect()
            })
            .collect(),
    )
}

/// A wavetable as a 32-bit float mono WAV, its frames one after another,
/// with Serum's `clm ` chunk saying how long a frame is.
pub fn wav_bytes(table: &Wavetable) -> Vec<u8> {
    let samples: Vec<f32> = table.frames.iter().flatten().copied().collect();
    let clm = format!("<!>{FRAME} 10000000 wavetable (canopy)");
    let mut clm_bytes = clm.into_bytes();
    if clm_bytes.len() % 2 == 1 {
        clm_bytes.push(0);
    }
    let data_len = samples.len() * 4;
    let mut out = Vec::with_capacity(data_len + 80);
    out.extend(b"RIFF");
    let riff_len = 4 + (8 + 16) + (8 + clm_bytes.len()) + (8 + data_len);
    out.extend((riff_len as u32).to_le_bytes());
    out.extend(b"WAVE");
    out.extend(b"fmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(3u16.to_le_bytes()); // IEEE float
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(44_100u32.to_le_bytes());
    out.extend((44_100u32 * 4).to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend(b"clm ");
    out.extend((clm_bytes.len() as u32).to_le_bytes());
    out.extend(&clm_bytes);
    out.extend(b"data");
    out.extend((data_len as u32).to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    out
}

/// A MIDI note number as Hz.
fn note_hz(note: f32) -> f32 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// Vital's stored times (the fourth root of seconds) in milliseconds.
fn quartic_ms(value: f32) -> f32 {
    value.max(0.0).powi(4) * 1000.0
}

/// What a preset becomes on canopy's Wavetable synth.
#[derive(Debug, Clone, Default)]
pub struct Converted {
    /// Wavetable parameters, by id.
    pub params: Vec<(String, f32)>,
    /// Each oscillator's wavetable, for the ones that are on.
    pub tables: [Option<Wavetable>; 3],
    /// Vital's own effects that are on, in order, as canopy effects.
    pub effects: Vec<(&'static str, Vec<(String, f32)>)>,
    /// What the synth couldn't take, for the import report.
    pub dropped: Vec<String>,
}

/// The ids of the canopy synth's parameters, to skip what it doesn't have.
fn has_param(id: &str) -> bool {
    instruments::find_builtin(crate::plugins::WAVETABLE)
        .is_some_and(|d| d.params.iter().any(|p| p.id == id))
}

/// A choice parameter's value for `label`, if it has one.
fn label_index(id: &str, label: &str) -> Option<f32> {
    let descriptor = instruments::find_builtin(crate::plugins::WAVETABLE)?;
    let param = descriptor.params.iter().find(|p| p.id == id)?;
    match param.kind {
        instruments::ParameterKind::Enum { labels, .. } => {
            labels.iter().position(|l| *l == label).map(|i| i as f32)
        }
        _ => None,
    }
}

/// Vital's tempo-synced lengths, as its menus list them.
const VITAL_TEMPOS: [&str; 12] = [
    "32/1", "16/1", "8/1", "4/1", "2/1", "1/1", "1/2", "1/4", "1/8", "1/16", "1/32", "1/64",
];

pub fn convert(preset: &Preset) -> Converted {
    let mut out = Converted::default();
    let set = |out: &mut Converted, id: String, value: f32| {
        if has_param(&id) {
            out.params.push((id, value));
        }
    };
    let osc_ids = ["a", "b", "c"];

    // Oscillators.
    let mut any_filter2 = false;
    for (k, x) in osc_ids.iter().enumerate() {
        let v = |name: &str| preset.num(&format!("osc_{}_{name}", k + 1));
        let on = v("on").unwrap_or(0.0) >= 0.5;
        if !has_param(&format!("osc_{x}_on")) {
            if on {
                out.dropped.push(format!("oscillator {}", k + 1));
            }
            continue;
        }
        set(&mut out, format!("osc_{x}_on"), if on { 1.0 } else { 0.0 });
        if !on {
            continue;
        }
        if let Some(level) = v("level") {
            set(&mut out, format!("osc_{x}_level"), level);
        }
        if let Some(pan) = v("pan") {
            set(&mut out, format!("osc_{x}_pan"), pan);
        }
        let transpose = v("transpose").unwrap_or(0.0).round();
        let octaves = (transpose / 12.0).round().clamp(-4.0, 4.0);
        let semis = (transpose - 12.0 * octaves).clamp(-12.0, 12.0);
        set(&mut out, format!("osc_{x}_octave"), octaves + 4.0);
        set(&mut out, format!("osc_{x}_semi"), semis + 12.0);
        set(
            &mut out,
            format!("osc_{x}_fine"),
            v("tune").unwrap_or(0.0) * 100.0,
        );
        let voices = v("unison_voices").unwrap_or(1.0).round().clamp(1.0, 16.0);
        set(&mut out, format!("osc_{x}_unison"), voices - 1.0);
        if let Some(detune) = v("unison_detune") {
            set(
                &mut out,
                format!("osc_{x}_detune"),
                (detune * detune).min(100.0),
            );
        }
        if let Some(blend) = v("unison_blend") {
            set(&mut out, format!("osc_{x}_blend"), blend);
        }
        if let Some(spread) = v("stereo_spread") {
            set(&mut out, format!("osc_{x}_width"), spread);
        }
        if let Some(phase) = v("phase") {
            set(&mut out, format!("osc_{x}_phase"), phase);
        }
        if let Some(random) = v("random_phase") {
            set(&mut out, format!("osc_{x}_random"), random);
        }
        // Its wavetable: the synth's user table, if it takes one.
        let frame = v("wave_frame").unwrap_or(0.0);
        match (
            preset.wavetable(k),
            label_index(&format!("osc_{x}_table"), "User"),
        ) {
            (Some(table), Some(user)) => {
                let frames = table.frames.len();
                let position = if frames > 1 {
                    (frame / (FRAMES - 1) as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                set(&mut out, format!("osc_{x}_table"), user);
                set(&mut out, format!("osc_{x}_position"), position);
                out.tables[k] = Some(table);
            }
            (table, _) => {
                // Basic Shapes runs sine, triangle, saw, square; Vital's
                // init table is a saw.
                set(&mut out, format!("osc_{x}_table"), 0.0);
                set(&mut out, format!("osc_{x}_position"), 2.0 / 3.0);
                if let Some(table) = table {
                    out.dropped.push(format!("wavetable \"{}\"", table.name));
                }
            }
        }
        // The oscillator's distortion, as a warp.
        let warp = match v("distortion_type").unwrap_or(0.0).round() as i32 {
            1 => Some("Sync"),
            2 => Some("Sync Win"),
            3 => Some("Quantize"),
            4 => Some("Bend+"),
            5 => Some("Asym+"),
            6 => Some("PWM"),
            7 | 8 => Some(if k == 1 { "FM from A" } else { "FM from B" }),
            10 | 11 => Some(if k == 1 { "RM from A" } else { "RM from B" }),
            _ => None,
        };
        if let Some(index) = warp.and_then(|w| label_index(&format!("osc_{x}_warp_mode"), w)) {
            set(&mut out, format!("osc_{x}_warp_mode"), index);
            set(
                &mut out,
                format!("osc_{x}_warp"),
                v("distortion_amount").unwrap_or(0.5),
            );
        }
        // Where it goes: filter 1, filter 2, both, or past them.
        let destination = v("destination").unwrap_or(0.0).round() as i32;
        let (into, target) = match destination {
            0 | 2 => (1.0, 0.0),
            1 => (1.0, 1.0),
            _ => (0.0, 0.0),
        };
        any_filter2 |= destination == 1;
        set(&mut out, format!("filter_osc_{x}"), into);
        set(&mut out, format!("filter_osc_{x}_target"), target);
    }
    if any_filter2 {
        set(&mut out, "filter_routing".to_owned(), 1.0);
    }

    // Filters.
    for (k, prefix) in ["filter", "filter2"].iter().enumerate() {
        let v = |name: &str| preset.num(&format!("filter_{}_{name}", k + 1));
        let on = v("on").unwrap_or(0.0) >= 0.5;
        set(&mut out, format!("{prefix}_on"), if on { 1.0 } else { 0.0 });
        if !on {
            continue;
        }
        let model = v("model").unwrap_or(0.0).round() as i32;
        let style = v("style").unwrap_or(0.0).round() as i32;
        let blend = v("blend").unwrap_or(0.0);
        let label = match model {
            2 => "Ladder",
            5 => "Formant",
            6 => {
                if style % 2 == 0 {
                    "Comb +"
                } else {
                    "Comb -"
                }
            }
            _ if style == 2 || style == 3 => "Notch",
            _ if blend < 0.5 => {
                if style == 1 {
                    "LP 24"
                } else {
                    "LP 12"
                }
            }
            _ if blend > 1.5 => {
                if style == 1 {
                    "HP 24"
                } else {
                    "HP 12"
                }
            }
            _ => "BP 12",
        };
        if let Some(index) = label_index(&format!("{prefix}_type"), label) {
            set(&mut out, format!("{prefix}_type"), index);
        }
        if let Some(cutoff) = v("cutoff") {
            set(&mut out, format!("{prefix}_cutoff"), note_hz(cutoff));
        }
        if let Some(resonance) = v("resonance") {
            set(&mut out, format!("{prefix}_resonance"), resonance);
        }
        if let Some(drive) = v("drive") {
            set(
                &mut out,
                format!("{prefix}_drive"),
                (drive / 20.0).clamp(0.0, 1.0),
            );
        }
        if let Some(keytrack) = v("keytrack") {
            set(
                &mut out,
                format!("{prefix}_keytrack"),
                keytrack.clamp(0.0, 1.0),
            );
        }
        if let Some(mix) = v("mix") {
            set(&mut out, format!("{prefix}_mix"), mix);
        }
    }

    // Envelopes: Vital's first three onto canopy's three.
    for k in 1..=3 {
        let v = |name: &str| preset.num(&format!("env_{k}_{name}"));
        for (stage, id) in [
            ("attack", "attack"),
            ("decay", "decay"),
            ("release", "release"),
        ] {
            if let Some(value) = v(stage) {
                set(&mut out, format!("env{k}_{id}"), quartic_ms(value));
            }
            if let Some(power) = v(&format!("{stage}_power")) {
                set(
                    &mut out,
                    format!("env{k}_{id}_curve"),
                    (power * 0.3).clamp(-1.0, 1.0),
                );
            }
        }
        if let Some(sustain) = v("sustain") {
            set(&mut out, format!("env{k}_sustain"), sustain);
        }
        for stage in ["delay", "hold"] {
            if let Some(value) = v(stage) {
                set(&mut out, format!("env{k}_{stage}"), quartic_ms(value));
            }
        }
    }

    // LFOs: the first four, with their shapes.
    let shapes = preset.settings.get("lfos").and_then(Value::as_array);
    for k in 1..=4 {
        let v = |name: &str| preset.num(&format!("lfo_{k}_{name}"));
        let shape = shapes.and_then(|s| s.get(k - 1));
        if let Some(shape) = shape {
            let name = shape.get("name").and_then(Value::as_str).unwrap_or("");
            let builtin = match name {
                "Sin" | "Sine" => Some("Sine"),
                "Triangle" => Some("Triangle"),
                "Saw Up" => Some("Saw Up"),
                "Saw Down" | "Saw" => Some("Saw Down"),
                "Square" => Some("Square"),
                _ => None,
            };
            match builtin.and_then(|b| label_index(&format!("lfo{k}_shape"), b)) {
                Some(index) if name != "Triangle" => set(&mut out, format!("lfo{k}_shape"), index),
                _ => {
                    // Vital's triangle starts at the bottom, so it's drawn
                    // too; anything else is a custom shape.
                    if let Some(custom) = label_index(&format!("lfo{k}_shape"), "Custom") {
                        set(&mut out, format!("lfo{k}_shape"), custom);
                        for i in 0..32 {
                            let x = i as f32 / 32.0;
                            set(&mut out, format!("lfo{k}_p{i}"), line_value(shape, x));
                        }
                        set(&mut out, format!("lfo{k}_glide"), 1.0);
                    }
                }
            }
        }
        let sync = v("sync").unwrap_or(1.0).round() as i32;
        if sync == 0 || sync == 4 {
            set(&mut out, format!("lfo{k}_sync"), 0.0);
            if let Some(frequency) = v("frequency") {
                set(&mut out, format!("lfo{k}_rate"), frequency.exp2());
            }
        } else {
            set(&mut out, format!("lfo{k}_sync"), 1.0);
            let tempo = v("tempo").unwrap_or(7.0).round().clamp(0.0, 11.0) as usize;
            let mut division = VITAL_TEMPOS[tempo].to_owned();
            match sync {
                2 => division.push('D'),
                3 => division.push('T'),
                _ => {}
            }
            let index = label_index(&format!("lfo{k}_division"), &division)
                .or_else(|| label_index(&format!("lfo{k}_division"), VITAL_TEMPOS[tempo]));
            if let Some(index) = index {
                set(&mut out, format!("lfo{k}_division"), index);
            }
        }
        let mode = match v("sync_type").unwrap_or(0.0).round() as i32 {
            0 => "Retrig",
            2 | 3 => "Env",
            _ => "Free",
        };
        if let Some(index) = label_index(&format!("lfo{k}_mode"), mode) {
            set(&mut out, format!("lfo{k}_mode"), index);
        }
        if let Some(phase) = v("phase") {
            set(&mut out, format!("lfo{k}_phase"), phase);
        }
        if let Some(fade) = v("fade_time") {
            set(&mut out, format!("lfo{k}_rise"), fade);
        }
    }

    // The modulation matrix.
    modulations(preset, &mut out);

    // Macros and the voice.
    for k in 1..=4 {
        if let Some(value) = preset.num(&format!("macro_control_{k}")) {
            set(&mut out, format!("macro{k}"), value);
        }
    }
    let polyphony = preset
        .num("polyphony")
        .unwrap_or(8.0)
        .round()
        .clamp(1.0, 32.0);
    set(&mut out, "polyphony".to_owned(), polyphony - 1.0);
    let mode = if polyphony > 1.0 {
        0.0
    } else if preset.on("legato") {
        2.0
    } else {
        1.0
    };
    set(&mut out, "voice_mode".to_owned(), mode);
    if let Some(time) = preset.num("portamento_time").filter(|t| *t > -9.5) {
        set(
            &mut out,
            "glide".to_owned(),
            (time.exp2() * 1000.0).min(2000.0),
        );
    }
    if let Some(range) = preset.num("pitch_bend_range") {
        set(&mut out, "bend_range".to_owned(), range);
    }
    if let Some(velocity) = preset.num("velocity_track") {
        set(&mut out, "velocity".to_owned(), velocity);
    }
    if let Some(volume) = preset.num("volume").filter(|v| *v > 0.0) {
        // Relative to Vital's default level, which canopy's default matches.
        let db = -3.0 + 40.0 * (volume / 5473.04).log10();
        set(&mut out, "master_volume".to_owned(), db.clamp(-60.0, 6.0));
    }

    effects(preset, &mut out);
    out
}

/// Vital's modulations onto the canopy synth's matrix slots, where both
/// ends exist there.
fn modulations(preset: &Preset, out: &mut Converted) {
    let Some(list) = preset.settings.get("modulations").and_then(Value::as_array) else {
        return;
    };
    let mut slot = 0;
    let mut dropped = 0;
    for (i, modulation) in list.iter().enumerate() {
        let source = modulation
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("");
        let destination = modulation
            .get("destination")
            .and_then(Value::as_str)
            .unwrap_or("");
        if source.is_empty() || destination.is_empty() {
            continue;
        }
        let n = i + 1;
        if preset.on(&format!("modulation_{n}_bypass")) {
            continue;
        }
        let amount = preset.num(&format!("modulation_{n}_amount")).unwrap_or(0.0);
        let mapped = mod_source(source).zip(mod_destination(destination));
        let Some((source, (destination, scale))) = mapped else {
            dropped += 1;
            continue;
        };
        if slot >= 16 {
            dropped += 1;
            continue;
        }
        slot += 1;
        let (Some(source), Some(dest)) = (
            label_index(&format!("mod{slot}_source"), source),
            label_index(&format!("mod{slot}_dest"), &destination),
        ) else {
            dropped += 1;
            slot -= 1;
            continue;
        };
        let bipolar = preset.on(&format!("modulation_{n}_bipolar"));
        let curve = preset.num(&format!("modulation_{n}_power")).unwrap_or(0.0);
        out.params.extend([
            (format!("mod{slot}_source"), source),
            (format!("mod{slot}_dest"), dest),
            (
                format!("mod{slot}_amount"),
                (amount * scale).clamp(-1.0, 1.0),
            ),
            (format!("mod{slot}_aux"), 0.0),
            (format!("mod{slot}_curve"), (curve / 10.0).clamp(-1.0, 1.0)),
            (
                format!("mod{slot}_polarity"),
                if bipolar { 2.0 } else { 0.0 },
            ),
        ]);
    }
    // The default patch's slots past the ones used are cleared.
    for empty in slot + 1..=16 {
        out.params.push((format!("mod{empty}_source"), 0.0));
        out.params.push((format!("mod{empty}_dest"), 0.0));
    }
    if dropped > 0 {
        out.dropped.push(format!("{dropped} modulations"));
    }
}

fn mod_source(source: &str) -> Option<&'static str> {
    Some(match source {
        "env_1" => "Env 1",
        "env_2" => "Env 2",
        "env_3" => "Env 3",
        "lfo_1" => "LFO 1",
        "lfo_2" => "LFO 2",
        "lfo_3" => "LFO 3",
        "lfo_4" => "LFO 4",
        "velocity" => "Velocity",
        "note" => "Note",
        "mod_wheel" => "Mod Wheel",
        "pitch_wheel" => "Pitch Bend",
        "macro_control_1" => "Macro 1",
        "macro_control_2" => "Macro 2",
        "macro_control_3" => "Macro 3",
        "macro_control_4" => "Macro 4",
        "random" | "random_1" | "random_2" => "Random",
        _ => return None,
    })
}

/// A Vital destination's canopy label, and what a Vital amount (a share of
/// the parameter's range) is worth in canopy's (a share of its scale).
fn mod_destination(destination: &str) -> Option<(String, f32)> {
    if let Some(rest) = destination.strip_prefix("osc_") {
        let (n, param) = rest.split_once('_')?;
        let osc = match n {
            "1" => "A",
            "2" => "B",
            "3" => "C",
            _ => return None,
        };
        let (name, scale) = match param {
            "wave_frame" => ("Position", 1.0),
            "distortion_amount" => ("Warp", 1.0),
            "level" => ("Level", 1.0),
            "pan" => ("Pan", 2.0),
            // 96 semitones of transpose against 24 of pitch.
            "transpose" => ("Pitch", 4.0),
            "tune" => ("Fine", 2.0),
            "unison_detune" => ("Detune", 1.0),
            "unison_blend" => ("Blend", 1.0),
            _ => return None,
        };
        return Some((format!("{osc} {name}"), scale));
    }
    // A cutoff's 128 semitones against 10 octaves.
    let cutoff = 128.0 / 120.0;
    Some(match destination {
        "filter_1_cutoff" => ("Cutoff".to_owned(), cutoff),
        "filter_1_resonance" => ("Resonance".to_owned(), 1.0),
        "filter_1_drive" => ("Drive".to_owned(), 1.0),
        "filter_1_mix" => ("Filter Mix".to_owned(), 1.0),
        "filter_2_cutoff" => ("Cutoff 2".to_owned(), cutoff),
        "filter_2_resonance" => ("Resonance 2".to_owned(), 1.0),
        "filter_2_drive" => ("Drive 2".to_owned(), 1.0),
        "filter_2_mix" => ("Filter 2 Mix".to_owned(), 1.0),
        // 16 octaves of rate against 8.
        "lfo_1_frequency" => ("LFO 1 Rate".to_owned(), 2.0),
        "lfo_2_frequency" => ("LFO 2 Rate".to_owned(), 2.0),
        "lfo_3_frequency" => ("LFO 3 Rate".to_owned(), 2.0),
        "lfo_4_frequency" => ("LFO 4 Rate".to_owned(), 2.0),
        "volume" => ("Amp".to_owned(), 1.0),
        "voice_transpose" => ("Pitch".to_owned(), 4.0),
        _ => return None,
    })
}

/// Vital's effects that are on, as canopy effects, in a usual order:
/// distortion, compression and tone first, then movement, then space.
fn effects(preset: &Preset, out: &mut Converted) {
    let v = |key: &str| preset.num(key);
    let pct = |x: Option<f32>, default: f32| x.map_or(default, |x| (x * 100.0).clamp(0.0, 100.0));
    let add = |out: &mut Converted, effect: &'static str, params: &[(&str, f32)]| {
        out.effects.push((
            effect,
            params
                .iter()
                .map(|(id, v)| ((*id).to_owned(), *v))
                .collect(),
        ));
    };
    if preset.on("distortion_on") {
        let shape = match v("distortion_type").unwrap_or(0.0).round() as i32 {
            1 => 1.0,
            2 => 3.0,
            3 => 4.0,
            4 | 5 => 6.0,
            _ => 0.0,
        };
        add(
            out,
            crate::plugins::WAVESHAPER,
            &[
                ("shape", shape),
                (
                    "drive",
                    v("distortion_drive").unwrap_or(0.0).clamp(0.0, 48.0),
                ),
                ("mix", pct(v("distortion_mix"), 100.0)),
            ],
        );
    }
    if preset.on("compressor_on") {
        add(
            out,
            crate::plugins::MULTIBAND,
            &[("mix", pct(v("compressor_mix"), 100.0))],
        );
    }
    if preset.on("eq_on") {
        let q = |res: Option<f32>| 0.5 * (4.0 * res.unwrap_or(0.3)).exp2();
        let low_type = if v("eq_low_mode").unwrap_or(0.0) >= 0.5 {
            0.0
        } else {
            1.0
        };
        let band_type = if v("eq_band_mode").unwrap_or(0.0) >= 0.5 {
            3.0
        } else {
            2.0
        };
        let high_type = if v("eq_high_mode").unwrap_or(0.0) >= 0.5 {
            5.0
        } else {
            4.0
        };
        let mut params = vec![];
        for (band, kind, prefix) in [
            (1, low_type, "low"),
            (2, band_type, "band"),
            (3, high_type, "high"),
        ] {
            params.extend([
                (format!("b{band}_on"), 1.0),
                (format!("b{band}_type"), kind),
                (
                    format!("b{band}_freq"),
                    note_hz(v(&format!("eq_{prefix}_cutoff")).unwrap_or(60.0)),
                ),
                (
                    format!("b{band}_gain"),
                    v(&format!("eq_{prefix}_gain")).unwrap_or(0.0),
                ),
                (
                    format!("b{band}_q"),
                    q(v(&format!("eq_{prefix}_resonance"))),
                ),
            ]);
        }
        for band in 4..=6 {
            params.push((format!("b{band}_on"), 0.0));
        }
        out.effects.push((crate::plugins::EQ, params));
    }
    if preset.on("filter_fx_on") {
        let blend = v("filter_fx_blend").unwrap_or(0.0);
        let kind = if blend < 0.5 {
            5.0
        } else if blend > 1.5 {
            0.0
        } else {
            6.0
        };
        let resonance = v("filter_fx_resonance").unwrap_or(0.5);
        let mut params = vec![
            ("b1_on".to_owned(), 1.0),
            ("b1_type".to_owned(), kind),
            (
                "b1_freq".to_owned(),
                note_hz(v("filter_fx_cutoff").unwrap_or(60.0)),
            ),
            ("b1_q".to_owned(), 0.5 * (4.0 * resonance).exp2()),
        ];
        for band in 2..=6 {
            params.push((format!("b{band}_on"), 0.0));
        }
        out.effects.push((crate::plugins::EQ, params));
    }
    for (prefix, mode) in [
        ("chorus", effects::chorus::CHORUS),
        ("flanger", effects::chorus::FLANGER),
        ("phaser", effects::chorus::PHASER),
    ] {
        if preset.on(&format!("{prefix}_on")) {
            add(
                out,
                crate::plugins::CHORUS,
                &modulation(preset, prefix, mode),
            );
        }
    }
    if preset.on("delay_on") {
        let synced = v("delay_sync").unwrap_or(1.0).round() as i32;
        let note = |tempo: f32| -> f32 {
            let label = VITAL_TEMPOS[(tempo.round().clamp(0.0, 11.0)) as usize];
            effects::delay::NOTES
                .iter()
                .position(|(n, _)| *n == label)
                .map_or(6.0, |i| i as f32)
        };
        let mut params = vec![
            ("mix", pct(v("delay_dry_wet"), 33.0)),
            (
                "feedback",
                (v("delay_feedback").unwrap_or(0.5).abs() * 100.0).min(95.0),
            ),
            (
                "mode",
                if v("delay_style").unwrap_or(1.0).round() as i32 >= 2 {
                    1.0
                } else {
                    0.0
                },
            ),
        ];
        if synced == 0 {
            let hz = v("delay_frequency").unwrap_or(2.0).exp2();
            params.extend([("sync", 1.0), ("time", 1000.0 / hz.max(0.5))]);
        } else {
            params.extend([
                ("sync", 0.0),
                ("note", note(v("delay_tempo").unwrap_or(9.0))),
            ]);
        }
        let cutoff = v("delay_filter_cutoff").unwrap_or(60.0);
        let spread = v("delay_filter_spread").unwrap_or(1.0) * 48.0;
        params.extend([
            ("low_cut", note_hz(cutoff - spread).clamp(20.0, 2000.0)),
            ("high_cut", note_hz(cutoff + spread).clamp(1000.0, 20_000.0)),
        ]);
        add(out, crate::plugins::DELAY, &params);
    }
    if preset.on("reverb_on") {
        add(
            out,
            crate::plugins::REVERB,
            &[
                ("mix", pct(v("reverb_dry_wet"), 25.0)),
                ("size", pct(v("reverb_size"), 50.0)),
                ("decay", v("reverb_decay_time").unwrap_or(0.0).exp2()),
                ("predelay", v("reverb_delay").unwrap_or(0.0) * 1000.0),
                (
                    "low_cut",
                    note_hz(v("reverb_pre_low_cutoff").unwrap_or(0.0)).clamp(20.0, 1000.0),
                ),
                (
                    "damping",
                    note_hz(v("reverb_pre_high_cutoff").unwrap_or(110.0)).clamp(500.0, 20_000.0),
                ),
            ],
        );
    }
}

/// Vital's chorus, flanger or phaser (`prefix`) as canopy's Chorus in
/// `mode`. Vital keeps its delays as log2 seconds, its flanger's and
/// phaser's centres as notes, and the phaser's depth in semitones.
fn modulation(preset: &Preset, prefix: &str, mode: usize) -> Vec<(&'static str, f32)> {
    let v = |key: &str| preset.num(&format!("{prefix}_{key}"));
    let mut params = vec![("mode", mode as f32)];
    match v("sync").unwrap_or(1.0).round() as i32 {
        0 => params.extend([
            ("sync", 1.0),
            ("rate", v("frequency").unwrap_or(-3.0).exp2()),
        ]),
        sync => {
            let tempo = v("tempo").unwrap_or(4.0).round().clamp(0.0, 11.0) as usize;
            let (num, den) = VITAL_TEMPOS[tempo].split_once('/').unwrap_or(("1", "4"));
            let whole = num.parse::<f64>().unwrap_or(1.0) / den.parse::<f64>().unwrap_or(4.0);
            let quarters = 4.0
                * whole
                * match sync {
                    2 => 1.5,
                    3 => 2.0 / 3.0,
                    _ => 1.0,
                };
            params.extend([("sync", 0.0), ("note", nearest_note(quarters))]);
        }
    }
    // Vital's phase offset is the right side's lead; canopy's spread
    // folds it to 0..=180, the same apart either way round.
    let spread = |offset: f32| {
        let degrees = (offset * 360.0).rem_euclid(360.0);
        degrees.min(360.0 - degrees)
    };
    let feedback = |x: Option<f32>, default: f32| (x.unwrap_or(default) * 100.0).clamp(-95.0, 95.0);
    let mix = |x: Option<f32>, default: f32| (x.unwrap_or(default) * 100.0).clamp(0.0, 100.0);
    match mode {
        effects::chorus::CHORUS => {
            // Its voices sit between two delays: the sweep's middle is
            // halfway.
            let ms = |x: Option<f32>, default: f32| 1000.0 * x.unwrap_or(default).exp2();
            let delay = 0.5 * (ms(v("delay_1"), -9.0) + ms(v("delay_2"), -7.0));
            let voices = v("voices").unwrap_or(4.0).round().clamp(1.0, 4.0);
            params.extend([
                ("delay", delay.clamp(0.1, 40.0)),
                ("depth", mix(v("mod_depth"), 0.5)),
                ("voices", voices - 1.0),
                ("spread", 180.0 * v("spread").unwrap_or(1.0).clamp(0.0, 1.0)),
                ("mix", mix(v("dry_wet"), 0.5)),
            ]);
        }
        effects::chorus::FLANGER => {
            // A comb whose first notch-free peak is the centre note.
            let delay = 1000.0 / note_hz(v("center").unwrap_or(64.0)).max(1.0);
            params.extend([
                ("delay", delay.clamp(0.1, 40.0)),
                ("depth", mix(v("mod_depth"), 0.5)),
                ("feedback", feedback(v("feedback"), 0.5)),
                ("spread", spread(v("phase_offset").unwrap_or(0.33))),
                ("mix", mix(v("dry_wet"), 0.5)),
            ]);
        }
        _ => {
            // Canopy's full depth swings three octaves either way.
            let depth = v("mod_depth").unwrap_or(24.0) / 36.0 * 100.0;
            params.extend([
                (
                    "center",
                    note_hz(v("center").unwrap_or(80.0)).clamp(50.0, 10_000.0),
                ),
                ("depth", depth.clamp(0.0, 100.0)),
                ("feedback", feedback(v("feedback"), 0.5)),
                ("spread", spread(v("phase_offset").unwrap_or(0.33))),
                ("mix", mix(v("dry_wet"), 1.0)),
            ]);
        }
    }
    params
}

/// The index of canopy's Chorus note value nearest `quarters` long.
fn nearest_note(quarters: f64) -> f32 {
    let distance = |(_, q): &(&str, f64)| (q / quarters).ln().abs();
    effects::chorus::NOTES
        .iter()
        .enumerate()
        .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
        .map_or(13.0, |(i, _)| i as f32)
}

/// How an automated Vital parameter (FL's `0..=1` across its range) turns
/// into a canopy Wavetable parameter's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Automation {
    /// The same `0..=1`.
    Same,
    /// `-1..=1`.
    Bipolar,
    /// A fine tune, ±1 semitone, as cents.
    Cents,
    /// Unison detune, as cents.
    Detune,
    /// A filter cutoff (Vital's MIDI notes 8 to 136), as Hz.
    Cutoff,
}

impl Automation {
    pub fn apply(self, x: f64) -> f32 {
        let x = x.clamp(0.0, 1.0) as f32;
        match self {
            Automation::Same => x,
            Automation::Bipolar => 2.0 * x - 1.0,
            Automation::Cents => (2.0 * x - 1.0) * 100.0,
            Automation::Detune => (10.0 * x).powi(2),
            Automation::Cutoff => note_hz(8.0 + 128.0 * x),
        }
    }
}

/// The canopy Wavetable parameter automation of the Vital parameter named
/// `name` (as Vital names it to its host, "Oscillator 1 Wave Frame") moves.
pub fn automation(name: &str) -> Option<(String, Automation)> {
    let name = name.trim();
    if let Some(rest) = name.strip_prefix("Oscillator ") {
        let (n, param) = rest.split_once(' ')?;
        let x = match n {
            "1" => "a",
            "2" => "b",
            "3" => "c",
            _ => return None,
        };
        let (id, how) = match param {
            "Wave Frame" => ("position", Automation::Same),
            "Distortion Amount" => ("warp", Automation::Same),
            "Level" => ("level", Automation::Same),
            "Pan" => ("pan", Automation::Bipolar),
            "Tune" => ("fine", Automation::Cents),
            "Unison Detune" => ("detune", Automation::Detune),
            "Unison Blend" => ("blend", Automation::Same),
            "Stereo Spread" => ("width", Automation::Same),
            _ => return None,
        };
        let id = format!("osc_{x}_{id}");
        return has_param(&id).then_some((id, how));
    }
    if let Some(rest) = name.strip_prefix("Filter ") {
        let (n, param) = rest.split_once(' ')?;
        let prefix = match n {
            "1" => "filter",
            "2" => "filter2",
            _ => return None,
        };
        let (id, how) = match param {
            "Cutoff" => ("cutoff", Automation::Cutoff),
            "Resonance" => ("resonance", Automation::Same),
            "Drive" => ("drive", Automation::Same),
            "Mix" => ("mix", Automation::Same),
            _ => return None,
        };
        return Some((format!("{prefix}_{id}"), how));
    }
    if let Some(n) = name.strip_prefix("Macro ")
        && let Ok(n) = n.trim().parse::<u8>()
        && (1..=4).contains(&n)
    {
        return Some((format!("macro{n}"), Automation::Same));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_by_vital_names() {
        let (id, how) = automation("Oscillator 1 Wave Frame").unwrap();
        assert_eq!((id.as_str(), how.apply(0.5)), ("osc_a_position", 0.5));
        let (id, how) = automation("Filter 2 Cutoff").unwrap();
        assert_eq!(id, "filter2_cutoff");
        assert!((how.apply((69.0 - 8.0) / 128.0) - 440.0).abs() < 0.1);
        assert_eq!(automation("Macro 3").unwrap().0, "macro3");
        assert!(automation("Oscillator 1 Spectral Morph Amount").is_none());
        assert!(automation("Reverb Mix").is_none());
    }

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("TWFu").unwrap(), b"Man");
        assert_eq!(base64("TWE=").unwrap(), b"Ma");
        assert_eq!(base64("TQ==").unwrap(), b"M");
        assert!(base64("T!==").is_none());
    }

    /// A preset as a Vital state would hold it, after some binary.
    fn state(json: &str) -> Vec<u8> {
        let mut state = vec![0u8, 1, 2, b'{', 0, 0];
        state.extend(json.as_bytes());
        state.extend([0, 0, 9]);
        state
    }

    fn wave_data(wave: &[f32]) -> String {
        const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes: Vec<u8> = wave.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut acc = 0u32;
            for (i, b) in chunk.iter().enumerate() {
                acc |= u32::from(*b) << (16 - 8 * i);
            }
            for i in 0..chunk.len() + 1 {
                out.push(ABC[(acc >> (18 - 6 * i)) as usize & 63] as char);
            }
            for _ in chunk.len()..3 {
                out.push('=');
            }
        }
        out
    }

    #[test]
    fn a_preset_is_found_in_its_state_and_its_table_renders() {
        let sine: Vec<f32> = (0..FRAME)
            .map(|i| (std::f32::consts::TAU * i as f32 / FRAME as f32).sin() * 0.5)
            .collect();
        let saw: Vec<f32> = (0..FRAME)
            .map(|i| 2.0 * i as f32 / FRAME as f32 - 1.0)
            .collect();
        let json = format!(
            r#"{{"preset_name":"Test","settings":{{"osc_1_on":1.0,"osc_1_transpose":-12.0,
            "osc_1_wave_frame":128.0,"env_1_attack":0.5,"filter_1_on":1.0,"filter_1_cutoff":69.0,
            "wavetables":[{{"name":"Two","groups":[{{"components":[{{"type":"Wave Source",
            "interpolation_style":1,"keyframes":[{{"position":0,"wave_data":"{}"}},
            {{"position":255,"wave_data":"{}"}}]}}]}}]}}]}}}}"#,
            wave_data(&sine),
            wave_data(&saw)
        );
        let preset = Preset::from_state(&state(&json)).unwrap();
        assert_eq!(preset.name, "Test");
        let table = preset.wavetable(0).unwrap();
        assert_eq!(table.frames.len(), FRAMES);
        // Normalized as a whole: the saw reaches 1, the sine half that.
        let peak = |frame: &[f32]| frame.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!((peak(&table.frames[FRAMES - 1]) - 1.0).abs() < 0.01);
        assert!((peak(&table.frames[0]) - 0.5).abs() < 0.01);
        // Halfway, a quarter cycle in: the sine's 0.5 and the saw's -0.5 cancel.
        assert!(table.frames[128][FRAME / 4].abs() < 0.02);
        assert!(preset.wavetable(1).is_none());

        let converted = convert(&preset);
        let get = |id: &str| {
            converted
                .params
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, v)| *v)
        };
        assert_eq!(get("osc_a_on"), Some(1.0));
        assert_eq!(get("osc_a_octave"), Some(3.0), "an octave down");
        assert!((get("env1_attack").unwrap() - 62.5).abs() < 0.01, "0.5^4 s");
        assert!((get("filter_cutoff").unwrap() - 440.0).abs() < 0.01);
    }

    #[test]
    fn lines_bend_and_run_top_down() {
        let triangle: Value =
            serde_json::from_str(r#"{"points":[0.0,1.0,0.5,0.0,1.0,1.0],"powers":[0.0,0.0,0.0]}"#)
                .unwrap();
        assert_eq!(line_value(&triangle, 0.0), 0.0);
        assert_eq!(line_value(&triangle, 0.5), 1.0);
        assert!((line_value(&triangle, 0.25) - 0.5).abs() < 1e-6);
        let bent: Value =
            serde_json::from_str(r#"{"points":[0.0,1.0,1.0,0.0],"powers":[4.0]}"#).unwrap();
        assert!(line_value(&bent, 0.5) < 0.2, "a positive power starts slow");
    }

    #[test]
    fn wavetables_write_as_float_wavs() {
        let table = Wavetable {
            name: "t".into(),
            frames: vec![vec![0.25; FRAME]; 2],
        };
        let bytes = wav_bytes(&table);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let riff = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff + 8, bytes.len());
        assert!(find(&bytes, b"<!>2048").is_some());
        let data = find(&bytes, b"data").unwrap();
        let first = f32::from_le_bytes(bytes[data + 8..data + 12].try_into().unwrap());
        assert_eq!(first, 0.25);
    }
}
