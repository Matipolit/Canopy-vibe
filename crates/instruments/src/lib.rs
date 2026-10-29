//! The [`Instrument`] trait, parameter descriptions, and the built-in
//! instruments. An instrument turns note events into audio on the audio thread.

pub mod builtin;
pub mod envelope;

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use midi::NoteEvent;
use serde::{Deserialize, Serialize};

/// How many values an instrument or effect can publish for its UI to show
/// (levels, gain reduction, where an LFO is...). Slots past the ones a
/// device documents stay 0 (or whatever it fills them with).
pub const MAX_METERS: usize = 64;

/// Loaded data an instrument plays from, like a sampler's samples. Built on
/// a worker thread by the host and handed over before the instrument goes
/// to the audio thread.
pub type InstrumentData = Arc<dyn Any + Send + Sync>;

pub use builtin::{DEFAULT_INSTRUMENT, builtins, find_builtin};
pub use envelope::EnvelopeParams;

/// Which instrument a track plays, as stored in a project file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstrumentRef {
    pub format: Format,
    /// A built-in's id; a CLAP plugin's id; a VST3 class UID as 32 hex
    /// digits; a VST2 unique id as 8.
    pub id: String,
    pub version: String,
}

impl InstrumentRef {
    /// Refers to the built-in instrument `id`, at its current version.
    pub fn builtin(id: &str) -> InstrumentRef {
        InstrumentRef {
            format: Format::BuiltIn,
            id: id.to_owned(),
            version: find_builtin(id).map_or("", |d| d.version).to_owned(),
        }
    }

    /// The description of this instrument, if it's one we can load: a
    /// built-in, or a plugin whose description was registered with
    /// [`register_external`].
    pub fn descriptor(&self) -> Option<&'static BuiltInInstrumentDescriptor> {
        match self.format {
            Format::BuiltIn => find_builtin(&self.id),
            format => find_external(format, &self.id),
        }
    }

    /// Builds a fresh instance, or `None` if the instrument isn't available.
    /// Call [`Instrument::prepare`] before handing it to the audio thread.
    pub fn create(&self) -> Option<Box<dyn Instrument>> {
        self.descriptor().map(|d| (d.create)())
    }
}

impl Default for InstrumentRef {
    fn default() -> Self {
        InstrumentRef::builtin(DEFAULT_INSTRUMENT)
    }
}

/// What kind of thing an effect or instrument is: one of canopy's own, or
/// a plugin. Plugins are hosted through `engine::AudioLookup`'s plugin
/// hooks rather than their descriptor's `create`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    #[default]
    BuiltIn,
    Clap,
    Vst3,
    Vst2,
}

impl Format {
    /// For `skip_serializing_if`: built-ins don't write their format.
    pub fn is_builtin(&self) -> bool {
        *self == Format::BuiltIn
    }

    /// How the UI names a plugin format ("CLAP", "VST3", "VST2"); "" for
    /// built-ins.
    pub fn label(self) -> &'static str {
        match self {
            Format::BuiltIn => "",
            Format::Clap => "CLAP",
            Format::Vst3 => "VST3",
            Format::Vst2 => "VST2",
        }
    }
}

/// The name [`Format`] had when it was only for instruments.
pub type InstrumentFormat = Format;

/// A mono modulation signal an instrument exports (an LFO, an envelope, a
/// macro), for the track's modulation routes to use as a source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModOutput {
    /// Stable name used in project files ("lfo1"). Never rename one that
    /// has shipped.
    pub id: &'static str,
    pub name: &'static str,
    /// Moves over `-1..=1` rather than `0..=1`.
    pub bipolar: bool,
}

/// Descriptions of plugin instruments, registered by the plugin host at run
/// time so every `descriptor()` call site works for them too. Each is
/// leaked: descriptors are `&'static` everywhere, and there are only as
/// many as plugins loaded (and rescanned).
static EXTERNAL: Registry<BuiltInInstrumentDescriptor> = Registry::new();

/// Registers (or replaces) the description of plugin instrument `id`, and
/// returns it, leaked. A replaced description stays valid for whoever
/// still holds it. Never call it on the audio thread.
pub fn register_external(
    format: Format,
    id: &str,
    descriptor: BuiltInInstrumentDescriptor,
) -> &'static BuiltInInstrumentDescriptor {
    EXTERNAL.register(format, id, descriptor)
}

