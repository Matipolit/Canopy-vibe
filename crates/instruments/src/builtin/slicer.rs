//! Slicer: plays a loop chopped into slices, one per key, like FL Studio's
//! Slicex. Made for drum breaks.
//!
//! # Data
//!
//! The host decodes the loop, matches it to the project's tempo and hands
//! it over as `Arc<SliceData>` through [`Instrument::set_data`]: the audio
//! at the session rate (time-stretched already in Stretch mode), where
//! each slice starts in it, and the speed to play it at (the Resample
//! speed; 1 when the audio is stretched or matching is off). Without data
//! the slicer is silent. The parameters Mode, Original Tempo and Follow
//! Tempo are the host's: they decide what data it builds, and the
//! instrument itself ignores them.
//!
//! # Playing
//!
//! Slice `k` plays on key `root + k`, from its start to the next slice's
//! (or the audio's end) through the amp envelope, fading out over the last
//! couple of milliseconds so the cut doesn't click. Its rate is the speed
//! times `2^((pitch + slice pitch + note bend + wheel) / 12)`. With Cut on
//! a new note fades out whatever was playing within 5 ms; off, up to
//! [`POLYPHONY`] slices ring over each other.
//!
//! # Meters
//!
//! | Slot | Meaning |
//! |---|---|
//! | [`METER_VOICES`] | voices sounding, including ones fading out |
//! | [`METER_SLICES`] | slices in the data; `0` when there is none |
//! | [`METER_PEAK`] | peak output level of the last block, linear |
//! | [`METER_PLAYING`] `..+3` | slices sounding, as bits: slice `k` is bit `k % 24` of slot `METER_PLAYING + k / 24` |
//! | [`METER_HEADS`] `..+VOICES` | where each voice is, as a share of the audio's length, `-1` for none |

use std::sync::Arc;

use dsp::{Adsr, AdsrDesc, Smoothed, StageDesc, db_to_gain};
use midi::{BEND_STEP_FRAMES, NoteEvent, NoteEventKind, Velocity};

use crate::envelope::{self, EnvelopeParams};
use crate::{
    BuiltInInstrumentDescriptor, DataKind, Instrument, InstrumentData, MAX_METERS, OFF_ON,
    ParamDescriptor, choice, float,
};

pub const DESCRIPTOR: BuiltInInstrumentDescriptor = BuiltInInstrumentDescriptor {
    id: "canopy.slicer",
    name: "Slicer",
    description: "Chops a loop into slices played from the keyboard, matched to the tempo",
    version: "1.0",
    author: "canopy",
    params: PARAMS,
    envelopes: &[ENVELOPE],
    create: || Box::new(Slicer::new()),
    data: DataKind::Slices,
    groups,
};

/// Most slices, as the project keeps them (`project::slicer::MAX_SLICES`).
pub const MAX_SLICES: usize = 64;

/// Slices that ring at once with Cut off.
pub const POLYPHONY: usize = 16;

/// Extra voices for cut and stolen slices to fade out in.
const FADE_VOICES: usize = 8;
pub const VOICES: usize = POLYPHONY + FADE_VOICES;

pub const METER_VOICES: usize = 0;
pub const METER_SLICES: usize = 1;
pub const METER_PEAK: usize = 2;
/// Three slots of 24 bits each: integers that size are exact in an `f32`.
pub const METER_PLAYING: usize = 3;
pub const METER_HEADS: usize = METER_PLAYING + 3;

/// How fast Cut and stealing fade a slice out.
const CUT_SECONDS: f32 = 0.005;

/// The fade at a slice's end, so stopping there doesn't click.
const END_FADE_SECONDS: f64 = 0.002;

/// Frames between updates of a voice's rate, level and pan.
const CONTROL_INTERVAL: u32 = 16;

const PARAM_RAMP_MS: f32 = 20.0;

