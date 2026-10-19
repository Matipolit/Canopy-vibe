//! One note on one string: its two polarizations, the pluck that set them
//! going, and everything that moves its pitch or damps it while it rings.
//!
//! A real string vibrates in two planes at once: towards the top (vertical)
//! and along it (horizontal). The bridge gives way more to the vertical
//! motion, so that plane hands its energy to the body faster: it is loud at
//! first and dies sooner, and the horizontal plane carries the long tail.
//! Heard together they make the two-stage decay of a plucked string, and
//! since the bridge also tunes the two planes slightly apart, their
//! partials beat (Weinreich 1977; Karjalainen, Välimäki & Tolonen 1998).
//! Each plane is its own loop ([`Loop`]); the pluck divides between them by
//! its angle to the top, and the horizontal loop also hears the bridge,
//! which is how a string picks up its neighbours' vibrations (sympathetic
//! resonance) and part of its own vertical motion. Nothing feeds back from
//! the horizontal planes into the vertical ones, so the coupled strings are
//! stable whatever the coupling.
//!
//! A hard pluck also stretches the string, raising its tension and its
//! pitch; as the vibration dies the pitch falls back by a few cents
//! (Tolonen, Välimäki & Karjalainen 2000). The loops follow the level.

use super::CONTROL_INTERVAL;
use super::excitation::{Excitation, Pluck};
use super::pickup::Taps;
use super::waveguide::{LN_1000, Loop, MIN_DELAY};

/// Longest stretch processed at once: one control interval.
pub(super) const SPAN: usize = CONTROL_INTERVAL;

/// Where the strings' losses turn from the fundamental's towards the
/// treble's: measured decay rates grow from a few hundred hertz and level
/// off above a couple of kilohertz.
const LOSS_CORNER_HZ: f32 = 900.0;

/// How long the level follower takes to fall by a factor of e after a peak:
/// longer than a low E's period, so it reads the envelope, not the wave.
const LEVEL_RELEASE_S: f32 = 0.05;

/// The fundamental itself rings this much longer than the sustain setting:
/// its next few partials die a little sooner, and what the ear (and the
/// setting) calls the note's sustain is the lot of them.
const FUNDAMENTAL_SUSTAIN: f32 = 1.25;

/// Cents to a frequency ratio, for the small ratios of tension and detuning.
const CENTS: f32 = std::f32::consts::LN_2 / 1200.0;

/// What kind of note a voice plays, as the meters show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Picked = 0,
    PalmMuted = 1,
    Harmonic = 2,
    Dead = 3,
    /// No note: an idle string ringing along with the others.
    Sympathetic = 4,
}

/// Settings for one control interval, the same for every voice of a string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct VoiceControls {
    pub sample_rate: f32,
    /// Seconds for the open A string's fundamental (in its long-lasting
    /// horizontal plane) to fall by 60 dB.
    pub sustain: f32,
    /// The tone knob: how much longer than the string's own the treble
    /// rings.
    pub treble_scale: f32,
    /// Extra damping from the palm resting on the strings, in nepers per
    /// second, at the fundamental and in the treble.
    pub mute_low: f32,
    pub mute_high: f32,
    /// How much faster the vertical plane loses its energy, at the
    /// fundamental and in the treble.
    pub vertical_loss: [f32; 2],
    /// The string's bend and vibrato, in semitones.
    pub bend: f32,
    /// Something that sets the losses moved: refit the loss filters.
    pub losses_moved: bool,
}

