//! SFZ sampler: plays sforzando-style `.sfz` instruments.
//!
//! # Loading
//!
//! The host loads an instrument on a worker thread with [`load`], passing
//! its own audio decoder (this crate decodes nothing), then hands the
//! resulting [`SfzBank`] over as `Arc<SfzBank>` through
//! [`Instrument::set_data`] before the sampler goes to the audio thread.
//! Without a bank the sampler is silent. Keep a clone of the `Arc` for the
//! UI ([`SfzBank::regions`], [`SfzBank::warnings`]...), which also makes
//! sure the samples are freed off the audio thread.
//!
//! # Meters
//!
//! [`Instrument::meters`] fills these slots:
//!
//! | Slot | Meaning |
//! |---|---|
//! | [`METER_VOICES`] | voices sounding, including ones fading out |
//! | [`METER_LAST_REGION`] | index into [`SfzBank::regions`] of the region most recently started, `-1` for none |
//! | [`METER_LAST_KEY`] | key of the note that started it, `-1` for none |
//! | [`METER_LAST_VELOCITY`] | that note's velocity, `0..=127`, `-1` for none |
//! | [`METER_REGION_COUNT`] | regions in the bank; `0` when none is loaded |
//! | [`METER_PEAK`] | peak output level of the last block, linear |
//! | [`METER_KEYSWITCH`] | the active keyswitch key, `-1` for none |
//! | [`METER_RELEASING`] | voices in their release |
//! | [`METER_KEYS`] `..+6` | keys sounding, as bits: key `k` is bit `k % 24` of slot `METER_KEYS + k / 24` |
//! | [`METER_REGIONS`] `..MAX_METERS` | indices of up to 50 sounding regions, `-1` in unused slots |

mod bank;
mod parse;
mod region;
mod voice;

use crate::{choice, float};
use std::sync::Arc;

use dsp::{Smoothed, db_to_gain};
use midi::{NoteEvent, NoteEventKind, Velocity};

pub use bank::{Decoded, LoadError, RegionInfo, SfzBank, load, load_str};
pub use region::Trigger;

use region::{OffMode, Region};
use voice::{Control, NoteStart, Voice};

use crate::{
    BuiltInInstrumentDescriptor, DataKind, Instrument, InstrumentData, MAX_METERS, ParamDescriptor,
};

pub const DESCRIPTOR: BuiltInInstrumentDescriptor = BuiltInInstrumentDescriptor {
    id: "canopy.sampler",
    name: "SFZ Sampler",
    description: "Plays SFZ instruments",
    version: "1.0",
    author: "canopy",
    params: PARAMS,
    envelopes: &[],
    create: || Box::new(Sampler::new()),
    data: DataKind::Sfz,
    groups: crate::no_groups,
};

pub const METER_VOICES: usize = 0;
pub const METER_LAST_REGION: usize = 1;
pub const METER_LAST_KEY: usize = 2;
pub const METER_LAST_VELOCITY: usize = 3;
pub const METER_REGION_COUNT: usize = 4;
pub const METER_PEAK: usize = 5;
pub const METER_KEYSWITCH: usize = 6;
pub const METER_RELEASING: usize = 7;
/// Six slots of 24 bits each: integers that size are exact in an `f32`.
pub const METER_KEYS: usize = 8;
pub const METER_REGIONS: usize = METER_KEYS + 6;

/// Most voices that sound at once (the polyphony parameter's top).
pub const MAX_POLYPHONY: usize = 64;

/// Extra voices for stolen and choked notes to fade out in while the new
/// ones start, so stealing never clicks.
const FADE_VOICES: usize = 16;

/// Most regions one note can start; more are ignored.
const MAX_LAYERS: usize = 32;

/// How fast stolen, choked (`off_mode=fast`) and over-polyphony voices fade.
const FAST_OFF_SECONDS: f32 = 0.005;

/// Shortest amplitude release, so `ampeg_release=0` (the SFZ default)
/// doesn't click on note-off.
const MIN_RELEASE_SECONDS: f32 = 0.005;

const PARAM_RAMP_MS: f32 = 20.0;

/// Where the random sequence (`lorand`/`hirand`, `*_random`) begins after
/// `reset`, so a render from the top always sounds the same.
const RANDOM_SEED: u32 = 0x9E37_79B9;

