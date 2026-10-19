//! What a region's opcodes mean: typed settings, with SFZ's defaults and
//! units converted (percentages to fractions; times stay in seconds,
//! pitches in cents, levels in dB).

use super::parse::{RawRegion, Warnings, is_ignored};

/// What makes a region play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Note-on (`trigger=attack`, the default).
    Attack,
    /// Note-off, at the velocity of the note-on (`release`, `release_key`).
    Release,
    /// Note-on while no other key is down.
    First,
    /// Note-on while another key is down.
    Legato,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopMode {
    NoLoop,
    /// Plays to the end; note-off is ignored.
    OneShot,
    Continuous,
    /// Loops while the key is held, then plays on past the loop.
    Sustain,
}

/// How a voice stops when another region's `group` chokes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OffMode {
    Fast,
    /// Its own release.
    Normal,
    /// Over `off_time`.
    Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FilterType {
    Lpf1p,
    Hpf1p,
    Lpf2p,
    Hpf2p,
    Bpf2p,
    Brf2p,
}

/// A DAHDSR envelope. Times in seconds; `start` and `sustain` are `0..=1`;
/// `depth` (cents) is for the pitch and filter envelopes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct EgDesc {
    pub delay: f32,
    pub start: f32,
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub depth: f32,
    /// Added to the matching setting at full velocity, scaled down for softer notes.
    pub vel2delay: f32,
    pub vel2attack: f32,
    pub vel2hold: f32,
    pub vel2decay: f32,
    pub vel2sustain: f32,
    pub vel2release: f32,
    pub vel2depth: f32,
}

impl EgDesc {
    const ZERO: EgDesc = EgDesc {
        delay: 0.0,
        start: 0.0,
        attack: 0.0,
        hold: 0.0,
        decay: 0.0,
        sustain: 0.0,
        release: 0.0,
        depth: 0.0,
        vel2delay: 0.0,
        vel2attack: 0.0,
        vel2hold: 0.0,
        vel2decay: 0.0,
        vel2sustain: 0.0,
        vel2release: 0.0,
        vel2depth: 0.0,
    };
    const AMP: EgDesc = EgDesc {
        sustain: 1.0,
        ..Self::ZERO
    };

    fn apply(&mut self, setting: &str, value: &str) -> Result<(), Problem> {
        match setting {
            "delay" => self.delay = seconds(value)?,
            "start" => self.start = percent(value)?.clamp(0.0, 1.0),
            "attack" => self.attack = seconds(value)?,
            "hold" => self.hold = seconds(value)?,
            "decay" => self.decay = seconds(value)?,
            "sustain" => self.sustain = percent(value)?.clamp(0.0, 1.0),
            "release" => self.release = seconds(value)?,
            "depth" => self.depth = cents(value)?,
            "vel2delay" => self.vel2delay = number(value)?.clamp(-100.0, 100.0),
            "vel2attack" => self.vel2attack = number(value)?.clamp(-100.0, 100.0),
            "vel2hold" => self.vel2hold = number(value)?.clamp(-100.0, 100.0),
            "vel2decay" => self.vel2decay = number(value)?.clamp(-100.0, 100.0),
            "vel2sustain" => self.vel2sustain = percent(value)?.clamp(-1.0, 1.0),
            "vel2release" => self.vel2release = number(value)?.clamp(-100.0, 100.0),
            "vel2depth" => self.vel2depth = cents(value)?,
            _ => return Err(Problem::Unsupported),
        }
        Ok(())
    }
}

/// A sine LFO. `depth` is in cents for pitch and filter, dB for amplitude.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct LfoDesc {
    pub freq: f32,
    pub depth: f32,
    pub delay: f32,
    pub fade: f32,
}

impl LfoDesc {
    fn apply(&mut self, setting: &str, value: &str) -> Result<(), Problem> {
        match setting {
            "freq" => self.freq = number(value)?.clamp(0.0, 100.0),
            "depth" => self.depth = number(value)?.clamp(-9600.0, 9600.0),
            "delay" => self.delay = seconds(value)?,
            "fade" => self.fade = seconds(value)?,
            _ => return Err(Problem::Unsupported),
        }
        Ok(())
    }
}