/// Everything about a new note that its string and the instrument decide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct NoteStart {
    /// The fretted note's frequency, sharpened by the fretting and
    /// detuned a little by the hand.
    pub hz: f32,
    /// The loop plays this partial of it: 1, or 2 and up for a harmonic.
    pub partial: f32,
    /// The horizontal plane's frequency over the vertical's; the two sit
    /// either side of the note.
    pub split: f32,
    /// The fundamental's decay time over the sustain setting.
    pub decay: f32,
    /// The string's treble decay time over its fundamental's.
    pub treble: f32,
    /// Cents a full-level vibration raises the pitch.
    pub tension: f32,
    /// Stiffness of the vertical and the horizontal loop (see the
    /// waveguide's `dispersion_coefficient`). The horizontal one is a
    /// little less stiff, so its partials sit a nearly constant number of
    /// hertz above the vertical one's rather than a constant ratio: a
    /// bridge shifts each mode by about the same few hundredths of a hertz
    /// (Gough 1981).
    pub dispersion: [f32; 2],
    /// How the pluck divides between the vertical and horizontal planes.
    pub shares: [f32; 2],
    /// How much of the horizontal plane is heard.
    pub horizontal_gain: f32,
    pub kind: NoteKind,
    /// Where the pickups listen (electric only).
    pub taps: Taps,
    /// The vibrating length of the stopped string, metres.
    pub length: f32,
}

/// A pitch change on a sounding string: a slide, hammer-on or pull-off.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Glide {
    /// Semitones from the note, moving linearly to 0.
    position: f32,
    /// Semitones per control interval.
    step: f32,
    /// A finger sliding along the neck changes pitch a fret at a time.
    frets: bool,
    /// What the string plays, in semitones from the note.
    offset: f32,
}

/// How quickly a slide settles on each fret, per control interval: about
/// 3 ms, quick enough to hear the frets, slow enough not to click.
const FRET_SETTLE: f32 = 0.1;

impl Glide {
    /// Advances one control interval; returns whether the pitch moved.
    fn tick(&mut self) -> bool {
        if self.position == 0.0 && self.offset == 0.0 {
            return false;
        }
        self.position = if self.position.abs() <= self.step {
            0.0
        } else {
            self.position - self.step.copysign(self.position)
        };
        if self.frets {
            let fret = self.position.round();
            self.offset += (fret - self.offset) * FRET_SETTLE;
            if self.position == 0.0 && self.offset.abs() < 1e-3 {
                self.offset = 0.0;
            }
        } else {
            self.offset = self.position;
        }
        true
    }
}

/// Damping on top of the string's own: a finger or the pick resting on it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Damping {
    /// Nepers per second at the fundamental and in the treble.
    low: f32,
    high: f32,
    target_low: f32,
    target_high: f32,
    /// Share of the way to the target covered per control interval.
    speed: f32,
}

impl Damping {
    /// Advances one control interval; returns whether the damping changed.
    fn tick(&mut self) -> bool {
        if self.low == self.target_low && self.high == self.target_high {
            return false;
        }
        self.low += (self.target_low - self.low) * self.speed;
        self.high += (self.target_high - self.high) * self.speed;
        // Snap once within 0.1%, so the loop stops being retuned.
        if (self.target_low - self.low).abs() <= 1e-3 * self.target_low.abs() + 1e-3 {
            self.low = self.target_low;
        }
        if (self.target_high - self.high).abs() <= 1e-3 * self.target_high.abs() + 1e-3 {
            self.high = self.target_high;
        }
        true
    }
}

pub(super) struct Voice {
    /// Vertical, then horizontal.
    loops: [Loop; 2],
    excitation: Excitation,
    exciting: bool,
    shares: [f32; 2],
    horizontal_gain: f32,
    /// This span's excitation, kept from the vertical pass for the
    /// horizontal one, and the vertical plane's output.
    excitation_span: [f32; SPAN],
    vertical_span: [f32; SPAN],
    active: bool,
    /// Peak follower on the output, for meters and to notice silence.
    level: f32,
    level_decay: f32,
    /// The highest level so far, for the tension's pitch rise.
    peak: f32,
    kind: NoteKind,
    hz: f32,
    partial: f32,
    split: f32,
    decay: f32,
    treble: f32,
    tension: f32,
    glide: Glide,
    damping: Damping,
    /// The string's bend, semitones.
    bend: f32,
    /// The pitch the loops were last moved to, and last fully tuned for.
    now_hz: f32,
    tuned_hz: f32,
    /// The damping jumped: refit the loss filters at the next control
    /// interval.
    retune: bool,
    taps: Taps,
    /// The vibrating length of the stopped string, metres, for moving the
    /// pickups.
    length: f32,
    aperture: f32,
    aperture_state: f32,
    sample_rate: f32,
}