const VOLUME: usize = 0;
const PAN: usize = 1;
const TUNE: usize = 2;
const VELOCITY: usize = 3;
const ATTACK: usize = 4;
const RELEASE: usize = 5;
const POLYPHONY: usize = 6;

const POLYPHONY_CHOICES: [usize; 9] = [1, 2, 4, 8, 16, 24, 32, 48, MAX_POLYPHONY];

/// The engine addresses parameters by index, so once shipped this list only
/// grows at the end.
const PARAMS: &[ParamDescriptor] = &[
    float("volume", "Volume", -60.0, 12.0, 0.0, "dB", false),
    float("pan", "Pan", -100.0, 100.0, 0.0, "%", false),
    float("tune", "Tune", -24.0, 24.0, 0.0, "st", false),
    // Scales each region's `amp_veltrack`: 0 % plays every note at full
    // level (velocity still picks layers), 200 % doubles the dynamics.
    float("velocity", "Velocity", 0.0, 200.0, 100.0, "%", false),
    // Added to every region's attack and release times.
    float("attack", "Attack", -2000.0, 2000.0, 0.0, "ms", false),
    float("release", "Release", -5000.0, 5000.0, 0.0, "ms", false),
    choice(
        "polyphony",
        "Polyphony",
        &["1", "2", "4", "8", "16", "24", "32", "48", "64"],
        POLYPHONY_CHOICES.len() - 1,
    ),
];

/// Which keys are down, and how they went down.
#[derive(Debug, Clone)]
struct Keys {
    down: [bool; 128],
    velocity: [Velocity; 128],
    /// The frame clock at each note-on, for `rt_decay`.
    since: [u64; 128],
}

impl Keys {
    const UP: Keys = Keys {
        down: [false; 128],
        velocity: [Velocity::MIN; 128],
        since: [0; 128],
    };
}

/// Plays an [`SfzBank`]: up to 64 voices (plus room for stolen ones to fade
/// out), each a region with its own envelopes, filter and LFOs.
///
/// A note-on starts every region its key and velocity select, subject to
/// round robin, random ranges, keyswitches and `first`/`legato` triggers;
/// a note-off releases them and starts the key's release triggers. Regions
/// with a `group` choke voices whose `off_by` names it. Past the polyphony,
/// the quietest released voice fades out, else the oldest.
pub struct Sampler {
    bank: Option<Arc<SfzBank>>,
    voices: Box<[Voice]>,
    rate: f32,
    /// Per region, its place in its round-robin cycle.
    sequence: Vec<u32>,
    keys: Keys,
    keyswitch: Option<u8>,
    /// Frames rendered since `reset`.
    clock: u64,
    notes_started: u64,
    seed: u32,
    control: Control,
    volume: Smoothed,
    pan_left: Smoothed,
    pan_right: Smoothed,
    velocity_scale: f32,
    attack_offset: f32,
    release_offset: f32,
    polyphony: usize,
    last_region: Option<usize>,
    last_key: Option<u8>,
    last_velocity: Velocity,
    peak: f32,
}

impl Sampler {
    pub fn new() -> Sampler {
        let mut sampler = Sampler {
            bank: None,
            voices: vec![Voice::new(); MAX_POLYPHONY + FADE_VOICES].into_boxed_slice(),
            rate: 48_000.0,
            sequence: Vec::new(),
            keys: Keys::UP,
            keyswitch: None,
            clock: 0,
            notes_started: 0,
            seed: RANDOM_SEED,
            control: Control::default(),
            volume: Smoothed::new(1.0, PARAM_RAMP_MS),
            pan_left: Smoothed::new(1.0, PARAM_RAMP_MS),
            pan_right: Smoothed::new(1.0, PARAM_RAMP_MS),
            velocity_scale: 1.0,
            attack_offset: 0.0,
            release_offset: 0.0,
            polyphony: MAX_POLYPHONY,
            last_region: None,
            last_key: None,
            last_velocity: Velocity::MIN,
            peak: 0.0,
        };
        for (index, param) in PARAMS.iter().enumerate() {
            sampler.set_param(index, param.default_value());
        }
        sampler.reset();
        sampler
    }