/// The registered description of plugin instrument `id`, if any.
pub fn find_external(format: Format, id: &str) -> Option<&'static BuiltInInstrumentDescriptor> {
    EXTERNAL.find(format, id)
}

/// Leaked descriptors by format and id, behind a lock only the UI side
/// takes (the audio thread never looks descriptors up). Effects keep
/// theirs in one too.
pub struct Registry<T: 'static> {
    map: std::sync::RwLock<Option<HashMap<Format, HashMap<String, &'static T>>>>,
}

impl<T: 'static> Registry<T> {
    pub const fn new() -> Registry<T> {
        Registry {
            map: std::sync::RwLock::new(None),
        }
    }

    pub fn register(&self, format: Format, id: &str, descriptor: T) -> &'static T {
        let leaked: &'static T = Box::leak(Box::new(descriptor));
        self.map
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_default()
            .entry(format)
            .or_default()
            .insert(id.to_owned(), leaked);
        leaked
    }

    pub fn find(&self, format: Format, id: &str) -> Option<&'static T> {
        self.map
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()?
            .get(&format)?
            .get(id)
            .copied()
    }
}

impl<T: 'static> Default for Registry<T> {
    fn default() -> Self {
        Registry::new()
    }
}

pub struct BuiltInInstrumentDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    pub author: &'static str,
    /// The same list [`Instrument::params`] returns, available without an instance.
    pub params: &'static [ParamDescriptor],
    /// Envelopes made of some of those parameters, which the UI shows as
    /// envelope editors instead of separate knobs.
    pub envelopes: &'static [EnvelopeParams],
    pub create: fn() -> Box<dyn Instrument>,
    /// What the instrument plays from besides its parameters, if anything:
    /// the host loads it (on a worker thread) from the file the track names
    /// and passes it to [`Instrument::set_data`].
    pub data: DataKind,
    /// Where parameter `index` sits in menus that list every parameter
    /// (automation's), as up to two levels of submenus, outermost first; ""
    /// for none. Each level's name begins the parameter's name, which the
    /// menu shortens by it. Instruments with few parameters use [`no_groups`].
    pub groups: fn(usize) -> [&'static str; 2],
}

impl BuiltInInstrumentDescriptor {
    /// The modulators the instrument exports ([`Instrument::mod_outputs`]
    /// fills them in this order). A lookup by id rather than a field, so
    /// adding it didn't touch every instrument's descriptor; plugins
    /// export none.
    pub fn mod_outputs(&self) -> &'static [ModOutput] {
        builtin::mod_outputs(self.id)
    }
}

/// [`BuiltInInstrumentDescriptor::groups`] for instruments whose
/// parameters all sit at the top of a menu.
pub fn no_groups(_: usize) -> [&'static str; 2] {
    ["", ""]
}

/// Data an instrument needs loaded for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    None,
    /// An SFZ instrument: the `.sfz` file and the samples it maps.
    Sfz,
    /// The Wavetable's user tables: up to one WAV file per oscillator,
    /// which a project's track lists (its `tables`), loaded into one
    /// `builtin::wavetable::UserTables`. The instrument plays without them.
    Wavetables,
    /// The Slicer's loop: the track's audio file, matched to the tempo and
    /// cut where the track's slices say, as one
    /// `builtin::slicer::SliceData`. Silent without it.
    Slices,
}

/// A sound generator driven by note events.
///
/// Built and prepared on the UI thread, then moved to the audio thread, where
/// only `reset`, `process` and `set_param` are called.
pub trait Instrument: Send {
    /// Called before the instrument goes to the audio thread, and again if the
    /// sample rate changes. Allocate voices and buffers here. `process` is
    /// never called with blocks longer than `max_block`.
    fn prepare(&mut self, sample_rate: f32, max_block: usize);

    /// Silence every voice immediately, with no release.
    fn reset(&mut self);