pub const MODE: usize = 0;
pub const ORIGINAL_TEMPO: usize = 1;
pub const FOLLOW: usize = 2;
pub const ROOT: usize = 3;
pub const CUT: usize = 4;
pub const VOLUME: usize = 5;
pub const PITCH: usize = 6;
pub const ATTACK: usize = 7;
pub const RELEASE: usize = 10;
const ENVELOPE: EnvelopeParams = EnvelopeParams {
    name: "Amp",
    attack: ATTACK,
    decay: ATTACK + 1,
    sustain: ATTACK + 2,
    release: RELEASE,
    curves: None,
};
/// The first of each per-slice block of [`MAX_SLICES`] parameters: slice
/// `k`'s Reverse is `SLICE_REVERSE + k`, and so on.
pub const SLICE_REVERSE: usize = 11;
pub const SLICE_PITCH: usize = SLICE_REVERSE + MAX_SLICES;
pub const SLICE_LEVEL: usize = SLICE_PITCH + MAX_SLICES;
pub const SLICE_PAN: usize = SLICE_LEVEL + MAX_SLICES;
const COUNT: usize = SLICE_PAN + MAX_SLICES;

/// The Mode parameter's labels (see `project::slicer::Mode`).
pub const MODES: &[&str] = &["Resample", "Stretch", "Off"];

/// Key names, as the piano roll shows them (middle C is C3).
pub const KEY_NAMES: [&str; 128] = [
    "C-2", "C#-2", "D-2", "D#-2", "E-2", "F-2", "F#-2", "G-2", "G#-2", "A-2", "A#-2", "B-2", "C-1",
    "C#-1", "D-1", "D#-1", "E-1", "F-1", "F#-1", "G-1", "G#-1", "A-1", "A#-1", "B-1", "C0", "C#0",
    "D0", "D#0", "E0", "F0", "F#0", "G0", "G#0", "A0", "A#0", "B0", "C1", "C#1", "D1", "D#1", "E1",
    "F1", "F#1", "G1", "G#1", "A1", "A#1", "B1", "C2", "C#2", "D2", "D#2", "E2", "F2", "F#2", "G2",
    "G#2", "A2", "A#2", "B2", "C3", "C#3", "D3", "D#3", "E3", "F3", "F#3", "G3", "G#3", "A3",
    "A#3", "B3", "C4", "C#4", "D4", "D#4", "E4", "F4", "F#4", "G4", "G#4", "A4", "A#4", "B4", "C5",
    "C#5", "D5", "D#5", "E5", "F5", "F#5", "G5", "G#5", "A5", "A#5", "B5", "C6", "C#6", "D6",
    "D#6", "E6", "F6", "F#6", "G6", "G#6", "A6", "A#6", "B6", "C7", "C#7", "D7", "D#7", "E7", "F7",
    "F#7", "G7", "G#7", "A7", "A#7", "B7", "C8", "C#8", "D8", "D#8", "E8", "F8", "F#8", "G8",
];

const GLOBALS: [ParamDescriptor; SLICE_REVERSE] = [
    choice("mode", "Mode", MODES, 0),
    float(
        "original_tempo",
        "Original Tempo",
        40.0,
        300.0,
        120.0,
        "BPM",
        false,
    ),
    choice("follow", "Follow Tempo", OFF_ON, 1),
    choice("root", "Root Key", &KEY_NAMES, 60),
    choice("cut", "Cut", OFF_ON, 1),
    float("volume", "Volume", -60.0, 12.0, 0.0, "dB", false),
    float("pitch", "Pitch", -24.0, 24.0, 0.0, "st", false),
    envelope::attack("attack", 0.5),
    envelope::decay("decay", 300.0),
    envelope::sustain("sustain", 1.0),
    envelope::release("release", 40.0),
];

/// Per-slice parameter ids and names, numbered from 1 as the panel shows
/// slices.
macro_rules! per_slice {
    ($($n:literal)*) => {
        const REVERSE_IDS: [&str; MAX_SLICES] = [$(concat!("slice", $n, "_reverse")),*];
        const REVERSE_NAMES: [&str; MAX_SLICES] = [$(concat!("Slice ", $n, " Reverse")),*];
        const PITCH_IDS: [&str; MAX_SLICES] = [$(concat!("slice", $n, "_pitch")),*];
        const PITCH_NAMES: [&str; MAX_SLICES] = [$(concat!("Slice ", $n, " Pitch")),*];
        const LEVEL_IDS: [&str; MAX_SLICES] = [$(concat!("slice", $n, "_level")),*];
        const LEVEL_NAMES: [&str; MAX_SLICES] = [$(concat!("Slice ", $n, " Level")),*];
        const PAN_IDS: [&str; MAX_SLICES] = [$(concat!("slice", $n, "_pan")),*];
        const PAN_NAMES: [&str; MAX_SLICES] = [$(concat!("Slice ", $n, " Pan")),*];
    };
}