    fn handle(&mut self, bank: &SfzBank, kind: NoteEventKind) {
        match kind {
            NoteEventKind::On { pitch, velocity } => self.note_on(bank, pitch.get(), velocity),
            NoteEventKind::Off { pitch, .. } => self.note_off(bank, pitch.get()),
            NoteEventKind::AllNotesOff => {
                for key in 0..128u8 {
                    if self.keys.down[key as usize] {
                        self.note_off(bank, key);
                    }
                }
                let release_offset = self.release_offset;
                for voice in self.voices.iter_mut().filter(|v| v.active && v.key_held) {
                    voice.release(release_time(voice, release_offset));
                }
            }
            NoteEventKind::PitchBend(value) => self.control.bend = value,
            NoteEventKind::Control { .. } => {}
        }
    }

    fn note_on(&mut self, bank: &SfzBank, key: u8, velocity: Velocity) {
        let k = key as usize;
        let others_down = self
            .keys
            .down
            .iter()
            .enumerate()
            .any(|(other, down)| *down && other != k);
        self.keys.down[k] = true;
        self.keys.velocity[k] = velocity;
        self.keys.since[k] = self.clock;
        if bank.keyswitches[k] {
            self.keyswitch = Some(key);
        }

        let velocity7 = velocity.to_midi1();
        let random = self.random();
        let mut layers = [0; MAX_LAYERS];
        let mut count = 0;
        for &index in &bank.attack_regions[k] {
            let desc = &bank.regions[index].desc;
            let trigger_ok = match desc.trigger {
                Trigger::First => !others_down,
                Trigger::Legato => others_down,
                Trigger::Attack | Trigger::Release => true,
            };
            if trigger_ok && self.selects(index, desc, velocity7, random) && count < MAX_LAYERS {
                layers[count] = index;
                count += 1;
            }
        }
        let note = self.note_start(key, velocity, true, false);
        self.start_layers(bank, &layers[..count], note, |_| 0.0);
    }

    fn note_off(&mut self, bank: &SfzBank, key: u8) {
        let k = key as usize;
        let release_offset = self.release_offset;
        for voice in self
            .voices
            .iter_mut()
            .filter(|v| v.active && v.key_held && v.key == key)
        {
            if voice.one_shot {
                voice.key_held = false;
            } else {
                voice.release(release_time(voice, release_offset));
            }
        }
        if !std::mem::take(&mut self.keys.down[k]) {
            return;
        }

        // Release triggers, at the note-on's velocity.
        let velocity = self.keys.velocity[k];
        let velocity7 = velocity.to_midi1();
        let held_seconds = self.clock.saturating_sub(self.keys.since[k]) as f32 / self.rate;
        let random = self.random();
        let mut layers = [0; MAX_LAYERS];
        let mut count = 0;
        for &index in &bank.release_regions[k] {
            let desc = &bank.regions[index].desc;
            if self.selects(index, desc, velocity7, random) && count < MAX_LAYERS {
                layers[count] = index;
                count += 1;
            }
        }
        let note = self.note_start(key, velocity, false, true);
        self.start_layers(bank, &layers[..count], note, |desc| {
            -desc.rt_decay * held_seconds
        });
    }

    /// Whether region `index` plays for this note, apart from its key and
    /// trigger: velocity, keyswitches, round robin and random range. Moves
    /// the region's round robin on when everything else matches.
    fn selects(&mut self, index: usize, desc: &Region, velocity7: u8, random: f32) -> bool {
        if velocity7 < desc.lovel || velocity7 > desc.hivel {
            return false;
        }
        if desc.sw_last.is_some() && desc.sw_last != self.keyswitch {
            return false;
        }
        if desc.sw_down.is_some_and(|k| !self.keys.down[k as usize])
            || desc.sw_up.is_some_and(|k| self.keys.down[k as usize])
        {
            return false;
        }
        let Some(counter) = self.sequence.get_mut(index) else {
            return false;
        };
        let position = *counter;
        *counter = (position + 1) % desc.seq_length;
        if position + 1 != desc.seq_position {
            return false;
        }
        random >= desc.lorand && (random < desc.hirand || desc.hirand >= 1.0)
    }

