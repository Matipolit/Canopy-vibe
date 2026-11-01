//! What the tools' numbers and names mean, both ways: beats and ticks,
//! "bar.beat", pitch names, parameters in their own units, colors and
//! automation targets. Every reader here refuses what is out of range with
//! a message that states the range; nothing is clamped quietly, because
//! an LLM that asked for 50 kHz should hear that it can't have it.

use instruments::{ParamDescriptor, ParameterKind};
use midi::{PPQ, Pitch, Tick};
use project::{
    AutomationTarget, Color, DeviceId, MAX_VOLUME_DB, ModulatorId, Project, SILENCE_DB, TempoMap,
    TrackId, TrackKind, normalized_to_pan, normalized_to_volume, pan_to_normalized,
    volume_to_normalized,
};
use serde_json::{Value, json};

/// The latest time a tool accepts, in beats: past a day of music at
/// 120 BPM, and far below where tick arithmetic could overflow.
pub const MAX_BEATS: f64 = 1_000_000.0;

/// A time in beats (quarter notes, fractions allowed) as ticks, rounded to
/// the nearest.
pub fn beats_to_tick(beats: f64) -> Result<Tick, String> {
    if !beats.is_finite() || beats < 0.0 {
        return Err(format!(
            "{beats} isn't a time; times are beats from 0 (the song's start)"
        ));
    }
    if beats > MAX_BEATS {
        return Err(format!(
            "{beats} beats is past the latest time, {MAX_BEATS}"
        ));
    }
    Ok(Tick::from_quarters(beats))
}

/// The same for a length or a distance, which may be negative (moving
/// earlier) but must be finite and within [`MAX_BEATS`].
pub fn beats_to_offset(beats: f64) -> Result<Tick, String> {
    if !beats.is_finite() || beats.abs() > MAX_BEATS {
        return Err(format!(
            "{beats} isn't a distance in beats (at most {MAX_BEATS} either way)"
        ));
    }
    Ok(Tick::from_quarters(beats))
}

pub fn tick_to_beats(t: Tick) -> f64 {
    t.get() as f64 / PPQ as f64
}

/// A time as a reader counts it, bars and beats from 1: "5.3" is bar 5,
/// beat 3. Off the beat, hundredths of the beat follow: "5.3.50" is half
/// way through it.
pub fn bar_beat(t: Tick, tempo: &TempoMap) -> String {
    let at = tempo.bar_beat(t);
    let beat_len = tempo.beat_len().get().max(1);
    let into_beat = t.get().rem_euclid(tempo.bar_len().get().max(1)) % beat_len;
    if into_beat == 0 {
        format!("{}.{}", at.bar, at.beat)
    } else {
        format!("{}.{}.{:02}", at.bar, at.beat, into_beat * 100 / beat_len)
    }
}

/// A pitch from a MIDI number (0–127) or a name with middle C (60) as "C3",
/// as the piano roll names them: "C-2" is 0, "F#2", "Bb3", "G8" is 127.
pub fn parse_pitch(v: &Value) -> Result<Pitch, String> {
    let help = "use a MIDI number 0–127 or a name like \"C3\" (= 60), \"F#2\" or \"Bb3\"";
    let number = match v {
        Value::Number(n) => n
            .as_f64()
            .filter(|f| f.fract() == 0.0)
            .ok_or_else(|| format!("{n} isn't a pitch; {help}"))?,
        Value::String(s) => match s.trim().parse::<f64>() {
            Ok(f) if f.fract() == 0.0 => f,
            _ => pitch_from_name(s.trim()).ok_or_else(|| format!("{s:?} isn't a pitch; {help}"))?,
        },
        other => return Err(format!("{other} isn't a pitch; {help}")),
    };
    if !(0.0..=127.0).contains(&number) {
        return Err(format!("{v} is outside the MIDI range; {help}"));
    }
    Ok(Pitch::new(number as u8).expect("checked against the range"))
}

fn pitch_from_name(name: &str) -> Option<f64> {
    let mut chars = name.chars().peekable();
    let class: i32 = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut shift = 0;
    while let Some(&c) = chars.peek() {
        match c {
            '#' | '♯' => shift += 1,
            'b' | '♭' => shift -= 1,
            _ => break,
        }
        chars.next();
    }
    let octave: i32 = chars.collect::<String>().parse().ok()?;
    Some(f64::from((octave + 2) * 12 + class + shift))
}

/// "C3" for 60, with sharps.
pub fn pitch_name(p: Pitch) -> String {
    let p = i32::from(p.get());
    format!("{}{}", project::NOTE_NAMES[(p % 12) as usize], p / 12 - 2)
}