impl Voice {
    pub fn new() -> Self {
        Self {
            loops: [Loop::new(), Loop::new()],
            excitation: Excitation::default(),
            exciting: false,
            shares: [0.0; 2],
            horizontal_gain: 0.0,
            excitation_span: [0.0; SPAN],
            vertical_span: [0.0; SPAN],
            active: false,
            level: 0.0,
            level_decay: 0.0,
            peak: 0.0,
            kind: NoteKind::Picked,
            hz: 440.0,
            partial: 1.0,
            split: 1.0,
            decay: 1.0,
            treble: 1.0,
            tension: 0.0,
            glide: Glide::default(),
            damping: Damping::default(),
            bend: 0.0,
            now_hz: 440.0,
            tuned_hz: 440.0,
            retune: false,
            taps: Taps::default(),
            length: 0.65,
            aperture: 0.0,
            aperture_state: 0.0,
            sample_rate: 48_000.0,
        }
    }

    /// Allocates lines long enough for `max_delay` samples.
    pub fn prepare(&mut self, sample_rate: f32, max_delay: f32) {
        self.sample_rate = sample_rate;
        for l in &mut self.loops {
            l.prepare(max_delay);
        }
        self.level_decay = (-1.0 / (LEVEL_RELEASE_S * sample_rate)).exp();
        self.stop();
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn kind(&self) -> NoteKind {
        self.kind
    }

    /// Peak output level, decaying.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Semitones the string is from its note, mid-slide.
    pub fn glide_offset(&self) -> f32 {
        self.glide.offset
    }

    /// Silent at once. The lines are cleared when the next note starts.
    pub fn stop(&mut self) {
        self.active = false;
        self.exciting = false;
        self.level = 0.0;
        self.peak = 0.0;
        self.aperture_state = 0.0;
    }

    /// Starts a note from a still string: tuned at once, then plucked.
    pub fn start(&mut self, note: &NoteStart, pluck: &Pluck, controls: &VoiceControls) {
        for (l, dispersion) in self.loops.iter_mut().zip(note.dispersion) {
            l.clear();
            l.set_dispersion(dispersion);
        }
        self.hz = note.hz;
        self.partial = note.partial;
        self.split = note.split;
        self.decay = note.decay;
        self.treble = note.treble;
        self.tension = note.tension;
        self.shares = note.shares;
        self.horizontal_gain = note.horizontal_gain;
        self.kind = note.kind;
        self.taps = note.taps;
        self.length = note.length;
        self.aperture = super::pickup::one_pole(note.taps.aperture_hz, controls.sample_rate);
        self.aperture_state = 0.0;
        self.glide = Glide::default();
        self.damping = Damping::default();
        self.bend = controls.bend;
        self.level = 0.0;
        self.peak = 0.0;
        self.active = true;
        self.tune(controls, 0);
        self.excite(pluck);
    }

    /// Adds a pluck to whatever the string is doing.
    pub fn excite(&mut self, pluck: &Pluck) {
        self.excitation = Excitation::new(pluck);
        self.exciting = pluck.amplitude != 0.0;
    }

    /// Moves the note to `hz` (keeping the rest of how it sounds), gliding
    /// there from `from` semitones away over `intervals` control
    /// intervals, a fret at a time if `frets`; `taps` are where the pickups
    /// now are along the shorter or longer string.
    pub fn glide_to(
        &mut self,
        hz: f32,
        from: f32,
        intervals: usize,
        frets: bool,
        taps: Taps,
        length: f32,
    ) {
        self.hz = hz;
        self.taps = taps;
        self.length = length;
        self.aperture = super::pickup::one_pole(taps.aperture_hz, self.sample_rate);
        self.glide = Glide {
            position: from,
            step: from.abs() / intervals.max(1) as f32,
            frets,
            offset: from,
        };
    }

    /// Moves the pickups (switched while the note rings): `taps` says
    /// where they are for a string `length` metres long at `hz`.
    pub fn retap(&mut self, taps: impl Fn(f32, f32) -> Taps) {
        if self.active {
            self.taps = taps(self.length, self.hz);
            self.aperture = super::pickup::one_pole(self.taps.aperture_hz, self.sample_rate);
        }
    }

    /// Damps the string towards `low` and `high` extra nepers per second,
    /// covering `speed` of the way each control interval (1 at once).
    pub fn damp(&mut self, low: f32, high: f32, speed: f32) {
        self.damping.target_low = low;
        self.damping.target_high = high;
        self.damping.speed = speed;
        if speed >= 1.0 {
            self.damping.low = low;
            self.damping.high = high;
            self.retune = true;
        }
    }

    /// The pitch now: the note with its partial, the string's bend, any
    /// slide, and the tension of a loud vibration.
    fn pitch(&self) -> f32 {
        let semitones = self.bend + self.glide.offset;
        let bent = if semitones == 0.0 {
            1.0
        } else {
            (semitones / 12.0).exp2()
        };
        let stretch = if self.peak > 1e-9 {
            let share = (self.level / self.peak).min(1.0);
            1.0 + self.tension * share * share * CENTS
        } else {
            1.0 + self.tension * CENTS
        };
        (self.hz * self.partial * bent * stretch).min(self.sample_rate / (MIN_DELAY + 2.0))
    }

    /// The losses, in nepers per second at the fundamental and at the
    /// treble, of the vertical and the horizontal plane.
    fn losses(&self, controls: &VoiceControls) -> [[f32; 2]; 2] {
        let t60 = (controls.sustain * self.decay * FUNDAMENTAL_SUSTAIN).max(0.01);
        let treble_t60 = (t60 * self.treble * controls.treble_scale).clamp(0.005, 0.95 * t60);
        let (low, high) = (LN_1000 / t60, LN_1000 / treble_t60);
        let extra_low = controls.mute_low + self.damping.low;
        let extra_high = controls.mute_high + self.damping.high;
        let [vertical_low, vertical_high] = controls.vertical_loss;
        [
            [
                low * vertical_low + extra_low,
                high * vertical_high + extra_high,
            ],
            [low + extra_low, high + extra_high],
        ]
    }

    /// Fits both loops' filters and delays to the pitch and losses now,
    /// ramping the delay over `ramp` samples (or at once if 0).
    fn tune(&mut self, controls: &VoiceControls, ramp: usize) {
        let rate = controls.sample_rate;
        let hz = self.pitch();
        let losses = self.losses(controls);
        let corner = std::f32::consts::TAU * LOSS_CORNER_HZ.max(1.5 * hz) / rate;
        // The planes sit half the split either side of the note, so it is
        // in tune whichever is louder.
        let half = self.split.sqrt();
        for (plane, (l, [low, high])) in self.loops.iter_mut().zip(losses).enumerate() {
            let hz = if plane == 1 { hz * half } else { hz / half };
            l.tune(
                rate,
                hz,
                (-low / hz).exp(),
                corner,
                (-high / hz).exp(),
                ramp,
            );
        }
        self.now_hz = hz;
        self.tuned_hz = hz;
    }

    /// Once per control interval: advance slides and damping, follow the
    /// bend, the tension and the shared controls.
    pub fn control_tick(&mut self, controls: &VoiceControls) {
        if !self.active {
            return;
        }
        self.bend = controls.bend;
        self.glide.tick();
        let damped = self.damping.tick();
        let hz = self.pitch();
        let still = (hz / self.now_hz - 1.0).abs() < 1e-4;
        let drifted = (hz / self.tuned_hz - 1.0).abs() > 4e-3;
        if damped || self.retune || controls.losses_moved || (still && drifted) {
            self.retune = false;
            self.tune(controls, CONTROL_INTERVAL);
        } else if hz != self.now_hz {
            let rate = controls.sample_rate;
            let half = self.split.sqrt();
            self.loops[0].retarget(rate, hz / half, CONTROL_INTERVAL);
            self.loops[1].retarget(rate, hz * half, CONTROL_INTERVAL);
            self.now_hz = hz;
        } else {
            for l in &mut self.loops {
                l.hold();
            }
        }
    }

    /// First pass over `len` samples: the pluck and the vertical plane.
    /// Adds what is heard to `bridge`, and the vertical plane alone (which
    /// drives the other strings) to `vertical`.
    pub fn render_vertical(&mut self, len: usize, bridge: &mut [f32], vertical: &mut [f32]) {
        if !self.active {
            return;
        }
        let mut direct = [0.0f32; SPAN];
        if self.exciting {
            for (s, d) in self.excitation_span[..len]
                .iter_mut()
                .zip(&mut direct[..len])
            {
                (*s, *d) = self.excitation.next();
            }
            self.exciting = !self.excitation.finished();
        } else {
            self.excitation_span[..len].fill(0.0);
        }
        if self.kind == NoteKind::Sympathetic {
            self.vertical_span[..len].fill(0.0);
            return;
        }
        let mut input = [0.0f32; SPAN];
        for (i, s) in input[..len].iter_mut().zip(&self.excitation_span[..len]) {
            *i = s * self.shares[0];
        }
        self.loops[0].run(&input[..len], &mut self.vertical_span[..len]);
        for i in 0..len {
            bridge[i] += self.vertical_span[i] + direct[i];
            vertical[i] += self.vertical_span[i];
        }
    }

    /// Second pass: the horizontal plane, driven by its share of the pluck
    /// and by the bridge (`coupling`). Adds what is heard to `bridge`, and
    /// stops once quieter than `silence`.
    pub fn render_horizontal(
        &mut self,
        len: usize,
        coupling: &[f32],
        bridge: &mut [f32],
        silence: f32,
    ) {
        if !self.active {
            return;
        }
        let mut input = [0.0f32; SPAN];
        for ((i, s), c) in input[..len]
            .iter_mut()
            .zip(&self.excitation_span[..len])
            .zip(coupling)
        {
            *i = s * self.shares[1] + c;
        }
        let mut horizontal = [0.0f32; SPAN];
        self.loops[1].run(&input[..len], &mut horizontal[..len]);
        let mut peak = self.level;
        let decay = self.level_decay;
        for i in 0..len {
            let h = horizontal[i] * self.horizontal_gain;
            bridge[i] += h;
            let magnitude = (self.vertical_span[i] + h).abs();
            peak = if magnitude > peak {
                magnitude
            } else {
                peak * decay
            };
        }
        self.level = peak;
        self.peak = self.peak.max(peak);
        if !self.exciting && self.level < silence {
            self.stop();
        }
    }

    /// What the pickups hear of the last span, added to `out`.
    pub fn render_pickups(&mut self, len: usize, out: &mut [f32]) {
        if !self.active || self.taps.count == 0 {
            return;
        }
        let mut heard = [0.0f32; SPAN];
        for t in 0..self.taps.count {
            let (position, gain) = (self.taps.position[t], self.taps.gain[t]);
            if self.kind != NoteKind::Sympathetic {
                let period = self.loops[0].period() * self.partial;
                self.loops[0].comb(position * period, gain, &mut heard[..len]);
            }
            let period = self.loops[1].period() * self.partial;
            self.loops[1].comb(
                position * period,
                gain * self.horizontal_gain,
                &mut heard[..len],
            );
        }
        let a = self.aperture;
        let mut state = self.aperture_state;
        for (o, &x) in out[..len].iter_mut().zip(&heard[..len]) {
            state = x + (state - x) * a;
            *o += state;
        }
        self.aperture_state = if state.abs() < 1e-20 { 0.0 } else { state };
    }
}