/// One region's settings. Keys are signed so `-1` ("no key") survives
/// until [`build`] checks the range.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Region {
    /// As written, backslashes turned into slashes.
    pub sample: String,
    pub lokey: i32,
    pub hikey: i32,
    pub lovel: u8,
    pub hivel: u8,
    pub pitch_keycenter: i32,
    /// Cents per key.
    pub pitch_keytrack: f32,
    /// Cents at full velocity.
    pub pitch_veltrack: f32,
    pub pitch_random: f32,
    /// Semitones.
    pub transpose: f32,
    /// Cents.
    pub tune: f32,
    /// Cents at full bend up and down.
    pub bend_up: f32,
    pub bend_down: f32,
    /// dB.
    pub volume: f32,
    pub amplitude: f32,
    /// `-1..=1`.
    pub pan: f32,
    /// `-1..=1`; 1 keeps a stereo sample as it is, 0 folds it to mono.
    pub width: f32,
    /// `-1..=1`.
    pub amp_veltrack: f32,
    /// dB per key.
    pub amp_keytrack: f32,
    pub amp_keycenter: i32,
    /// dB, either way.
    pub amp_random: f32,
    pub ampeg: EgDesc,
    pub fileg: EgDesc,
    pub pitcheg: EgDesc,
    /// Seconds before the region starts.
    pub delay: f32,
    /// Frames.
    pub offset: u64,
    pub offset_random: u64,
    /// Last frame played.
    pub end: Option<u64>,
    pub loop_mode: Option<LoopMode>,
    pub loop_start: Option<u64>,
    /// Last frame of the loop.
    pub loop_end: Option<u64>,
    /// Seconds.
    pub loop_crossfade: Option<f32>,
    pub trigger: Trigger,
    /// dB per second the key was held, for release triggers.
    pub rt_decay: f32,
    pub seq_length: u32,
    pub seq_position: u32,
    pub lorand: f32,
    pub hirand: f32,
    pub group: i64,
    pub off_by: Option<i64>,
    pub off_mode: OffMode,
    pub off_time: f32,
    pub polyphony: Option<usize>,
    pub note_polyphony: Option<usize>,
    pub fil_type: FilterType,
    /// Hz; no filter when unset.
    pub cutoff: Option<f32>,
    /// dB of resonant peak.
    pub resonance: f32,
    pub fil_veltrack: f32,
    pub fil_keytrack: f32,
    pub fil_keycenter: i32,
    pub fil_random: f32,
    pub pitchlfo: LfoDesc,
    pub amplfo: LfoDesc,
    pub fillfo: LfoDesc,
    pub sw_last: Option<u8>,
    pub sw_lokey: Option<u8>,
    pub sw_hikey: Option<u8>,
    pub sw_down: Option<u8>,
    pub sw_up: Option<u8>,
    pub sw_default: Option<u8>,
    pub xfin_lovel: u8,
    pub xfin_hivel: u8,
    pub xfout_lovel: u8,
    pub xfout_hivel: u8,
    pub xfin_lokey: u8,
    pub xfin_hikey: u8,
    pub xfout_lokey: u8,
    pub xfout_hikey: u8,
    /// Equal-power crossfades (`power`, the default) rather than linear (`gain`).
    pub xf_velcurve_power: bool,
    pub xf_keycurve_power: bool,
}