per_slice!(
    1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32
    33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63 64
);

/// The globals, then each per-slice block in turn. The engine addresses
/// parameters by index, so once shipped this list only grows at the end.
/// A static, so the descriptors exist once rather than being copied
/// wherever the list is used.
static ALL: [ParamDescriptor; COUNT] = {
    let mut all = [GLOBALS[0]; COUNT];
    let mut i = 0;
    while i < SLICE_REVERSE {
        all[i] = GLOBALS[i];
        i += 1;
    }
    let mut k = 0;
    while k < MAX_SLICES {
        all[SLICE_REVERSE + k] = choice(REVERSE_IDS[k], REVERSE_NAMES[k], OFF_ON, 0);
        all[SLICE_PITCH + k] = float(PITCH_IDS[k], PITCH_NAMES[k], -24.0, 24.0, 0.0, "st", false);
        all[SLICE_LEVEL + k] = float(LEVEL_IDS[k], LEVEL_NAMES[k], -60.0, 12.0, 0.0, "dB", false);
        all[SLICE_PAN + k] = float(PAN_IDS[k], PAN_NAMES[k], -100.0, 100.0, 0.0, "%", false);
        k += 1;
    }
    all
};

pub const PARAMS: &[ParamDescriptor] = &ALL;

/// Menus list the per-slice parameters in a submenu for each kind.
fn groups(index: usize) -> [&'static str; 2] {
    match index {
        i if i < SLICE_REVERSE => ["", ""],
        i if i < SLICE_PITCH => ["Slice Reverse", ""],
        i if i < SLICE_LEVEL => ["Slice Pitch", ""],
        i if i < SLICE_PAN => ["Slice Level", ""],
        _ => ["Slice Pan", ""],
    }
}

/// What the slicer plays: a loop, tempo-matched and at the session rate,
/// and where its slices start. Built by the host off the audio thread.
#[derive(Debug, Clone)]
pub struct SliceData {
    pub left: Arc<[f32]>,
    pub right: Arc<[f32]>,
    /// The rate `left` and `right` are at.
    pub sample_rate: f32,
    /// Each slice's first frame, sorted, at most [`MAX_SLICES`]; the last
    /// slice runs to the end. Empty plays the whole loop as one slice.
    pub starts: Box<[usize]>,
    /// How fast to play the audio: the Resample speed, or 1 for audio
    /// stretched to the tempo already.
    pub speed: f64,
}

impl SliceData {
    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    /// How many slices it has (one for none marked).
    pub fn slice_count(&self) -> usize {
        self.starts.len().clamp(1, MAX_SLICES)
    }

    /// Slice `k`'s frames, `None` past the last.
    pub fn slice(&self, k: usize) -> Option<std::ops::Range<usize>> {
        if k >= self.slice_count() {
            return None;
        }
        let frames = self.frames();
        let start = self.starts.get(k).copied().unwrap_or(0).min(frames);
        let end = self
            .starts
            .get(k + 1)
            .copied()
            .unwrap_or(frames)
            .min(frames);
        Some(start..end.max(start))
    }
}

/// One sounding slice.
#[derive(Debug, Clone)]
struct Voice {
    active: bool,
    key: u8,
    /// Key still down: a note-off will release it.
    held: bool,
    slice: usize,
    /// The slice's frames in the data.
    start: f64,
    end: f64,
    /// Where it reads, in frames.
    pos: f64,
    reverse: bool,
    env: Adsr,
    velocity: f32,
    /// The note's own bend in semitones, gliding to `bend_target` over a
    /// bend step.
    bend: f32,
    bend_target: f32,
    bend_per_frame: f32,
    /// Frames read per frame played.
    step: f64,
    /// Left and right gain (slice level and pan), ramped per control
    /// interval.
    gain: [f32; 2],
    gain_step: [f32; 2],
    until_control: u32,
    /// Fading out after a cut or a steal.
    killing: bool,
    kill_gain: f32,
    started: u64,
}

impl Voice {
    fn new() -> Voice {
        Voice {
            active: false,
            key: 0,
            held: false,
            slice: 0,
            start: 0.0,
            end: 0.0,
            pos: 0.0,
            reverse: false,
            env: Adsr::new(AdsrDesc::default(), 48_000.0),
            velocity: 0.0,
            bend: 0.0,
            bend_target: 0.0,
            bend_per_frame: 0.0,
            step: 1.0,
            gain: [0.0; 2],
            gain_step: [0.0; 2],
            until_control: 0,
            killing: false,
            kill_gain: 1.0,
            started: 0,
        }
    }