    /// Render one block, overwriting `left` and `right` (same length).
    /// `events` are sorted by frame, and every frame is `< left.len()`.
    /// `AllNotesOff` releases every voice. Must be real-time safe.
    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]);

    fn params(&self) -> &'static [ParamDescriptor];

    /// Set parameter `index` (its position in [`Self::params`]) to `value`, in
    /// the parameter's own units; enum parameters take the label index.
    /// Out-of-range values are clamped and unknown indices ignored.
    /// Real-time safe: parameters are addressed by index so that changing one
    /// never allocates or frees a string on the audio thread.
    fn set_param(&mut self, index: usize, value: f32);

    /// The song's tempo, for tempo-synced LFOs and the like. Called before
    /// the instrument goes to the audio thread and whenever the tempo
    /// changes; real-time safe.
    fn set_tempo(&mut self, _bpm: f64) {}

    /// Where the song is at the start of the next block, in quarter notes
    /// from its start, and whether it's playing: for instruments that
    /// follow the beat (plugins' tempo-synced arpeggiators and LFOs). Called
    /// before every `process`; real-time safe.
    fn set_position(&mut self, _beat: f64, _playing: bool) {}

    /// Values for the UI to show (see [`MAX_METERS`]); real-time safe. The
    /// instrument decides what each slot means and documents it.
    fn meters(&self, _out: &mut [f32; MAX_METERS]) {}

    /// Data the host loaded for it (see [`DataKind`]), before it goes to the
    /// audio thread; never called on the audio thread.
    fn set_data(&mut self, _data: InstrumentData) {}

    /// Like [`Self::set_param`], for a value a modulator moves: called at
    /// most once per parameter per engine block (`MAX_BLOCK` frames),
    /// before `process`. Reach the value within that block (a ramp of at
    /// most one block, not the knob smoothing) and stay stable however
    /// fast it moves. Enum parameters just switch. Real-time safe.
    fn modulate(&mut self, index: usize, value: f32) {
        self.set_param(index, value);
    }

    /// On the audio thread, when the engine stops running this instance
    /// (its track is gone, or it was replaced), before it goes back to the
    /// UI thread to be dropped. Called exactly once. Plugins stop
    /// processing here. Real-time safe.
    fn retire(&mut self) {}

    /// The current value of each exported modulator (the descriptor's
    /// [`BuiltInInstrumentDescriptor::mod_outputs`], in order) as of the
    /// end of the last `process`: `-1..=1` if bipolar, else `0..=1`.
    /// `out.len()` is that list's length. Audio thread; real-time safe.
    fn mod_outputs(&self, _out: &mut [f32]) {}
}

/// A float parameter: `min` to `max`, starting at `default`, shown with
/// `unit`; `logarithmic` for frequencies and times, whose knobs move
/// through the range on a log scale.
pub const fn float(
    id: &'static str,
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
    unit: &'static str,
    logarithmic: bool,
) -> ParamDescriptor {
    ParamDescriptor {
        id,
        name,
        kind: ParameterKind::Float {
            min,
            max,
            default,
            unit,
            logarithmic,
        },
    }
}

/// An enum parameter: one of `labels`, by index, starting at `default`.
pub const fn choice(
    id: &'static str,
    name: &'static str,
    labels: &'static [&'static str],
    default: usize,
) -> ParamDescriptor {
    ParamDescriptor {
        id,
        name,
        kind: ParameterKind::Enum { labels, default },
    }
}

/// The labels of an on/off switch.
pub const OFF_ON: &[&str] = &["Off", "On"];

/// Parameter `index` of `params` as `values` (indexed the same way) holds
/// it, clamped to its range; its default where `values` is too short or
/// holds NaN. For values that come from outside (a project, the UI), which
/// may be either.
pub fn param_value(params: &[ParamDescriptor], values: &[f32], index: usize) -> f32 {
    let param = &params[index];
    match values.get(index) {
        Some(v) if !v.is_nan() => param.clamp(*v),
        _ => param.default_value(),
    }
}

/// `value` for parameter `index` of `params`, clamped to its range: what
/// `set_param` should apply. `None` for an index past the end, or NaN, which
/// would pass through clamping and could leave a voice stuck at NaN.
#[inline]
pub fn checked_value(params: &[ParamDescriptor], index: usize, value: f32) -> Option<f32> {
    let param = params.get(index)?;
    (!value.is_nan()).then(|| param.clamp(value))
}

