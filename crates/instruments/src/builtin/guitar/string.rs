//! One guitar string: the note it plays, the finger on it, its bend, and
//! the voices that sound it.
//!
//! A string has two voices. Picking a string that is still ringing first
//! stops it (the pick touches it) and then sets it going again: the old
//! vibration fades in the second voice over a couple of milliseconds while
//! the new one starts in the first, so the cut is quick but never a click.
//! A third voice rings along with the other strings while this one is idle
//! (sympathetic resonance); it falls silent when the string is played.
//!
//! The string bends with the pitch wheel only while a finger holds its
//! note, as a guitarist bends one string and lets the others ring: a
//! released note keeps whatever bend it had.

use midi::Pitch;

use super::CONTROL_INTERVAL;
use super::excitation::Pluck;
use super::fretboard::OPEN_NOTES;
use super::pickup::Taps;
use super::voice::{NoteKind, NoteStart, Voice, VoiceControls};
use super::waveguide::LN_1000;

/// A released note is damped by the fretting finger lifting off (or the
/// other hand muting an open string): its fundamental fades in about this
/// long, and its treble much sooner, so the end is a soft thud rather than
/// a cut.
const RELEASE_T60: f32 = 0.25;
const RELEASE_TREBLE_T60: f32 = 0.04;
/// The finger takes a few milliseconds to come down on the string.
const RELEASE_RAMP_S: f32 = 0.008;

/// A ringing string picked again is stopped by the pick first.
const CHOKE_T60: f32 = 0.04;
const CHOKE_TREBLE_T60: f32 = 0.01;
const CHOKE_RAMP_S: f32 = 0.0015;

/// A note waiting to be plucked, later in a strum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Pending {
    /// The key that played it, to match its note-off.
    pub key: Pitch,
    /// The note that sounds (the key, moved by octaves into range).
    pub played: u8,
    pub velocity: f32,
    /// Samples until the pick reaches this string.
    pub wait: usize,
    /// Its key was let go before the pick got there: release right after.
    pub release: bool,
    pub kind: NoteKind,
    /// The fret the note is stopped at (for a harmonic, the fret pressed:
    /// 0 for a natural one).
    pub fret: i32,
    /// The partial a harmonic sounds, 1 for an ordinary note, and the fret
    /// it is touched over.
    pub partial: u8,
    pub touch: i32,
    /// 1 for a down-stroke, -1 for an up-stroke.
    pub stroke: f32,
}

/// What the UI shows for a string (see the meter slots in the module root).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StringMeter {
    pub note: f32,
    pub level: f32,
    pub fret: f32,
    pub held: f32,
    pub kind: f32,
}

/// What became of a note let go: for the instrument's release noise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Released {
    pub fret: i32,
    pub level: f32,
}

pub(super) struct GuitarString {
    voices: [Voice; 2],
    /// The voice playing the current note; the other may be fading.
    current: usize,
    sympathetic: Voice,
    key: Option<Pitch>,
    played: u8,
    fret: i32,
    touch: i32,
    held: bool,
    pending: Option<Pending>,
    /// Semitones the string is bent (wheel and vibrato), as of the last
    /// control interval.
    bend: f32,
    release_speed: f32,
    choke_speed: f32,
    /// Plucks so far, to vary each one.
    plucks: u32,
}

impl GuitarString {
    pub fn new(index: usize) -> Self {
        Self {
            voices: [Voice::new(), Voice::new()],
            current: 0,
            sympathetic: Voice::new(),
            key: None,
            played: OPEN_NOTES[index],
            fret: 0,
            touch: 0,
            held: false,
            pending: None,
            bend: 0.0,
            release_speed: 0.0,
            choke_speed: 0.0,
            plucks: 0,
        }
    }

    pub fn prepare(&mut self, sample_rate: f32, max_delay: f32) {
        for voice in &mut self.voices {
            voice.prepare(sample_rate, max_delay);
        }
        self.sympathetic.prepare(sample_rate, max_delay);
        let speed =
            |seconds: f32| 1.0 - (-(CONTROL_INTERVAL as f32) / (seconds * sample_rate)).exp();
        self.release_speed = speed(RELEASE_RAMP_S);
        self.choke_speed = speed(CHOKE_RAMP_S);
        self.silence();
    }

    /// Stops at once, forgetting any note.
    pub fn silence(&mut self) {
        for voice in &mut self.voices {
            voice.stop();
        }
        self.sympathetic.stop();
        self.key = None;
        self.held = false;
        self.pending = None;
        self.bend = 0.0;
        self.plucks = 0;
    }