/// An `f32` as JSON with the digits it was set with: 0.2, not
/// 0.20000000298023224.
pub fn number(value: f32) -> Value {
    if !value.is_finite() {
        return Value::Null;
    }
    format!("{value}")
        .parse::<f64>()
        .map_or(Value::Null, |f| json!(f))
}

/// A parameter's new value from what the LLM sent: a number in the
/// parameter's own unit, or a choice's label (any case; `true`/`false` for
/// an Off/On switch; a number for choices labelled with numbers).
pub fn param_value(d: &ParamDescriptor, v: &Value) -> Result<f32, String> {
    match d.kind {
        ParameterKind::Float { min, max, unit, .. } => {
            let range = range_text(min, max, unit);
            let Some(value) = v.as_f64() else {
                return Err(format!("{} takes a number, {range}; not {v}", d.id));
            };
            let value = value as f32;
            if !(min..=max).contains(&value) {
                return Err(format!("{} is {range}; {v} is out of range", d.id));
            }
            Ok(value)
        }
        ParameterKind::Enum { labels, .. } => {
            let wanted = match v {
                Value::String(s) => s.trim().to_lowercase(),
                Value::Bool(on) if is_switch(labels) => (if *on { "on" } else { "off" }).into(),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            labels
                .iter()
                .position(|l| l.to_lowercase() == wanted)
                .map(|i| i as f32)
                .ok_or_else(|| {
                    format!(
                        "{} is one of {}; {v} isn't one of them",
                        d.id,
                        labels
                            .iter()
                            .map(|l| format!("{l:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
        }
    }
}

fn is_switch(labels: &[&str]) -> bool {
    labels.len() == 2
        && labels[0].eq_ignore_ascii_case("off")
        && labels[1].eq_ignore_ascii_case("on")
}

/// "20–20000 Hz", "0–1".
fn range_text(min: f32, max: f32, unit: &str) -> String {
    if unit.is_empty() {
        format!("{min}–{max}")
    } else {
        format!("{min}–{max} {unit}")
    }
}

/// A parameter and its value as reads show it: a number with its unit,
/// range, scale and default, or a choice by label with every label.
pub fn describe_param(d: &ParamDescriptor, value: f32) -> Value {
    match d.kind {
        ParameterKind::Float {
            min,
            max,
            default,
            unit,
            logarithmic,
        } => json!({
            "id": d.id,
            "name": d.name,
            "value": number(d.clamp(value)),
            "unit": unit,
            "min": number(min),
            "max": number(max),
            "log": logarithmic,
            "default": number(default),
        }),
        ParameterKind::Enum { labels, default } => json!({
            "id": d.id,
            "name": d.name,
            "value": labels.get(d.clamp(value) as usize).copied().unwrap_or("?"),
            "choices": labels,
            "default": labels.get(default).copied().unwrap_or("?"),
        }),
    }
}

/// Track colors by name: the twelve of the track color picker (in its
/// order), plus a few other names people use for them.
const COLORS: [(&str, usize); 16] = [
    ("red", 0),
    ("orange", 1),
    ("yellow", 2),
    ("green", 3),
    ("teal", 4),
    ("cyan", 5),
    ("blue", 6),
    ("indigo", 7),
    ("purple", 8),
    ("pink", 9),
    ("brown", 10),
    ("grey", 11),
    ("coral", 0),
    ("amber", 1),
    ("lime", 2),
    ("gray", 11),
];

/// A color from "#rrggbb" or a name (see [`COLOR_NAMES`]).
pub fn color(v: &Value) -> Result<Color, String> {
    let help = format!("use \"#rrggbb\" or one of {}", COLOR_NAMES.join(", "));
    let Some(text) = v.as_str() else {
        return Err(format!("{v} isn't a color; {help}"));
    };
    let text = text.trim().to_lowercase();
    if let Some(hex) = text.strip_prefix('#')
        && hex.len() == 6
        && let Ok(rgb) = u32::from_str_radix(hex, 16)
    {
        return Ok(Color::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
    }
    COLORS
        .iter()
        .find(|(name, _)| *name == text)
        .map(|&(_, index)| Color::PALETTE[index])
        .ok_or_else(|| format!("{v} isn't a color; {help}"))
}

/// The color names offered, in the picker's order.
pub const COLOR_NAMES: [&str; 12] = [
    "red", "orange", "yellow", "green", "teal", "cyan", "blue", "indigo", "purple", "pink",
    "brown", "grey",
];

/// "#rrggbb", as reads show colors.
pub fn color_hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// An automation target from its JSON form: `"volume"`, `"pan"`,
/// `{"instrument": "cutoff"}`, `{"device": 12, "param": "freq"}` or
/// `{"modulator": 5, "param": "rate"}`. Only the form is checked here;
/// [`Units::of`] checks that it exists on a track.
pub fn parse_target(v: &Value) -> Result<AutomationTarget, String> {
    let help = "a target is \"volume\", \"pan\", {\"instrument\": \"<param>\"}, \
                {\"device\": <id>, \"param\": \"<param>\"} or {\"modulator\": <id>, \"param\": \"<param>\"}";
    let id = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
    let target = match v {
        Value::String(s) if s.eq_ignore_ascii_case("volume") => Some(AutomationTarget::Volume),
        Value::String(s) if s.eq_ignore_ascii_case("pan") => Some(AutomationTarget::Pan),
        Value::Object(o) => match (
            o.len(),
            o.get("instrument"),
            o.get("device"),
            o.get("modulator"),
            o.get("param"),
        ) {
            (1, Some(Value::String(param)), None, None, None) => {
                Some(AutomationTarget::InstrumentParam(param.clone()))
            }
            (2, None, Some(device), None, Some(Value::String(param))) => {
                id(device).map(|device| AutomationTarget::DeviceParam {
                    device: DeviceId(device),
                    param: param.clone(),
                })
            }
            (2, None, None, Some(modulator), Some(Value::String(param))) => {
                id(modulator).map(|modulator| AutomationTarget::ModulatorParam {
                    modulator: ModulatorId(modulator),
                    param: param.clone(),
                })
            }
            _ => None,
        },
        _ => None,
    };
    target.ok_or_else(|| format!("{v} isn't a target; {help}"))
}

/// The JSON form [`parse_target`] reads.
pub fn target_json(target: &AutomationTarget) -> Value {
    match target {
        AutomationTarget::Volume => json!("volume"),
        AutomationTarget::Pan => json!("pan"),
        AutomationTarget::InstrumentParam(param) => json!({ "instrument": param }),
        AutomationTarget::DeviceParam { device, param } => {
            json!({ "device": device.0, "param": param })
        }
        AutomationTarget::ModulatorParam { modulator, param } => {
            json!({ "modulator": modulator.0, "param": param })
        }
    }
}

/// How an automation target's `0..=1` values read in its own units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Units {
    /// The fader, in dB.
    Volume,
    /// -1 (left) to 1 (right).
    Pan,
    /// A parameter with a descriptor.
    Param(&'static ParamDescriptor),
    /// A value with no descriptor to say its units (modulators' parameters,
    /// for now): shown as stored, 0–1.
    Normalized,
}

impl Units {
    /// The units of `target` on `track`; an error if the track, its
    /// instrument, the device or the parameter doesn't exist there.
    pub fn of(
        project: &Project,
        track: TrackId,
        target: &AutomationTarget,
    ) -> Result<Units, String> {
        let t = project
            .track(track)
            .ok_or_else(|| format!("track {track} doesn't exist"))?;
        match target {
            AutomationTarget::Volume => Ok(Units::Volume),
            AutomationTarget::Pan => Ok(Units::Pan),
            AutomationTarget::InstrumentParam(param) => {
                let TrackKind::Midi { instrument, .. } = &t.kind else {
                    return Err(format!(
                        "track {track} has no instrument (it isn't a MIDI track)"
                    ));
                };
                let descriptor = instrument
                    .descriptor()
                    .ok_or_else(|| format!("track {track}'s instrument isn't available"))?;
                find_param(descriptor.params, param, descriptor.name).map(Units::Param)
            }
            AutomationTarget::DeviceParam { device, param } => {
                let d = t
                    .devices
                    .iter()
                    .find(|d| d.id == *device)
                    .ok_or_else(|| format!("device {device} isn't on track {track}"))?;
                let descriptor = d
                    .effect
                    .descriptor()
                    .ok_or_else(|| format!("device {device} ({}) isn't available", d.name()))?;
                find_param(descriptor.params, param, descriptor.name).map(Units::Param)
            }
            AutomationTarget::ModulatorParam { modulator, .. } => {
                if t.modulators.iter().any(|m| m.id == *modulator) {
                    Ok(Units::Normalized)
                } else {
                    Err(format!("modulator {modulator} isn't on track {track}"))
                }
            }
        }
    }

    /// A stored `0..=1` value in these units.
    pub fn read(self, normalized: f32) -> Value {
        match self {
            Units::Volume => number(normalized_to_volume(normalized)),
            Units::Pan => number(normalized_to_pan(normalized)),
            Units::Param(d) => describe_value(d, d.from_normalized(normalized)),
            Units::Normalized => number(normalized),
        }
    }

    /// A value in these units as stored, `0..=1`; out of range is an error.
    pub fn write(self, v: &Value) -> Result<f32, String> {
        let float = |name: &str, min: f32, max: f32, unit: &str| {
            let range = range_text(min, max, unit);
            match v.as_f64() {
                Some(f) if (f64::from(min)..=f64::from(max)).contains(&f) => Ok(f as f32),
                Some(_) => Err(format!("{name} is {range}; {v} is out of range")),
                None => Err(format!("{name} takes a number, {range}; not {v}")),
            }
        };
        match self {
            Units::Volume => {
                float("volume", SILENCE_DB, MAX_VOLUME_DB, "dB").map(volume_to_normalized)
            }
            Units::Pan => float("pan", -1.0, 1.0, "").map(pan_to_normalized),
            Units::Param(d) => param_value(d, v).map(|value| d.to_normalized(value)),
            Units::Normalized => float("value", 0.0, 1.0, ""),
        }
    }

    /// The unit values are in, for reads ("dB", "Hz", "" for none).
    pub fn unit(self) -> &'static str {
        match self {
            Units::Volume => "dB",
            Units::Pan | Units::Normalized => "",
            Units::Param(d) => match d.kind {
                ParameterKind::Float { unit, .. } => unit,
                ParameterKind::Enum { .. } => "",
            },
        }
    }
}

/// A value alone: a number, or a choice's label.
pub fn describe_value(d: &ParamDescriptor, value: f32) -> Value {
    match d.kind {
        ParameterKind::Float { .. } => number(d.clamp(value)),
        ParameterKind::Enum { labels, .. } => {
            json!(labels.get(d.clamp(value) as usize).copied().unwrap_or("?"))
        }
    }
}

/// Parameter `id` of a device called `owner`, or an error that says where
/// to look.
pub fn find_param(
    params: &'static [ParamDescriptor],
    id: &str,
    owner: &str,
) -> Result<&'static ParamDescriptor, String> {
    params
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("{owner} has no parameter {id:?}; get_device lists its parameters"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_and_ticks_round_trip() {
        assert_eq!(beats_to_tick(1.5), Ok(Tick::new(1440)));
        assert_eq!(tick_to_beats(Tick::new(1440)), 1.5);
        assert_eq!(beats_to_tick(0.0), Ok(Tick::ZERO));
        // A third of a beat lands on the nearest tick and comes back close.
        let third = beats_to_tick(1.0 / 3.0).unwrap();
        assert_eq!(third, Tick::new(320));
        assert!((tick_to_beats(third) - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn negative_beats_are_an_error() {
        assert!(beats_to_tick(-0.25).is_err());
        assert!(beats_to_tick(f64::NAN).is_err());
        assert!(beats_to_tick(f64::INFINITY).is_err());
        assert!(beats_to_tick(MAX_BEATS * 2.0).is_err());
        assert_eq!(beats_to_offset(-2.0), Ok(Tick::new(-1920)));
    }

    #[test]
    fn bar_beat_reads_one_based() {
        let tempo = TempoMap::default();
        assert_eq!(bar_beat(Tick::ZERO, &tempo), "1.1");
        assert_eq!(bar_beat(Tick::new(4 * PPQ), &tempo), "2.1");
        assert_eq!(bar_beat(Tick::new(16 * PPQ + 2 * PPQ), &tempo), "5.3");
        assert_eq!(
            bar_beat(Tick::new(16 * PPQ + 2 * PPQ + PPQ / 2), &tempo),
            "5.3.50"
        );
        assert_eq!(bar_beat(Tick::new(PPQ / 4), &tempo), "1.1.25");
    }

    #[test]
    fn pitch_names_parse_both_ways() {
        let pitch = |v: Value| parse_pitch(&v).map(|p| p.get());
        assert_eq!(pitch(json!("C3")), Ok(60));
        assert_eq!(pitch(json!("C-2")), Ok(0));
        assert_eq!(pitch(json!("G8")), Ok(127));
        assert_eq!(pitch(json!("Bb3")), Ok(70));
        assert_eq!(pitch(json!("F#2")), Ok(54));
        assert_eq!(pitch(json!("a3")), Ok(69));
        assert_eq!(pitch(json!(64)), Ok(64));
        assert_eq!(pitch(json!("64")), Ok(64));
        assert!(pitch(json!(128)).unwrap_err().contains("0–127"));
        assert!(pitch(json!("G#8")).is_err(), "128 by name");
        assert!(pitch(json!(-1)).is_err());
        assert!(pitch(json!(60.5)).is_err());
        assert!(pitch(json!("H3")).unwrap_err().contains("H3"));
        for p in 0..=127 {
            let p = Pitch::new(p).unwrap();
            assert_eq!(
                parse_pitch(&json!(pitch_name(p))),
                Ok(p),
                "{}",
                pitch_name(p)
            );
        }
        assert_eq!(pitch_name(Pitch::new(60).unwrap()), "C3");
        assert_eq!(pitch_name(Pitch::new(0).unwrap()), "C-2");
    }

    fn effect_param(effect: &str, id: &str) -> &'static ParamDescriptor {
        let descriptor = effects::find_builtin(effect).unwrap();
        find_param(descriptor.params, id, descriptor.name).unwrap()
    }

    #[test]
    fn out_of_range_values_name_the_range() {
        let threshold = effect_param("canopy.compressor", "threshold");
        let error = param_value(threshold, &json!(-80)).unwrap_err();
        assert!(error.contains("-60"), "{error}");
        assert!(error.contains('0'), "{error}");
        assert!(error.contains("dB"), "{error}");
        assert_eq!(param_value(threshold, &json!(-24)), Ok(-24.0));
        assert_eq!(
            param_value(threshold, &json!(0)),
            Ok(0.0),
            "the ends are in range"
        );
        assert!(
            param_value(threshold, &json!("loud"))
                .unwrap_err()
                .contains("number")
        );
    }

    #[test]
    fn choices_match_labels_case_insensitively() {
        let descriptor = effects::find_builtin("canopy.eq").unwrap();
        let (choice, labels) = descriptor
            .params
            .iter()
            .find_map(|p| match p.kind {
                ParameterKind::Enum { labels, .. } if labels.len() > 2 => Some((p, labels)),
                _ => None,
            })
            .expect("the EQ has a choice");
        let last = labels.len() - 1;
        assert_eq!(
            param_value(choice, &json!(labels[last].to_uppercase())),
            Ok(last as f32)
        );
        let error = param_value(choice, &json!("nonsense")).unwrap_err();
        for label in labels {
            assert!(error.contains(label), "{error} lists {label}");
        }
        // Switches take booleans.
        let switch = ParamDescriptor {
            id: "on",
            name: "On",
            kind: ParameterKind::Enum {
                labels: instruments::OFF_ON,
                default: 0,
            },
        };
        assert_eq!(param_value(&switch, &json!(true)), Ok(1.0));
        assert_eq!(param_value(&switch, &json!("off")), Ok(0.0));
    }

    #[test]
    fn colors_by_name_or_hex() {
        assert_eq!(color(&json!("#ff8000")), Ok(Color::rgb(255, 128, 0)));
        assert_eq!(color(&json!("#FF8000")), Ok(Color::rgb(255, 128, 0)));
        assert_eq!(color(&json!("blue")), Ok(Color::PALETTE[6]));
        assert_eq!(color(&json!("Gray")), Ok(Color::PALETTE[11]));
        for name in COLOR_NAMES {
            assert!(color(&json!(name)).is_ok(), "{name}");
        }
        let error = color(&json!("mauve")).unwrap_err();
        assert!(
            error.contains("#rrggbb") && error.contains("teal"),
            "{error}"
        );
        assert!(color(&json!("#12345")).is_err());
        assert_eq!(color_hex(Color::rgb(255, 128, 0)), "#ff8000");
    }

    #[test]
    fn targets_read_back_as_written() {
        for v in [
            json!("volume"),
            json!("pan"),
            json!({ "instrument": "cutoff" }),
            json!({ "device": 12, "param": "freq" }),
            json!({ "modulator": 5, "param": "rate" }),
        ] {
            assert_eq!(target_json(&parse_target(&v).unwrap()), v);
        }
        assert!(parse_target(&json!({ "device": 12 })).is_err());
        assert!(parse_target(&json!({ "device": -1, "param": "x" })).is_err());
        assert!(parse_target(&json!("width")).is_err());
    }

    #[test]
    fn units_convert_the_fader_both_ways() {
        let stored = Units::Volume.write(&json!(-6.0)).unwrap();
        assert_eq!(stored, volume_to_normalized(-6.0));
        assert_eq!(Units::Volume.read(stored), json!(-6.0));
        assert!(
            Units::Volume
                .write(&json!(12))
                .unwrap_err()
                .contains("-60–6 dB")
        );
        assert!(Units::Pan.write(&json!(-1.5)).is_err());
    }

    #[test]
    fn numbers_keep_the_digits_they_were_set_with() {
        assert_eq!(number(0.2), json!(0.2));
        assert_eq!(number(-60.0), json!(-60.0));
        assert_eq!(number(f32::NAN), Value::Null);
    }
}