/// A block of `len` frames split at its note events: each stretch of frames
/// up to the next event with that event, then the stretch after the last
/// one with `None`. An event past the block's end counts as at its end, and
/// one earlier than the event before it as at the same frame, so the
/// stretches are in order and cover `0..len` exactly.
pub fn segments(
    events: &[NoteEvent],
    len: usize,
) -> impl Iterator<Item = (std::ops::Range<usize>, Option<midi::NoteEventKind>)> + '_ {
    let mut start = 0;
    events.iter().map(Some).chain([None]).map(move |event| {
        let end = event.map_or(len, |e| (e.frame as usize).clamp(start, len));
        let span = start..end;
        start = end;
        (span, event.map(|e| e.kind))
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDescriptor {
    /// Stable name used in project files. Never rename one that has shipped.
    pub id: &'static str,
    pub name: &'static str,
    pub kind: ParameterKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParameterKind {
    Float {
        min: f32,
        max: f32,
        default: f32,
        /// Shown after the value, e.g. `"dB"` or `"Hz"`. Empty for none.
        unit: &'static str,
        /// Knobs and automation move through the range on a log scale
        /// (frequencies, times). With `min` at zero the scale still reaches
        /// it: logarithmic down to [`ZERO_KNEE`] of the range, nearly
        /// straight below.
        logarithmic: bool,
    },
    Enum {
        labels: &'static [&'static str],
        default: usize,
    },
}

/// Where a logarithmic range that starts at zero stops being logarithmic,
/// as a share of the range: for times up to 2 s, 0.2 ms.
pub const ZERO_KNEE: f32 = 1e-4;

impl ParamDescriptor {
    pub fn default_value(&self) -> f32 {
        match self.kind {
            ParameterKind::Float { default, .. } => default,
            ParameterKind::Enum { default, .. } => default as f32,
        }
    }

    pub fn clamp(&self, value: f32) -> f32 {
        match self.kind {
            ParameterKind::Float { min, max, .. } => value.clamp(min, max),
            ParameterKind::Enum { labels, .. } => value
                .round()
                .clamp(0.0, labels.len().saturating_sub(1) as f32),
        }
    }

    /// Maps a value onto `0..=1`, the scale knobs and automation lanes use.
    pub fn to_normalized(&self, value: f32) -> f32 {
        let value = self.clamp(value);
        match self.kind {
            ParameterKind::Float {
                min,
                max,
                logarithmic,
                ..
            } => {
                if max <= min {
                    0.0
                } else if logarithmic && min <= 0.0 {
                    let knee = (max - min) * ZERO_KNEE;
                    (1.0 + (value - min) / knee).ln() / (1.0 + (max - min) / knee).ln()
                } else if logarithmic {
                    (value / min).ln() / (max / min).ln()
                } else {
                    (value - min) / (max - min)
                }
            }
            ParameterKind::Enum { labels, .. } => {
                if labels.len() < 2 {
                    0.0
                } else {
                    value / (labels.len() - 1) as f32
                }
            }
        }
    }

    /// The inverse of [`Self::to_normalized`].
    pub fn from_normalized(&self, normalized: f32) -> f32 {
        let t = normalized.clamp(0.0, 1.0);
        match self.kind {
            ParameterKind::Float {
                min,
                max,
                logarithmic,
                ..
            } => {
                if logarithmic && min <= 0.0 {
                    let knee = (max - min) * ZERO_KNEE;
                    let span = (1.0 + (max - min) / knee).powf(t) - 1.0;
                    (min + knee * span).min(max)
                } else if logarithmic {
                    min * (max / min).powf(t)
                } else {
                    min + (max - min) * t
                }
            }
            ParameterKind::Enum { labels, .. } => {
                (t * labels.len().saturating_sub(1) as f32).round()
            }
        }
    }

    /// The value as shown to the user, with its unit.
    pub fn format(&self, value: f32) -> String {
        let value = self.clamp(value);
        match self.kind {
            ParameterKind::Float { unit, .. } => {
                let digits = match value.abs() {
                    v if v >= 100.0 => 0,
                    v if v >= 10.0 => 1,
                    _ => 2,
                };
                if unit.is_empty() {
                    format!("{value:.digits$}")
                } else {
                    format!("{value:.digits$} {unit}")
                }
            }
            ParameterKind::Enum { labels, .. } => labels
                .get(value as usize)
                .copied()
                .unwrap_or("?")
                .to_owned(),
        }
    }
}

impl ParamDescriptor {
    /// A value typed in, as [`Self::format`] and knobs show values or
    /// plainer: "250", "250 ms", "1.5 s", "2k", "2.5 kHz", "-6 dB", "-inf",
    /// "50%" (of a range of 0 to 1, if that's what it is), a note name
    /// ("A4") for a frequency; a choice's label or the start of one. Any
    /// other unit typed after the number is ignored. Clamped to the range;
    /// `None` if it can't be read.
    pub fn parse(&self, text: &str) -> Option<f32> {
        let text = text.trim().to_lowercase();
        if text.is_empty() {
            return None;
        }
        match self.kind {
            ParameterKind::Enum { labels, .. } => {
                let lower: Vec<String> = labels.iter().map(|l| l.to_lowercase()).collect();
                let exact = lower.iter().position(|l| *l == text);
                let mut starting = lower
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.starts_with(&text));
                let prefix = match (starting.next(), starting.next()) {
                    (Some((i, _)), None) => Some(i),
                    _ => None,
                };
                exact.or(prefix).map(|i| i as f32)
            }
            ParameterKind::Float { min, max, unit, .. } => {
                let unit = unit.to_lowercase();
                // A decimal comma, as many keyboards type it.
                let text = text.replace(',', ".");
                if text.trim_end_matches("db").trim() == "-inf" {
                    return Some(min);
                }
                if unit == "hz"
                    && let Some(hz) = note_hz(&text)
                {
                    return Some(self.clamp(hz));
                }
                let split = text
                    .char_indices()
                    .find(|(i, c)| {
                        !(c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0))
                    })
                    .map_or(text.len(), |(i, _)| i);
                let mut value: f32 = text[..split].parse().ok()?;
                let suffix = text[split..].trim();
                value *= match suffix {
                    "k" => 1000.0,
                    "khz" if unit == "hz" => 1000.0,
                    "s" | "sec" | "secs" if unit == "ms" => 1000.0,
                    "ms" if unit == "s" => 0.001,
                    "%" if unit != "%" && max <= 1.0 => 0.01,
                    _ => 1.0,
                };
                value.is_finite().then(|| self.clamp(value))
            }
        }
    }
}