    fn level(&self) -> f32 {
        self.env.level() * self.kill_gain
    }
}

/// Plays [`SliceData`]: see the module's documentation.
pub struct Slicer {
    data: Option<Arc<SliceData>>,
    voices: Box<[Voice]>,
    rate: f32,
    /// Everything [`PARAMS`] holds, as set.
    values: [f32; COUNT],
    env: AdsrDesc,
    volume: Smoothed,
    /// The pitch wheel, in semitones.
    wheel: f32,
    notes_started: u64,
    peak: f32,
}

impl Slicer {
    pub fn new() -> Slicer {
        let mut slicer = Slicer {
            data: None,
            voices: vec![Voice::new(); VOICES].into_boxed_slice(),
            rate: 48_000.0,
            values: [0.0; COUNT],
            // Drawn straight in the panel, so played straight.
            env: AdsrDesc {
                attack: StageDesc::new(0.5, 0.0),
                decay: StageDesc::new(300.0, 0.0),
                sustain: 1.0,
                release: StageDesc::new(40.0, 0.0),
            },
            volume: Smoothed::new(1.0, PARAM_RAMP_MS),
            wheel: 0.0,
            notes_started: 0,
            peak: 0.0,
        };
        for (index, param) in PARAMS.iter().enumerate() {
            slicer.set_param(index, param.default_value());
        }
        slicer.reset();
        slicer
    }

    fn handle(&mut self, data: &SliceData, kind: NoteEventKind) {
        match kind {
            NoteEventKind::On { pitch, velocity } => self.note_on(data, pitch.get(), velocity),
            NoteEventKind::Off { pitch, .. } => {
                for voice in self.voices.iter_mut() {
                    if voice.active && voice.held && voice.key == pitch.get() {
                        voice.held = false;
                        voice.env.start_release();
                    }
                }
            }
            NoteEventKind::AllNotesOff => {
                for voice in self.voices.iter_mut().filter(|v| v.active) {
                    voice.held = false;
                    voice.env.start_release();
                }
            }
            NoteEventKind::NoteBend { pitch, cents } => {
                let target = f32::from(cents) / 100.0;
                for voice in self.voices.iter_mut() {
                    if voice.active && !voice.killing && voice.key == pitch.get() {
                        voice.bend_target = target;
                        voice.bend_per_frame = (target - voice.bend) / BEND_STEP_FRAMES as f32;
                    }
                }
            }
            NoteEventKind::PitchBend(value) => {
                self.wheel = NoteEventKind::bend_ratio(value).log2() * 12.0;
            }
            NoteEventKind::Control { .. } => {}
        }
    }