    /// The parts of a voice's start that are the same for all its layers.
    fn note_start(
        &self,
        key: u8,
        velocity: Velocity,
        held: bool,
        release_trigger: bool,
    ) -> NoteStart {
        NoteStart {
            key,
            velocity: velocity.as_f32(),
            velocity7: velocity.to_midi1(),
            velocity_scale: self.velocity_scale,
            attack_offset: self.attack_offset,
            gain_db: 0.0,
            held,
            release_trigger,
            random: [0.0; 4],
            started: 0,
        }
    }

    /// Starts `layers` for one note. Chokes come first, so a note never
    /// chokes its own layers.
    fn start_layers(
        &mut self,
        bank: &SfzBank,
        layers: &[usize],
        mut note: NoteStart,
        gain_db: impl Fn(&Region) -> f32,
    ) {
        if layers.is_empty() {
            return;
        }
        self.notes_started += 1;
        note.started = self.notes_started;
        let release_offset = self.release_offset;
        for &index in layers {
            let group = bank.regions[index].desc.group;
            for voice in self
                .voices
                .iter_mut()
                .filter(|v| v.active && !v.killing && v.off_by == Some(group))
            {
                let seconds = match voice.off_mode {
                    OffMode::Fast => FAST_OFF_SECONDS,
                    OffMode::Normal => release_time(voice, release_offset),
                    OffMode::Time => voice.off_time.max(MIN_RELEASE_SECONDS),
                };
                voice.kill(seconds);
            }
        }
        for &index in layers {
            let region = &bank.regions[index];
            let desc = &region.desc;
            note.gain_db = gain_db(desc);
            note.random = [self.random(), self.random(), self.random(), self.random()];
            if let Some(limit) = desc.note_polyphony {
                self.limit(limit, |v| v.key == note.key && v.group == desc.group);
            }
            if let Some(limit) = desc.polyphony {
                self.limit(limit, |v| v.group == desc.group);
            }
            self.limit(self.polyphony, |_| true);
            let slot = self.free_voice();
            let voice = &mut self.voices[slot];
            let sample = &bank.samples[region.sample];
            if voice.start(&note, index, region, sample, &self.control) {
                if note.release_trigger && region.loop_mode == region::LoopMode::Continuous {
                    // Nothing would ever release it.
                    voice.release(release_time(voice, release_offset));
                }
                self.last_region = Some(index);
                self.last_key = Some(note.key);
                self.last_velocity = Velocity::from_midi1(note.velocity7);
            }
        }
    }

    /// Fades out voices matching `matches` until fewer than `limit` remain
    /// (not counting ones already fading).
    fn limit(&mut self, limit: usize, matches: impl Fn(&Voice) -> bool) {
        loop {
            let candidates = || {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.active && !v.killing && matches(v))
            };
            if candidates().count() < limit.max(1) {
                return;
            }
            let victim =
                super::voice_to_steal(candidates(), |v| !v.key_held, Voice::level, |v| v.started);
            let Some(victim) = victim else {
                return;
            };
            self.voices[victim].kill(FAST_OFF_SECONDS);
        }
    }

    /// An idle voice, else the quietest one fading out.
    fn free_voice(&self) -> usize {
        if let Some(index) = self.voices.iter().position(|v| !v.active) {
            return index;
        }
        self.voices
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                b.killing
                    .cmp(&a.killing)
                    .then(a.level().total_cmp(&b.level()))
            })
            .map_or(0, |(index, _)| index)
    }

    /// Xorshift, `0..1`.
    fn random(&mut self) -> f32 {
        dsp::noise::unit(dsp::xorshift32(&mut self.seed))
    }

    /// Adds every sounding voice into `left` and `right`.
    fn render(&mut self, bank: &SfzBank, left: &mut [f32], right: &mut [f32]) {
        self.clock += left.len() as u64;
        for voice in self.voices.iter_mut().filter(|v| v.active) {
            let region = &bank.regions[voice.region];
            let sample = &bank.samples[region.sample];
            voice.render(region, sample, &self.control, left, right);
        }
    }

    fn set_pan(&mut self, pan: f32) {
        let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        let scale = std::f32::consts::SQRT_2;
        self.pan_left.set_target((scale * angle.cos()).min(1.0));
        self.pan_right.set_target((scale * angle.sin()).min(1.0));
    }
}