/// A note name's frequency, A4 at 440 Hz: "c4", "f#2", "bb-1".
fn note_hz(text: &str) -> Option<f32> {
    let mut chars = text.chars();
    let semitone = match chars.next()? {
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
    let (shift, octave) = if let Some(octave) = rest.strip_prefix('#') {
        (1, octave)
    } else if let Some(octave) = rest.strip_prefix('b').filter(|o| !o.is_empty()) {
        (-1, octave)
    } else {
        (0, rest)
    };
    let octave: i32 = octave.parse().ok()?;
    let midi = 12 * (octave + 1) + semitone + shift;
    Some(dsp::note_hz(midi as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_the_block_in_order_whatever_the_frames() {
        let at = |frame| NoteEvent {
            frame,
            kind: midi::NoteEventKind::AllNotesOff,
        };
        let events = [at(5), at(2), at(40)];
        let spans: Vec<_> = segments(&events, 10)
            .map(|(span, e)| (span, e.is_some()))
            .collect();
        assert_eq!(
            spans,
            [(0..5, true), (5..5, true), (5..10, true), (10..10, false)]
        );
    }

    #[test]
    fn checked_values_are_clamped_and_never_nan() {
        assert_eq!(checked_value(&[CUTOFF], 0, 1e9), Some(20_000.0));
        assert_eq!(checked_value(&[CUTOFF], 0, f32::NAN), None);
        assert_eq!(checked_value(&[CUTOFF], 1, 100.0), None);
    }

    const CUTOFF: ParamDescriptor = ParamDescriptor {
        id: "cutoff",
        name: "Cutoff",
        kind: ParameterKind::Float {
            min: 20.0,
            max: 20_000.0,
            default: 1000.0,
            unit: "Hz",
            logarithmic: true,
        },
    };

    const SHAPE: ParamDescriptor = ParamDescriptor {
        id: "shape",
        name: "Shape",
        kind: ParameterKind::Enum {
            labels: &["Sine", "Saw", "Square"],
            default: 1,
        },
    };

    #[test]
    fn normalized_round_trips() {
        for value in [20.0, 200.0, 1000.0, 20_000.0] {
            let back = CUTOFF.from_normalized(CUTOFF.to_normalized(value));
            assert!((back - value).abs() / value < 1e-4, "{value} -> {back}");
        }
        assert!(
            (CUTOFF.to_normalized(632.456) - 0.5).abs() < 1e-3,
            "log midpoint"
        );
        for index in 0..3 {
            let index = index as f32;
            assert_eq!(SHAPE.from_normalized(SHAPE.to_normalized(index)), index);
        }
    }

    /// A time from zero, on the log scale that reaches it.
    const ATTACK: ParamDescriptor = ParamDescriptor {
        id: "attack",
        name: "Attack",
        kind: ParameterKind::Float {
            min: 0.0,
            max: 2000.0,
            default: 1.0,
            unit: "ms",
            logarithmic: true,
        },
    };

    #[test]
    fn logarithmic_ranges_can_start_at_zero() {
        assert_eq!(ATTACK.to_normalized(0.0), 0.0);
        assert_eq!(ATTACK.from_normalized(0.0), 0.0);
        assert_eq!(ATTACK.from_normalized(1.0), 2000.0);
        for value in [0.05, 1.0, 10.0, 100.0, 1999.0] {
            let back = ATTACK.from_normalized(ATTACK.to_normalized(value));
            assert!((back - value).abs() / value < 1e-3, "{value} -> {back}");
        }
        // Logarithmic above the knee: each tenfold step about as far.
        let step = |a: f32, b: f32| ATTACK.to_normalized(b) - ATTACK.to_normalized(a);
        assert!((step(10.0, 100.0) - step(100.0, 1000.0)).abs() < 0.01);
        assert!(ATTACK.to_normalized(1.0) < 0.25, "short times get room");
    }

    #[test]
    fn typed_values_read_as_shown() {
        let parse = |p: &ParamDescriptor, text: &str| p.parse(text);
        assert_eq!(parse(&CUTOFF, "250"), Some(250.0));
        assert_eq!(parse(&CUTOFF, " 2.5 kHz"), Some(2500.0));
        assert_eq!(parse(&CUTOFF, "2k"), Some(2000.0));
        assert_eq!(parse(&CUTOFF, "1234 Hz"), Some(1234.0));
        assert_eq!(parse(&CUTOFF, "50000"), Some(20_000.0), "clamped");
        let a4 = parse(&CUTOFF, "A4").unwrap();
        assert!((a4 - 440.0).abs() < 1e-3);
        let c4 = parse(&CUTOFF, "c4").unwrap();
        assert!((c4 - 261.63).abs() < 0.01);
        assert!((parse(&CUTOFF, "bb2").unwrap() - 116.54).abs() < 0.01);
        assert_eq!(parse(&ATTACK, "1.5 s"), Some(1500.0));
        assert_eq!(parse(&ATTACK, "0"), Some(0.0));
        assert_eq!(parse(&ATTACK, "0,5"), Some(0.5));
        assert_eq!(parse(&ATTACK, "fast"), None);
        assert_eq!(parse(&ATTACK, ""), None);
        assert_eq!(parse(&SHAPE, "square"), Some(2.0));
        assert_eq!(parse(&SHAPE, "Sa"), Some(1.0), "a label's start");
        assert_eq!(parse(&SHAPE, "s"), None, "two labels start so");
        let gain = ParamDescriptor {
            id: "gain",
            name: "Gain",
            kind: ParameterKind::Float {
                min: -60.0,
                max: 24.0,
                default: 0.0,
                unit: "dB",
                logarithmic: false,
            },
        };
        assert_eq!(parse(&gain, "+3.0 dB"), Some(3.0));
        assert_eq!(parse(&gain, "-6"), Some(-6.0));
        assert_eq!(parse(&gain, "-inf dB"), Some(-60.0));
        let amount = ParamDescriptor {
            id: "amount",
            name: "Amount",
            kind: ParameterKind::Float {
                min: 0.0,
                max: 1.0,
                default: 0.0,
                unit: "",
                logarithmic: false,
            },
        };
        assert_eq!(parse(&amount, "50%"), Some(0.5));
        assert_eq!(parse(&amount, "0.25"), Some(0.25));
    }

    #[test]
    fn clamps_and_formats() {
        assert_eq!(CUTOFF.clamp(5.0), 20.0);
        assert_eq!(SHAPE.clamp(7.0), 2.0);
        assert_eq!(SHAPE.format(1.2), "Saw");
        assert_eq!(CUTOFF.format(1234.0), "1234 Hz");
        assert_eq!(SHAPE.default_value(), 1.0);
    }

    /// Records what reaches it, to check the trait's default methods.
    struct Recorder(Vec<(usize, f32)>);

    impl Instrument for Recorder {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn reset(&mut self) {}
        fn process(&mut self, _: &[NoteEvent], _: &mut [f32], _: &mut [f32]) {}
        fn params(&self) -> &'static [ParamDescriptor] {
            &[]
        }
        fn set_param(&mut self, index: usize, value: f32) {
            self.0.push((index, value));
        }
    }

    #[test]
    fn modulation_and_retiring_default_to_plain_settings() {
        let mut instrument = Recorder(Vec::new());
        instrument.modulate(3, 0.5);
        instrument.retire();
        let mut out = [7.0; 2];
        instrument.mod_outputs(&mut out);
        assert_eq!(instrument.0, [(3, 0.5)], "modulate sets the parameter");
        assert_eq!(out, [7.0; 2], "no outputs, nothing written");
        for descriptor in builtins() {
            for output in descriptor.mod_outputs() {
                assert!(!output.id.is_empty(), "{}", descriptor.id);
            }
        }
    }

    fn fake(name: &'static str) -> BuiltInInstrumentDescriptor {
        BuiltInInstrumentDescriptor {
            id: "test.fake-synth",
            name,
            description: "",
            version: "1",
            author: "",
            params: &[CUTOFF],
            envelopes: &[],
            create: || Box::new(Recorder(Vec::new())),
            data: DataKind::None,
            groups: no_groups,
        }
    }

    #[test]
    fn plugins_are_found_once_registered_and_replacing_keeps_old_ones_alive() {
        let reference = InstrumentRef {
            format: Format::Clap,
            id: "test.fake-synth".into(),
            version: "1".into(),
        };
        assert!(reference.descriptor().is_none(), "not registered yet");
        let first = register_external(Format::Clap, "test.fake-synth", fake("First"));
        assert!(std::ptr::eq(reference.descriptor().unwrap(), first));
        assert!(
            find_external(Format::Vst3, "test.fake-synth").is_none(),
            "formats are kept apart"
        );
        let second = register_external(Format::Clap, "test.fake-synth", fake("Second"));
        assert_eq!(reference.descriptor().unwrap().name, "Second");
        assert_eq!(first.name, "First", "the old one is still there");
        assert!(!std::ptr::eq(first, second));
        // The old name still reads.
        let old: InstrumentFormat = InstrumentFormat::BuiltIn;
        assert!(old.is_builtin() && !Format::Vst3.is_builtin());
    }

    #[test]
    fn every_builtin_is_findable_and_consistent() {
        assert!(find_builtin(DEFAULT_INSTRUMENT).is_some());
        for descriptor in builtins() {
            let mut instrument = (descriptor.create)();
            assert_eq!(instrument.params(), descriptor.params, "{}", descriptor.id);
            instrument.prepare(48_000.0, 256);
            let mut left = [1.0; 64];
            let mut right = [1.0; 64];
            instrument.process(&[], &mut left, &mut right);
            assert!(
                left.iter().chain(&right).all(|s| *s == 0.0),
                "{} should be silent with no notes",
                descriptor.id
            );
            let reference = InstrumentRef::builtin(descriptor.id);
            assert_eq!(reference.version, descriptor.version);
            assert!(reference.create().is_some());
            for envelope in descriptor.envelopes {
                for index in envelope.indices() {
                    assert!(
                        matches!(
                            descriptor.params.get(index),
                            Some(ParamDescriptor {
                                kind: ParameterKind::Float { .. },
                                ..
                            })
                        ),
                        "{}: envelope parameter {index} isn't a float parameter",
                        descriptor.id
                    );
                }
            }
        }
    }
}