    fn note_on(&mut self, data: &SliceData, key: u8, velocity: Velocity) {
        let root = self.values[ROOT] as i32;
        let Ok(k) = usize::try_from(i32::from(key) - root) else {
            return;
        };
        let Some(frames) = data.slice(k) else {
            return;
        };
        if frames.is_empty() {
            return;
        }
        if self.values[CUT] >= 0.5 {
            for voice in self.voices.iter_mut().filter(|v| v.active) {
                voice.killing = true;
            }
        } else {
            let sounding = || {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.active && !v.killing)
            };
            if sounding().count() >= POLYPHONY
                && let Some(victim) =
                    super::voice_to_steal(sounding(), |v| !v.held, Voice::level, |v| v.started)
            {
                self.voices[victim].killing = true;
            }
        }
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                // Every voice is busy: the quietest of those fading out.
                self.voices
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        b.killing
                            .cmp(&a.killing)
                            .then(a.level().total_cmp(&b.level()))
                    })
                    .map_or(0, |(index, _)| index)
            });
        self.notes_started += 1;
        let reverse = self.values[SLICE_REVERSE + k] >= 0.5;
        let voice = &mut self.voices[slot];
        voice.active = true;
        voice.key = key;
        voice.held = true;
        voice.slice = k;
        voice.start = frames.start as f64;
        voice.end = frames.end as f64;
        voice.reverse = reverse;
        voice.pos = if reverse {
            voice.end - 1.0
        } else {
            voice.start
        };
        voice.env.reset();
        voice.env.set_desc(self.env);
        voice.env.start_attack();
        voice.velocity = velocity.as_f32();
        voice.bend = 0.0;
        voice.bend_target = 0.0;
        voice.bend_per_frame = 0.0;
        voice.until_control = 0;
        voice.killing = false;
        voice.kill_gain = 1.0;
        voice.started = self.notes_started;
        // Level and pan start where they are, without a ramp.
        voice.gain = slice_gains(&self.values, k);
        voice.gain_step = [0.0; 2];
    }

    /// Adds every sounding voice into `left` and `right`.
    fn render(&mut self, data: &SliceData, left: &mut [f32], right: &mut [f32]) {
        let kill_step = 1.0 / (CUT_SECONDS * self.rate).max(1.0);
        let end_fade = END_FADE_SECONDS * f64::from(self.rate);
        let base = data.speed * f64::from(data.sample_rate / self.rate);
        let (audio_l, audio_r) = (&data.left[..], &data.right[..]);
        let last = data.frames().saturating_sub(1);
        let pitch = self.values[PITCH] + self.wheel;
        for voice in self.voices.iter_mut().filter(|v| v.active) {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                if voice.until_control == 0 {
                    voice.until_control = CONTROL_INTERVAL;
                    let frames = CONTROL_INTERVAL as f32;
                    if voice.bend != voice.bend_target {
                        let moved = voice.bend + voice.bend_per_frame * frames;
                        let passed = (moved - voice.bend_target) * voice.bend_per_frame.signum();
                        voice.bend = if passed >= 0.0 {
                            voice.bend_target
                        } else {
                            moved
                        };
                    }
                    let semitones = pitch + self.values[SLICE_PITCH + voice.slice] + voice.bend;
                    voice.step = base * f64::from((semitones / 12.0).exp2());
                    let target = slice_gains(&self.values, voice.slice);
                    voice.gain_step = [
                        (target[0] - voice.gain[0]) / frames,
                        (target[1] - voice.gain[1]) / frames,
                    ];
                }
                voice.until_control -= 1;

                let remaining = if voice.reverse {
                    voice.pos - voice.start
                } else {
                    voice.end - voice.pos
                };
                if remaining <= 0.0 {
                    voice.active = false;
                    break;
                }
                let edge = (remaining / voice.step / end_fade).min(1.0) as f32;
                let (sample_l, sample_r) = read(audio_l, audio_r, last, voice.pos);
                let env = voice.env.next_sample();
                voice.gain[0] += voice.gain_step[0];
                voice.gain[1] += voice.gain_step[1];
                let gain = env * edge * voice.kill_gain * voice.velocity;
                *l += sample_l * gain * voice.gain[0];
                *r += sample_r * gain * voice.gain[1];
                if voice.reverse {
                    voice.pos -= voice.step;
                } else {
                    voice.pos += voice.step;
                }
                if voice.killing {
                    voice.kill_gain -= kill_step;
                    if voice.kill_gain <= 0.0 {
                        voice.active = false;
                        break;
                    }
                }
                if !voice.env.is_active() {
                    voice.active = false;
                    break;
                }
            }
        }
    }

    /// Sets one of the amp envelope's parameters, for new stages of every
    /// voice. Real-time safe, unlike [`EnvelopeParams::desc`].
    fn set_envelope(&mut self, index: usize, value: f32) {
        ENVELOPE.apply(index, value, &mut self.env);
        for voice in self.voices.iter_mut() {
            *voice.env.desc_mut() = self.env;
        }
    }
}

/// Slice `k`'s left and right gain: its level, panned as the Sampler
/// pans (both sides at unity in the middle, the far one silent at either
/// end).
fn slice_gains(values: &[f32; COUNT], k: usize) -> [f32; 2] {
    let level = db_to_gain(values[SLICE_LEVEL + k]);
    let angle = (values[SLICE_PAN + k] / 100.0 + 1.0) * std::f32::consts::FRAC_PI_4;
    let scale = std::f32::consts::SQRT_2;
    [
        level * (scale * angle.cos()).min(1.0),
        level * (scale * angle.sin()).min(1.0),
    ]
}