impl Default for Region {
    fn default() -> Self {
        Region {
            sample: String::new(),
            lokey: 0,
            hikey: 127,
            lovel: 0,
            hivel: 127,
            pitch_keycenter: 60,
            pitch_keytrack: 100.0,
            pitch_veltrack: 0.0,
            pitch_random: 0.0,
            transpose: 0.0,
            tune: 0.0,
            bend_up: 200.0,
            bend_down: -200.0,
            volume: 0.0,
            amplitude: 1.0,
            pan: 0.0,
            width: 1.0,
            amp_veltrack: 1.0,
            amp_keytrack: 0.0,
            amp_keycenter: 60,
            amp_random: 0.0,
            ampeg: EgDesc::AMP,
            fileg: EgDesc::ZERO,
            pitcheg: EgDesc::ZERO,
            delay: 0.0,
            offset: 0,
            offset_random: 0,
            end: None,
            loop_mode: None,
            loop_start: None,
            loop_end: None,
            loop_crossfade: None,
            trigger: Trigger::Attack,
            rt_decay: 0.0,
            seq_length: 1,
            seq_position: 1,
            lorand: 0.0,
            hirand: 1.0,
            group: 0,
            off_by: None,
            off_mode: OffMode::Fast,
            off_time: 0.006,
            polyphony: None,
            note_polyphony: None,
            fil_type: FilterType::Lpf2p,
            cutoff: None,
            resonance: 0.0,
            fil_veltrack: 0.0,
            fil_keytrack: 0.0,
            fil_keycenter: 60,
            fil_random: 0.0,
            pitchlfo: LfoDesc::default(),
            amplfo: LfoDesc::default(),
            fillfo: LfoDesc::default(),
            sw_last: None,
            sw_lokey: None,
            sw_hikey: None,
            sw_down: None,
            sw_up: None,
            sw_default: None,
            xfin_lovel: 0,
            xfin_hivel: 0,
            xfout_lovel: 127,
            xfout_hivel: 127,
            xfin_lokey: 0,
            xfin_hikey: 0,
            xfout_lokey: 127,
            xfout_hikey: 127,
            xf_velcurve_power: true,
            xf_keycurve_power: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Problem {
    Unsupported,
    BadValue,
}

/// The region `raw` describes, or `None` (with a warning) if it can never play.
pub(super) fn build(raw: &RawRegion, warnings: &mut Warnings) -> Option<Region> {
    let mut region = Region::default();
    for opcode in &raw.opcodes {
        match region.apply(&opcode.name, &opcode.value, raw.note_offset) {
            Ok(()) => {}
            Err(Problem::Unsupported) => warnings.unsupported(&opcode.name),
            Err(Problem::BadValue) => {
                warnings.bad_value(&opcode.name, &opcode.value, &raw.location)
            }
        }
    }
    if region.sample.is_empty() {
        warnings.count("Regions without a sample, skipped");
        return None;
    }
    if region.hikey < 0 || region.lokey > 127 || region.lokey > region.hikey {
        warnings.count("Regions played by no key (CC-triggered?), skipped");
        return None;
    }
    region.lokey = region.lokey.max(0);
    region.hikey = region.hikey.min(127);
    if region.lovel > region.hivel {
        warnings.count("Regions with an empty velocity range, skipped");
        return None;
    }
    Some(region)
}

impl Region {
    fn apply(&mut self, name: &str, value: &str, note_offset: i32) -> Result<(), Problem> {
        let note = || note(value, note_offset);
        let key = || key(value, note_offset);
        match name {
            "sample" => self.sample = value.replace('\\', "/"),
            "lokey" => self.lokey = note()?,
            "hikey" => self.hikey = note()?,
            "key" => {
                let key = note()?;
                self.lokey = key;
                self.hikey = key;
                self.pitch_keycenter = key;
            }
            "pitch_keycenter" => self.pitch_keycenter = note()?,
            "pitch_keytrack" => self.pitch_keytrack = number(value)?.clamp(-1200.0, 1200.0),
            "pitch_veltrack" => self.pitch_veltrack = cents(value)?,
            "pitch_random" => self.pitch_random = cents(value)?.abs(),
            "lovel" => self.lovel = velocity(value)?,
            "hivel" => self.hivel = velocity(value)?,
            "tune" | "pitch" => self.tune = cents(value)?,
            "transpose" => self.transpose = number(value)?.clamp(-127.0, 127.0),
            "bend_up" | "bendup" => self.bend_up = cents(value)?,
            "bend_down" | "benddown" => self.bend_down = cents(value)?,
            "volume" => self.volume = number(value)?.clamp(-144.0, 24.0),
            "amplitude" => self.amplitude = percent(value)?.clamp(0.0, 1.0),
            "pan" => self.pan = percent(value)?.clamp(-1.0, 1.0),
            "width" => self.width = percent(value)?.clamp(-1.0, 1.0),
            "amp_veltrack" => self.amp_veltrack = percent(value)?.clamp(-1.0, 1.0),
            "amp_keytrack" => self.amp_keytrack = number(value)?.clamp(-96.0, 12.0),
            "amp_keycenter" => self.amp_keycenter = note()?,
            "amp_random" => self.amp_random = number(value)?.clamp(0.0, 24.0),
            "delay" => self.delay = seconds(value)?,
            "offset" => self.offset = frames(value)?,
            "offset_random" => self.offset_random = frames(value)?,
            "end" => self.end = Some(frames(value)?),
            "loop_mode" | "loopmode" => {
                self.loop_mode = Some(match word(value) {
                    "no_loop" => LoopMode::NoLoop,
                    "one_shot" => LoopMode::OneShot,
                    "loop_continuous" => LoopMode::Continuous,
                    "loop_sustain" => LoopMode::Sustain,
                    _ => return Err(Problem::BadValue),
                })
            }
            "loop_start" | "loopstart" => self.loop_start = Some(frames(value)?),
            "loop_end" | "loopend" => self.loop_end = Some(frames(value)?),
            "loop_crossfade" => self.loop_crossfade = Some(seconds(value)?),
            "trigger" => {
                self.trigger = match word(value) {
                    "attack" => Trigger::Attack,
                    "release" | "release_key" => Trigger::Release,
                    "first" => Trigger::First,
                    "legato" => Trigger::Legato,
                    _ => return Err(Problem::BadValue),
                }
            }
            "rt_decay" => self.rt_decay = number(value)?.clamp(0.0, 200.0),
            "seq_length" => self.seq_length = int(value)?.clamp(1, 100) as u32,
            "seq_position" => self.seq_position = int(value)?.clamp(1, 100) as u32,
            "lorand" => self.lorand = number(value)?.clamp(0.0, 1.0),
            "hirand" => self.hirand = number(value)?.clamp(0.0, 1.0),
            "group" => self.group = int(value)?,
            "off_by" | "offby" => self.off_by = Some(int(value)?),
            "off_mode" => {
                self.off_mode = match word(value) {
                    "fast" => OffMode::Fast,
                    "normal" => OffMode::Normal,
                    "time" => OffMode::Time,
                    _ => return Err(Problem::BadValue),
                }
            }
            "off_time" => self.off_time = seconds(value)?,
            "polyphony" => self.polyphony = Some(int(value)?.clamp(1, 1024) as usize),
            "note_polyphony" => self.note_polyphony = Some(int(value)?.clamp(1, 1024) as usize),
            "fil_type" | "filtype" => {
                self.fil_type = match word(value) {
                    "lpf_1p" => FilterType::Lpf1p,
                    "hpf_1p" => FilterType::Hpf1p,
                    "lpf_2p" => FilterType::Lpf2p,
                    "hpf_2p" => FilterType::Hpf2p,
                    "bpf_2p" => FilterType::Bpf2p,
                    "brf_2p" => FilterType::Brf2p,
                    _ => return Err(Problem::BadValue),
                }
            }
            "cutoff" => self.cutoff = Some(number(value)?.clamp(1.0, 100_000.0)),
            "resonance" => self.resonance = number(value)?.clamp(0.0, 40.0),
            "fil_veltrack" => self.fil_veltrack = cents(value)?,
            "fil_keytrack" => self.fil_keytrack = number(value)?.clamp(0.0, 1200.0),
            "fil_keycenter" => self.fil_keycenter = note()?,
            "fil_random" => self.fil_random = cents(value)?.abs(),
            "sw_last" => self.sw_last = Some(key()?),
            "sw_lokey" => self.sw_lokey = Some(key()?),
            "sw_hikey" => self.sw_hikey = Some(key()?),
            "sw_down" => self.sw_down = Some(key()?),
            "sw_up" => self.sw_up = Some(key()?),
            "sw_default" => self.sw_default = Some(key()?),
            "xfin_lovel" => self.xfin_lovel = velocity(value)?,
            "xfin_hivel" => self.xfin_hivel = velocity(value)?,
            "xfout_lovel" => self.xfout_lovel = velocity(value)?,
            "xfout_hivel" => self.xfout_hivel = velocity(value)?,
            "xfin_lokey" => self.xfin_lokey = key()?,
            "xfin_hikey" => self.xfin_hikey = key()?,
            "xfout_lokey" => self.xfout_lokey = key()?,
            "xfout_hikey" => self.xfout_hikey = key()?,
            "xf_velcurve" => self.xf_velcurve_power = curve(value)?,
            "xf_keycurve" => self.xf_keycurve_power = curve(value)?,
            _ => return self.apply_modulation(name, value),
        }
        Ok(())
    }

    /// Envelope and LFO opcodes, which share their settings' names.
    fn apply_modulation(&mut self, name: &str, value: &str) -> Result<(), Problem> {
        let targets: [(&str, Option<&mut EgDesc>, Option<&mut LfoDesc>); 6] = [
            ("ampeg_", Some(&mut self.ampeg), None),
            ("fileg_", Some(&mut self.fileg), None),
            ("pitcheg_", Some(&mut self.pitcheg), None),
            ("amplfo_", None, Some(&mut self.amplfo)),
            ("fillfo_", None, Some(&mut self.fillfo)),
            ("pitchlfo_", None, Some(&mut self.pitchlfo)),
        ];
        for (prefix, eg, lfo) in targets {
            if let Some(setting) = name.strip_prefix(prefix) {
                return match (eg, lfo) {
                    (Some(eg), _) => eg.apply(setting, value),
                    (_, Some(lfo)) => lfo.apply(setting, value),
                    _ => Err(Problem::Unsupported),
                };
            }
        }
        if is_ignored(name) {
            Ok(())
        } else {
            Err(Problem::Unsupported)
        }
    }
}

/// The first word of a value; trailing text after a number is ignored.
fn word(value: &str) -> &str {
    value.split_whitespace().next().unwrap_or("")
}

fn number(value: &str) -> Result<f32, Problem> {
    word(value)
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or(Problem::BadValue)
}

fn int(value: &str) -> Result<i64, Problem> {
    let word = word(value);
    word.parse::<i64>()
        .ok()
        .or_else(|| {
            word.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map(|v| v.round() as i64)
        })
        .ok_or(Problem::BadValue)
}

fn percent(value: &str) -> Result<f32, Problem> {
    Ok(number(value)? / 100.0)
}

fn seconds(value: &str) -> Result<f32, Problem> {
    Ok(number(value)?.clamp(0.0, 100.0))
}

fn cents(value: &str) -> Result<f32, Problem> {
    Ok(number(value)?.clamp(-9600.0, 9600.0))
}

fn frames(value: &str) -> Result<u64, Problem> {
    u64::try_from(int(value)?).map_err(|_| Problem::BadValue)
}

fn velocity(value: &str) -> Result<u8, Problem> {
    Ok(int(value)?.clamp(0, 127) as u8)
}

fn curve(value: &str) -> Result<bool, Problem> {
    match word(value) {
        "power" => Ok(true),
        "gain" => Ok(false),
        _ => Err(Problem::BadValue),
    }
}

/// A note number or name, shifted by `<control>`'s offsets. `-1` stays
/// `-1`, SFZ's "no key".
fn note(value: &str, offset: i32) -> Result<i32, Problem> {
    let note = parse_note(word(value)).ok_or(Problem::BadValue)?;
    Ok(if note < 0 { note } else { note + offset })
}

fn key(value: &str, offset: i32) -> Result<u8, Problem> {
    u8::try_from(note(value, offset)?)
        .ok()
        .filter(|k| *k <= 127)
        .ok_or(Problem::BadValue)
}

/// `60`, `c4` (middle C, 60), `c#4`, `db4`, `C-1` (0)...
pub(super) fn parse_note(text: &str) -> Option<i32> {
    if let Ok(number) = text.parse::<i32>() {
        return Some(number);
    }
    if let Ok(number) = text.parse::<f64>() {
        return number.is_finite().then_some(number.round() as i32);
    }
    let mut chars = text.chars();
    let semitone = match chars.next()?.to_ascii_lowercase() {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave) = if let Some(octave) = rest.strip_prefix(['#', '♯']) {
        (1, octave)
    } else if let Some(octave) = rest.strip_prefix(['b', '♭']) {
        (-1, octave)
    } else {
        (0, rest)
    };
    let octave: i32 = octave.parse().ok()?;
    Some((octave + 1) * 12 + semitone + accidental)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names() {
        for (text, note) in [
            ("60", 60),
            ("c4", 60),
            ("C4", 60),
            ("c#4", 61),
            ("db4", 61),
            ("b3", 59),
            ("bb3", 58),
            ("a4", 69),
            ("c-1", 0),
            ("g9", 127),
            ("-1", -1),
        ] {
            assert_eq!(parse_note(text), Some(note), "{text}");
        }
        for bad in ["h4", "c", "", "c#x"] {
            assert_eq!(parse_note(bad), None, "{bad}");
        }
    }

    #[test]
    fn units_are_converted() {
        let mut region = Region::default();
        for (name, value) in [
            ("pan", "-50"),
            ("ampeg_sustain", "25"),
            ("amp_veltrack", "0"),
            ("key", "a4"),
            ("loop_mode", "loop_sustain"),
        ] {
            region.apply(name, value, 0).unwrap();
        }
        assert_eq!(region.pan, -0.5);
        assert_eq!(region.ampeg.sustain, 0.25);
        assert_eq!(region.amp_veltrack, 0.0);
        assert_eq!(
            (region.lokey, region.hikey, region.pitch_keycenter),
            (69, 69, 69)
        );
        assert_eq!(region.loop_mode, Some(LoopMode::Sustain));
        assert_eq!(region.apply("lokey", "x", 0), Err(Problem::BadValue));
        assert_eq!(
            region.apply("ampeg_vel2foo", "1", 0),
            Err(Problem::Unsupported)
        );
        assert_eq!(
            region.apply("amplitude_oncc11", "1", 0),
            Err(Problem::Unsupported)
        );
        assert_eq!(region.apply("region_label", "Soft", 0), Ok(()));
    }
}