/// A voice's amplitude release with the release parameter's offset added.
fn release_time(voice: &Voice, offset: f32) -> f32 {
    (voice.release_seconds + offset).max(MIN_RELEASE_SECONDS)
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Instrument for Sampler {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.rate = sample_rate;
        for voice in self.voices.iter_mut() {
            voice.prepare(sample_rate);
        }
        for smoothed in [&mut self.volume, &mut self.pan_left, &mut self.pan_right] {
            smoothed.prepare(sample_rate);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for voice in self.voices.iter_mut() {
            voice.active = false;
        }
        self.keys = Keys::UP;
        self.sequence.fill(0);
        self.keyswitch = self.bank.as_ref().and_then(|b| b.default_keyswitch);
        self.clock = 0;
        self.notes_started = 0;
        self.seed = RANDOM_SEED;
        self.control.bend = 0;
        for smoothed in [&mut self.volume, &mut self.pan_left, &mut self.pan_right] {
            smoothed.settle();
        }
        self.last_region = None;
        self.last_key = None;
        self.last_velocity = Velocity::MIN;
        self.peak = 0.0;
    }

    fn process(&mut self, events: &[NoteEvent], left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        let Some(bank) = self.bank.take() else {
            self.peak = 0.0;
            return;
        };
        // Taken out and put back (no reference count change) so voices can
        // borrow it while `self` changes.
        for (span, event) in crate::segments(events, left.len()) {
            self.render(&bank, &mut left[span.clone()], &mut right[span]);
            if let Some(kind) = event {
                self.handle(&bank, kind);
            }
        }
        self.bank = Some(bank);

        let mut peak = 0.0f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let gain = self.volume.tick();
            *l *= gain * self.pan_left.tick();
            *r *= gain * self.pan_right.tick();
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
        match index {
            VOLUME => self.volume.set_target(db_to_gain(value)),
            PAN => self.set_pan(value / 100.0),
            TUNE => self.control.tune_cents = value * 100.0,
            VELOCITY => self.velocity_scale = value / 100.0,
            ATTACK => self.attack_offset = value / 1000.0,
            RELEASE => self.release_offset = value / 1000.0,
            POLYPHONY => {
                self.polyphony = POLYPHONY_CHOICES[value as usize];
                self.limit(self.polyphony + 1, |_| true);
            }
            _ => {}
        }
    }

    fn meters(&self, out: &mut [f32; MAX_METERS]) {
        out.fill(0.0);
        let active = || self.voices.iter().filter(|v| v.active);
        out[METER_VOICES] = active().count() as f32;
        out[METER_LAST_REGION] = self.last_region.map_or(-1.0, |r| r as f32);
        out[METER_LAST_KEY] = self.last_key.map_or(-1.0, f32::from);
        out[METER_LAST_VELOCITY] = self
            .last_key
            .map_or(-1.0, |_| f32::from(self.last_velocity.to_midi1()));
        out[METER_REGION_COUNT] = self.bank.as_ref().map_or(0.0, |b| b.regions.len() as f32);
        out[METER_PEAK] = self.peak;
        out[METER_KEYSWITCH] = self.keyswitch.map_or(-1.0, f32::from);
        out[METER_RELEASING] = active().filter(|v| v.is_releasing()).count() as f32;

        let mut keys = [0u32; 6];
        for voice in active() {
            keys[voice.key as usize / 24] |= 1 << (voice.key % 24);
        }
        for (slot, bits) in out[METER_KEYS..METER_REGIONS].iter_mut().zip(keys) {
            *slot = bits as f32;
        }

        let regions = &mut out[METER_REGIONS..];
        regions.fill(-1.0);
        let mut count = 0;
        for voice in active() {
            let index = voice.region as f32;
            if count < regions.len() && !regions[..count].contains(&index) {
                regions[count] = index;
                count += 1;
            }
        }
    }

    fn set_data(&mut self, data: InstrumentData) {
        self.bank = data.downcast::<SfzBank>().ok();
        let regions = self.bank.as_ref().map_or(0, |b| b.regions.len());
        self.sequence = vec![0; regions];
        self.reset();
    }
}