/// Both channels at fractional frame `pos`, by 4-point cubic
/// (Catmull-Rom) interpolation, the frames past either end held.
#[inline]
fn read(left: &[f32], right: &[f32], last: usize, pos: f64) -> (f32, f32) {
    let whole = (pos as i64).max(0);
    let t = (pos - whole as f64) as f32;
    let i = whole as usize;
    let at = |offset: isize| (i as isize + offset).clamp(0, last as isize) as usize;
    let taps = [at(-1), at(0), at(1), at(2)];
    let cubic = |x: &[f32]| dsp::catmull_rom(x[taps[0]], x[taps[1]], x[taps[2]], x[taps[3]], t);
    (cubic(left), cubic(right))
}

impl Default for Slicer {
    fn default() -> Self {
        Self::new()
    }
}

impl Instrument for Slicer {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.rate = sample_rate;
        for voice in self.voices.iter_mut() {
            voice.env = Adsr::new(self.env, sample_rate);
        }
        self.volume.prepare(sample_rate);
        self.reset();
    }

    fn reset(&mut self) {
        for voice in self.voices.iter_mut() {
            voice.active = false;
            voice.env.reset();
        }
        self.wheel = 0.0;
        self.notes_started = 0;
        self.volume.settle();
        self.peak = 0.0;
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        // Taken out and put back (no reference count change) so voices can
        // read it while `self` changes.
        let Some(data) = self.data.take() else {
            self.peak = 0.0;
            return;
        };
        if data.frames() > 0 {
            for (span, event) in crate::segments(events, left.len()) {
                self.render(&data, &mut left[span.clone()], &mut right[span]);
                if let Some(kind) = event {
                    self.handle(&data, kind);
                }
            }
        }
        self.data = Some(data);

        let mut peak = 0.0f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let gain = self.volume.tick();
            *l *= gain;
            *r *= gain;
            peak = peak.max(l.abs()).max(r.abs());
        }
        self.peak = peak;
    }

    fn params(&self) -> &'static [ParamDescriptor] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(value) = crate::checked_value(PARAMS, index, value) else {
            return;
        };
        self.values[index] = value;
        match index {
            VOLUME => self.volume.set_target(db_to_gain(value)),
            ATTACK..=RELEASE => self.set_envelope(index, value),
            _ => {}
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out.fill(0.0);
        let active = || self.voices.iter().filter(|v| v.active);
        out[METER_VOICES] = active().count() as f32;
        out[METER_SLICES] = self.data.as_ref().map_or(0.0, |d| d.slice_count() as f32);
        out[METER_PEAK] = self.peak;
        let mut playing = [0u32; 3];
        for voice in active().filter(|v| !v.killing) {
            playing[voice.slice / 24] |= 1 << (voice.slice % 24);
        }
        for (slot, bits) in out[METER_PLAYING..METER_HEADS].iter_mut().zip(playing) {
            *slot = bits as f32;
        }
        let frames = self.data.as_ref().map_or(0, |d| d.frames()).max(1) as f64;
        for (slot, voice) in out[METER_HEADS..METER_HEADS + VOICES]
            .iter_mut()
            .zip(self.voices.iter())
        {
            *slot = if voice.active && !voice.killing {
                (voice.pos / frames) as f32
            } else {
                -1.0
            };
        }
    }

    fn set_data(&mut self, data: InstrumentData) {
        self.data = data.downcast::<SliceData>().ok();
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_are_laid_out_in_blocks_with_unique_ids() {
        assert_eq!(PARAMS.len(), 11 + 4 * MAX_SLICES);
        assert_eq!(PARAMS[SLICE_REVERSE].id, "slice1_reverse");
        assert_eq!(PARAMS[SLICE_PITCH + 63].id, "slice64_pitch");
        assert_eq!(PARAMS[SLICE_LEVEL].name, "Slice 1 Level");
        assert_eq!(PARAMS[SLICE_PAN + 9].id, "slice10_pan");
        assert_eq!(PARAMS[ROOT].default_value(), 60.0);
        assert_eq!(KEY_NAMES[60], "C3");
        let mut ids: Vec<&str> = PARAMS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PARAMS.len());
        for (i, p) in PARAMS.iter().enumerate().skip(SLICE_REVERSE) {
            let [group, _] = groups(i);
            assert!(p.name.starts_with("Slice "), "{}", p.name);
            assert!(
                group.ends_with(p.name.rsplit(' ').next().unwrap()),
                "{group}"
            );
        }
    }
}