    /// A note is sounding (not counting sympathetic ringing).
    pub fn is_active(&self) -> bool {
        self.voices.iter().any(Voice::is_active)
    }

    /// Sounding at all, sympathetic ringing included.
    pub fn is_ringing(&self) -> bool {
        self.is_active() || self.sympathetic.is_active()
    }

    /// The current note is sounding.
    fn sounding(&self) -> bool {
        self.voices[self.current].is_active()
    }

    /// Held by a finger and sounding: a candidate for a legato note.
    pub fn is_held(&self) -> bool {
        self.held && self.sounding()
    }

    pub fn played(&self) -> u8 {
        self.played
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Plucks so far, for varying the next.
    pub fn plucks(&self) -> u32 {
        self.plucks
    }

    pub fn view(&self) -> super::fretboard::StringView {
        use super::fretboard::StringView;
        match self.pending {
            Some(pending) => StringView {
                note: Some(pending.played),
                busy: true,
                ringing: false,
            },
            None => StringView {
                note: self.sounding().then_some(self.played),
                busy: self.is_held(),
                ringing: !self.held && self.sounding(),
            },
        }
    }

    /// Queues a pluck `pending.wait` samples from now, replacing any
    /// already queued (it never got to sound).
    pub fn schedule(&mut self, pending: Pending) {
        self.pending = Some(pending);
    }

    /// Samples until the queued pluck, if any.
    pub fn wait(&self) -> Option<usize> {
        self.pending.map(|p| p.wait)
    }

    /// The queued pluck, if it is due now.
    pub fn take_due(&mut self) -> Option<Pending> {
        self.pending.take_if(|p| p.wait == 0)
    }

    /// Moves time on by `samples` for the queued pluck.
    pub fn advance(&mut self, samples: usize) {
        if let Some(pending) = &mut self.pending {
            pending.wait = pending.wait.saturating_sub(samples);
        }
    }

    fn choke(voice: &mut Voice, speed: f32) {
        voice.damp(LN_1000 / CHOKE_T60, LN_1000 / CHOKE_TREBLE_T60, speed);
    }

    /// Plucks the note `pending` describes, as `note` and `pluck` say,
    /// stopping whatever the string was playing. A dead note is damped
    /// from the start by `dead` (nepers per second, low and high).
    pub fn pluck(
        &mut self,
        pending: &Pending,
        note: &NoteStart,
        pluck: &Pluck,
        controls: &VoiceControls,
        dead: Option<[f32; 2]>,
    ) {
        if self.sounding() {
            Self::choke(&mut self.voices[self.current], self.choke_speed);
            self.current ^= 1;
        }
        if self.sympathetic.is_active() {
            Self::choke(&mut self.sympathetic, self.choke_speed);
        }
        // A new note starts with the string's bend only if a finger holds
        // it bent already; a fresh pick starts from the wheel as it is.
        let voice = &mut self.voices[self.current];
        voice.start(note, pluck, controls);
        if let Some([low, high]) = dead {
            voice.damp(low, high, 1.0);
        }
        self.bend = controls.bend;
        self.key = Some(pending.key);
        self.played = pending.played;
        self.fret = pending.fret;
        self.touch = pending.touch;
        self.held = true;
        self.plucks = self.plucks.wrapping_add(1);
    }

    /// Moves the sounding note to `played` (at `hz`, fret `fret`) without
    /// picking again: a slide (`frets`), hammer-on or pull-off, over
    /// `intervals` control intervals, with `pluck` for the finger's own
    /// small pluck, if any.
    #[allow(clippy::too_many_arguments)]
    pub fn legato(
        &mut self,
        key: Pitch,
        played: u8,
        fret: i32,
        hz: f32,
        intervals: usize,
        frets: bool,
        taps: Taps,
        length: f32,
        pluck: Option<&Pluck>,
    ) {
        let voice = &mut self.voices[self.current];
        let from = f32::from(self.played) - f32::from(played) + voice.glide_offset();
        voice.glide_to(hz, from, intervals, frets, taps, length);
        if let Some(pluck) = pluck {
            voice.excite(pluck);
        }
        self.key = Some(key);
        self.played = played;
        self.fret = fret;
        self.touch = fret;
        self.held = true;
    }

    /// The pickups were switched: every sounding voice hears them where
    /// `taps` (from a vibrating length and a frequency) says.
    pub fn retap(&mut self, taps: impl Fn(f32, f32) -> Taps + Copy) {
        for voice in &mut self.voices {
            voice.retap(taps);
        }
        self.sympathetic.retap(taps);
    }

    /// The finger lets go of `key`: damp the note if it is this string's,
    /// or release it as soon as it is plucked if it is still on its way.
    pub fn note_off(&mut self, key: Pitch) -> Option<Released> {
        if let Some(pending) = &mut self.pending
            && pending.key == key
        {
            pending.release = true;
        }
        if self.held && self.key == Some(key) {
            self.release();
            let voice = &self.voices[self.current];
            return voice.is_active().then_some(Released {
                fret: self.fret,
                level: voice.level(),
            });
        }
        None
    }

    pub fn release(&mut self) {
        self.held = false;
        let speed = self.release_speed;
        self.voices[self.current].damp(LN_1000 / RELEASE_T60, LN_1000 / RELEASE_TREBLE_T60, speed);
    }

    /// The hands come off the strings: an idle string ringing along stops
    /// as a released note does.
    pub fn damp_sympathetic(&mut self) {
        if self.sympathetic.is_active() {
            let speed = self.release_speed;
            self.sympathetic
                .damp(LN_1000 / RELEASE_T60, LN_1000 / RELEASE_TREBLE_T60, speed);
        }
    }

    /// The hands are back on the neck: an idle string may ring along again.
    pub fn free_sympathetic(&mut self) {
        if self.sympathetic.is_active() {
            let speed = self.release_speed;
            self.sympathetic.damp(0.0, 0.0, speed);
        }
    }

    /// Releases a held note and forgets a queued one.
    pub fn release_all(&mut self) {
        self.pending = None;
        if self.held {
            self.release();
        }
    }

    /// Once per control interval: follow the shared controls, the wheel
    /// (`wheel`, semitones, if a finger holds the note), slides and damping.
    /// `armed` says other strings are sounding, so an idle one should ring
    /// along, as `open` describes it.
    pub fn control_tick(
        &mut self,
        controls: &VoiceControls,
        wheel: f32,
        armed: bool,
        open: &NoteStart,
    ) {
        if self.held {
            self.bend = wheel;
        }
        let mut own = *controls;
        own.bend = self.bend;
        for voice in &mut self.voices {
            voice.control_tick(&own);
        }
        let idle = !self.sounding() && self.pending.is_none();
        if armed && idle && !self.sympathetic.is_active() {
            let still = VoiceControls { bend: 0.0, ..own };
            self.sympathetic.start(open, &Pluck::NONE, &still);
        } else if self.sympathetic.is_active() {
            let still = VoiceControls { bend: 0.0, ..own };
            self.sympathetic.control_tick(&still);
        }
    }

    /// First pass over a span: plucks and vertical planes (see [`Voice`]).
    pub fn render_vertical(&mut self, len: usize, bridge: &mut [f32], vertical: &mut [f32]) {
        for voice in &mut self.voices {
            voice.render_vertical(len, bridge, vertical);
        }
        self.sympathetic.render_vertical(len, bridge, vertical);
    }

    /// Second pass: horizontal planes, driven by the bridge. The
    /// sympathetic voice keeps going while `armed`, however quiet.
    pub fn render_horizontal(
        &mut self,
        len: usize,
        coupling: &[f32],
        bridge: &mut [f32],
        silence: f32,
        armed: bool,
    ) {
        for voice in &mut self.voices {
            voice.render_horizontal(len, coupling, bridge, silence);
        }
        let sympathetic_silence = if armed { 0.0 } else { silence };
        self.sympathetic
            .render_horizontal(len, coupling, bridge, sympathetic_silence);
    }

    /// What the pickups hear of the last span, added to `out`.
    pub fn render_pickups(&mut self, len: usize, out: &mut [f32]) {
        for voice in &mut self.voices {
            voice.render_pickups(len, out);
        }
        self.sympathetic.render_pickups(len, out);
    }

    pub fn meter(&self) -> StringMeter {
        let voice = &self.voices[self.current];
        if !voice.is_active() {
            return StringMeter {
                note: -1.0,
                level: 0.0,
                fret: 0.0,
                held: 0.0,
                kind: 0.0,
            };
        }
        let fret = if voice.kind() == NoteKind::Harmonic {
            self.touch as f32
        } else {
            self.fret as f32 + voice.glide_offset()
        };
        StringMeter {
            note: f32::from(self.played),
            level: voice.level().min(1.0),
            fret,
            held: if self.held { 1.0 } else { 0.0 },
            kind: voice.kind() as u8 as f32,
        }
    }
}
