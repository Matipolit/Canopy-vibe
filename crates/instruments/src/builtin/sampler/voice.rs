//! One sounding region: sample playback with cubic interpolation and
//! looping, SFZ's envelopes and LFOs, and the region's filter.

use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_4, SQRT_2, TAU};

use dsp::{Smoothed, Svf, db_to_gain};

use super::bank::{BankRegion, LoopRange, PAD, Sample};
use super::region::{EgDesc, FilterType, LfoDesc, LoopMode, OffMode, Region};

/// Frames between updates of pitch, filter cutoff and LFOs. Short enough
/// that steps can't be heard, long enough to keep `exp2` and `tan` out of
/// the per-frame loop.
pub(super) const CONTROL_INTERVAL: u32 = 16;

/// How long a note's own bend may take to arrive, at most: it arrives
/// within a block ([`dsp::MAX_BLOCK`]) anyway, a few control updates.
const BEND_RAMP_MS: f32 = 5.0;

/// Decays and releases are exponential: they fall to this fraction of
/// where they started (-80 dB) over their time, then stop.
const ENV_FLOOR: f32 = 1e-4;

/// Shared by every voice, read at each control update.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Control {
    /// The pitch wheel, `-8192..=8191`.
    pub bend: i16,
    /// The sampler's tune parameter.
    pub tune_cents: f32,
}

/// What a note brings to the voices it starts.
#[derive(Debug, Clone, Copy)]
pub(super) struct NoteStart {
    pub key: u8,
    /// `0..=1`, for velocity curves.
    pub velocity: f32,
    /// `0..=127`, for velocity crossfades.
    pub velocity7: u8,
    /// The sampler's velocity sensitivity; scales `amp_veltrack`.
    pub velocity_scale: f32,
    /// Seconds added to the attack.
    pub attack_offset: f32,
    /// Extra gain, like a release trigger's `rt_decay`.
    pub gain_db: f32,
    /// Its key is down: a note-off will release it.
    pub held: bool,
    pub release_trigger: bool,
    /// Four values in `0..1`: pitch, amplitude, filter and offset randomness.
    pub random: [f32; 4],
    /// Note-on order.
    pub started: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    Done,
}

/// SFZ's DAHDSR envelope: a linear attack, exponential decay and release.
/// A release starts from wherever the envelope is.
#[derive(Debug, Clone)]
pub(super) struct Eg {
    stage: Stage,
    level: f32,
    /// Frames left in this stage.
    remaining: u32,
    /// Added per frame during the attack.
    step: f32,
    /// Multiplies the distance to `target` each frame of a decay or release.
    coeff: f32,
    target: f32,
    start: f32,
    attack: u32,
    hold: u32,
    decay: u32,
    sustain: f32,
    release: u32,
    /// Seconds to frames.
    rate: f32,
}

impl Eg {
    pub const IDLE: Eg = Eg {
        stage: Stage::Done,
        level: 0.0,
        remaining: 0,
        step: 0.0,
        coeff: 0.0,
        target: 0.0,
        start: 0.0,
        attack: 0,
        hold: 0,
        decay: 0,
        sustain: 0.0,
        release: 0,
        rate: 48_000.0,
    };

    /// `velocity` is `0..=1`; `attack_offset` is added to the attack time.
    pub fn start(desc: &EgDesc, velocity: f32, rate: f32, attack_offset: f32) -> Eg {
        let frames = |seconds: f32| (seconds.max(0.0) * rate).round() as u32;
        let mut eg = Eg {
            stage: Stage::Delay,
            level: 0.0,
            remaining: frames(desc.delay + desc.vel2delay * velocity),
            step: 0.0,
            coeff: 1.0,
            target: 0.0,
            start: desc.start,
            attack: frames(desc.attack + desc.vel2attack * velocity + attack_offset),
            hold: frames(desc.hold + desc.vel2hold * velocity),
            decay: frames(desc.decay + desc.vel2decay * velocity),
            sustain: (desc.sustain + desc.vel2sustain * velocity).clamp(0.0, 1.0),
            release: frames(desc.release + desc.vel2release * velocity),
            rate,
        };
        if eg.remaining == 0 {
            eg.enter_attack();
        }
        eg
    }

    fn enter_attack(&mut self) {
        if self.attack == 0 {
            self.enter_hold();
        } else {
            self.stage = Stage::Attack;
            self.level = self.start;
            self.remaining = self.attack;
            self.step = (1.0 - self.start) / self.attack as f32;
        }
    }

    fn enter_hold(&mut self) {
        self.level = 1.0;
        if self.hold == 0 {
            self.enter_decay();
        } else {
            self.stage = Stage::Hold;
            self.remaining = self.hold;
        }
    }

    fn enter_decay(&mut self) {
        if self.decay == 0 || self.sustain >= 1.0 {
            self.level = self.sustain;
            self.stage = Stage::Sustain;
        } else {
            self.stage = Stage::Decay;
            self.remaining = self.decay;
            self.target = self.sustain;
            self.coeff = ENV_FLOOR.powf(1.0 / self.decay as f32);
        }
    }

    /// Releases over its own release time.
    pub fn release(&mut self) {
        self.release_over(self.release as f32 / self.rate);
    }

    /// Releases over `seconds`, from wherever it is; shortens a release
    /// already running. Released before it started, it never sounds.
    pub fn release_over(&mut self, seconds: f32) {
        match self.stage {
            Stage::Done => {}
            Stage::Delay => {
                self.stage = Stage::Done;
                self.level = 0.0;
            }
            _ => {
                let frames = ((seconds * self.rate).round() as u32).max(1);
                if self.stage == Stage::Release && frames >= self.remaining {
                    return;
                }
                self.stage = Stage::Release;
                self.remaining = frames;
                self.coeff = ENV_FLOOR.powf(1.0 / frames as f32);
            }
        }
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Delay => {
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.enter_attack();
                }
                return 0.0;
            }
            Stage::Attack => {
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.enter_hold();
                } else {
                    self.level += self.step;
                }
            }
            Stage::Hold => {
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.enter_decay();
                }
            }
            Stage::Decay => {
                self.level = self.target + (self.level - self.target) * self.coeff;
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.level = self.target;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain | Stage::Done => {}
            Stage::Release => {
                self.level *= self.coeff;
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.level = 0.0;
                    self.stage = Stage::Done;
                }
            }
        }
        self.level
    }

    /// Advances `frames` frames and returns the level, for envelopes read
    /// once per control interval.
    pub fn skip(&mut self, frames: u32) -> f32 {
        for _ in 0..frames {
            self.next();
        }
        self.level
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn is_releasing(&self) -> bool {
        self.stage == Stage::Release
    }

    /// Silent for good: finished, or sustaining at zero.
    pub fn is_silent(&self) -> bool {
        self.stage == Stage::Done || (self.stage == Stage::Sustain && self.level <= 0.0)
    }
}

/// A sine LFO with a delay and a fade-in, read once per control interval.
#[derive(Debug, Clone, Default)]
struct Lfo {
    phase: f32,
    /// Cycles per frame.
    increment: f32,
    delay: u32,
    fade: u32,
    faded: u32,
    depth: f32,
}

impl Lfo {
    fn start(desc: &LfoDesc, rate: f32) -> Lfo {
        let frames = |seconds: f32| (seconds * rate).round() as u32;
        Lfo {
            phase: 0.0,
            increment: desc.freq / rate,
            delay: frames(desc.delay),
            fade: frames(desc.fade),
            faded: 0,
            depth: desc.depth,
        }
    }

    fn is_on(&self) -> bool {
        self.depth != 0.0 && self.increment > 0.0
    }

    /// The value now, in the depth's units, then moves on `frames`.
    fn advance(&mut self, frames: u32) -> f32 {
        if !self.is_on() {
            return 0.0;
        }
        if self.delay > 0 {
            self.delay = self.delay.saturating_sub(frames);
            return 0.0;
        }
        let fade = if self.faded >= self.fade {
            1.0
        } else {
            self.faded as f32 / self.fade as f32
        };
        self.faded = self.faded.saturating_add(frames);
        let value = (self.phase * TAU).sin() * self.depth * fade;
        self.phase = (self.phase + self.increment * frames as f32).fract();
        value
    }
}

/// The region's filter, one state per channel.
#[derive(Debug, Clone)]
struct Filter {
    kind: FilterType,
    svf: [Svf; 2],
    /// One-pole lowpass state per channel.
    pole: [f32; 2],
    /// One-pole coefficient.
    g: f32,
    /// SVF damping (`1 / Q`), to bring the bandpass peak to unity.
    damping: f32,
    rate: f32,
}

impl Filter {
    fn new() -> Filter {
        Filter {
            kind: FilterType::Lpf2p,
            svf: [Svf::new(1000.0, 0.0), Svf::new(1000.0, 0.0)],
            pole: [0.0; 2],
            g: 1.0,
            damping: 2.0,
            rate: 48_000.0,
        }
    }

    fn prepare(&mut self, rate: f32) {
        self.rate = rate;
        for svf in &mut self.svf {
            svf.prepare(rate);
        }
    }

    fn start(&mut self, kind: FilterType, cutoff: f32, resonance_db: f32) {
        self.kind = kind;
        self.pole = [0.0; 2];
        // 0 dB is a Butterworth response (Q = 0.707); each dB raises the peak.
        let q = FRAC_1_SQRT_2 * db_to_gain(resonance_db);
        self.damping = (1.0 / q).clamp(0.05, 2.0);
        // `Svf` maps resonance 0..=1 linearly onto damping 2..=0.05.
        let resonance = (2.0 - self.damping) / 1.95;
        for svf in &mut self.svf {
            svf.reset();
            svf.set_resonance(resonance);
        }
        self.set_cutoff(cutoff);
    }

    fn set_cutoff(&mut self, hz: f32) {
        match self.kind {
            FilterType::Lpf1p | FilterType::Hpf1p => {
                let hz = hz.clamp(10.0, self.rate * 0.49);
                self.g = 1.0 - (-TAU * hz / self.rate).exp();
            }
            _ => {
                for svf in &mut self.svf {
                    svf.set_cutoff(hz);
                }
            }
        }
    }

    #[inline]
    fn process(&mut self, channel: usize, x: f32) -> f32 {
        match self.kind {
            FilterType::Lpf1p | FilterType::Hpf1p => {
                let state = &mut self.pole[channel];
                *state += self.g * (x - *state);
                if self.kind == FilterType::Lpf1p {
                    *state
                } else {
                    x - *state
                }
            }
            FilterType::Lpf2p => self.svf[channel].process_sample(x).lowpass,
            FilterType::Hpf2p => self.svf[channel].process_sample(x).highpass,
            FilterType::Bpf2p => self.svf[channel].process_sample(x).bandpass * self.damping,
            FilterType::Brf2p => {
                let out = self.svf[channel].process_sample(x);
                out.lowpass + out.highpass
            }
        }
    }
}

/// One sounding region.
#[derive(Debug, Clone)]
pub(super) struct Voice {
    pub active: bool,
    /// Index into the bank's regions.
    pub region: usize,
    pub key: u8,
    /// Note-on order; the lowest is the oldest.
    pub started: u64,
    /// Started by a key that is still down; its note-off releases it.
    pub key_held: bool,
    /// Fading out after being stolen or choked. No longer counts toward
    /// the polyphony.
    pub killing: bool,
    /// Copied from the region for choke and polyphony checks.
    pub group: i64,
    pub off_by: Option<i64>,
    pub off_mode: OffMode,
    pub off_time: f32,
    /// Note-off doesn't stop it (`one_shot`).
    pub one_shot: bool,
    /// Its amplitude release in seconds, before the sampler's release offset.
    pub release_seconds: f32,
    rate: f32,
    /// Frames to wait (`delay`) before starting.
    wait: u32,
    /// Where it is in the sample, in frames.
    pos: f64,
    /// Frames to move per output frame at the note's pitch.
    step: f64,
    /// `step` with bend, tune and modulation, updated per control interval.
    current_step: f64,
    looping: bool,
    /// Exits its loop when released (`loop_sustain`).
    sustain_loop: bool,
    /// Released inside the loop's crossfade: keep looping until past it.
    leave_loop: bool,
    /// Has jumped back to the loop start at least once.
    wrapped: bool,
    amp_eg: Eg,
    fil_eg: Eg,
    pitch_eg: Eg,
    fil_eg_depth: f32,
    pitch_eg_depth: f32,
    pitch_lfo: Lfo,
    amp_lfo: Lfo,
    fil_lfo: Lfo,
    filter: Filter,
    filtered: bool,
    /// Cutoff before modulation, in Hz.
    cutoff: f32,
    /// Level from volume, velocity, crossfades and the like.
    gain: f32,
    /// Stereo mix: left out from left and right in, then right out.
    matrix: [f32; 4],
    /// Amplitude LFO gain, ramped across each control interval.
    amp_mod: f32,
    amp_mod_step: f32,
    /// Frames until the next control update.
    control_left: u32,
    bend_up: f32,
    bend_down: f32,
    /// The note's own bend, in cents, followed at each control update.
    note_bend: Smoothed,
    /// Started and not yet rendered: a bend arriving now is where the
    /// note starts, not a move to glide through.
    fresh: bool,
}

impl Voice {
    pub fn new() -> Voice {
        Voice {
            active: false,
            region: 0,
            key: 0,
            started: 0,
            key_held: false,
            killing: false,
            group: 0,
            off_by: None,
            off_mode: OffMode::Fast,
            off_time: 0.0,
            one_shot: false,
            release_seconds: 0.0,
            rate: 48_000.0,
            wait: 0,
            pos: 0.0,
            step: 1.0,
            current_step: 1.0,
            looping: false,
            sustain_loop: false,
            leave_loop: false,
            wrapped: false,
            amp_eg: Eg::IDLE,
            fil_eg: Eg::IDLE,
            pitch_eg: Eg::IDLE,
            fil_eg_depth: 0.0,
            pitch_eg_depth: 0.0,
            pitch_lfo: Lfo::default(),
            amp_lfo: Lfo::default(),
            fil_lfo: Lfo::default(),
            filter: Filter::new(),
            filtered: false,
            cutoff: 1000.0,
            gain: 0.0,
            matrix: [0.0; 4],
            amp_mod: 1.0,
            amp_mod_step: 0.0,
            control_left: 0,
            bend_up: 200.0,
            bend_down: -200.0,
            note_bend: Smoothed::new(0.0, BEND_RAMP_MS),
            fresh: false,
        }
    }

    pub fn prepare(&mut self, rate: f32) {
        self.rate = rate;
        self.filter.prepare(rate);
        self.note_bend
            .prepare_every(rate, CONTROL_INTERVAL as usize);
        self.active = false;
    }

    /// The note is now `cents` from its own pitch: glide there over the
    /// next few control updates, or start there if nothing has played yet.
    pub fn bend(&mut self, cents: f32) {
        if self.fresh {
            let change = cents - self.note_bend.current();
            self.note_bend.set_immediate(cents);
            self.current_step *= f64::from((change / 1200.0).exp2());
        } else {
            self.note_bend.modulate(cents);
        }
    }

    /// How loud it is now, for choosing a voice to steal.
    pub fn level(&self) -> f32 {
        self.amp_eg.level() * self.gain
    }

    pub fn is_releasing(&self) -> bool {
        self.amp_eg.is_releasing()
    }

    /// Starts region `index` for `note`. Returns false, leaving the voice
    /// idle, if there is nothing to play (an offset past the end).
    pub fn start(
        &mut self,
        note: &NoteStart,
        index: usize,
        region: &BankRegion,
        sample: &Sample,
        control: &Control,
    ) -> bool {
        let desc = &region.desc;
        let [pitch_random, amp_random, fil_random, offset_random] = note.random;
        let start = desc
            .offset
            .saturating_add((desc.offset_random as f32 * offset_random) as u64);
        if start >= region.end as u64 {
            self.active = false;
            return false;
        }
        let key = f32::from(note.key);
        let velocity = note.velocity;
        let rate = self.rate;

        self.active = true;
        self.region = index;
        self.key = note.key;
        self.started = note.started;
        self.key_held = note.held;
        self.killing = false;
        self.group = desc.group;
        self.off_by = desc.off_by;
        self.off_mode = desc.off_mode;
        self.off_time = desc.off_time;
        self.one_shot = region.loop_mode == LoopMode::OneShot;
        self.release_seconds = desc.ampeg.release + desc.ampeg.vel2release * velocity;
        self.wait = (desc.delay * rate).round() as u32;
        self.pos = start as f64;
        self.wrapped = false;
        self.leave_loop = false;
        self.looping = region
            .loop_range
            .is_some_and(|range| (start as usize) < range.end);
        self.sustain_loop = region.loop_mode == LoopMode::Sustain;
        if note.release_trigger && self.sustain_loop {
            self.looping = false;
        }

        let cents = desc.tune
            + desc.pitch_veltrack * velocity
            + desc.pitch_random * (2.0 * pitch_random - 1.0);
        let semitones = (key - desc.pitch_keycenter as f32) * desc.pitch_keytrack / 100.0
            + desc.transpose
            + cents / 100.0;
        // In f64 like the position, so a long note's phase stays exact.
        self.step = f64::from(sample.rate) / f64::from(rate) * (f64::from(semitones) / 12.0).exp2();
        self.current_step = self.step;
        self.bend_up = desc.bend_up;
        self.bend_down = desc.bend_down;
        self.note_bend.set_immediate(0.0);
        self.fresh = true;

        let db = desc.volume
            + desc.amp_keytrack * (key - desc.amp_keycenter as f32)
            + desc.amp_random * (2.0 * amp_random - 1.0)
            + note.gain_db;
        self.gain = db_to_gain(db)
            * desc.amplitude
            * velocity_gain(velocity, desc.amp_veltrack * note.velocity_scale)
            * crossfades(desc, note.key, note.velocity7);
        self.matrix = pan_matrix(desc.pan, desc.width, sample.right.is_some());

        self.amp_eg = Eg::start(&desc.ampeg, velocity, rate, note.attack_offset);
        self.fil_eg_depth = desc.fileg.depth + desc.fileg.vel2depth * velocity;
        self.pitch_eg_depth = desc.pitcheg.depth + desc.pitcheg.vel2depth * velocity;
        self.fil_eg = if self.fil_eg_depth != 0.0 {
            Eg::start(&desc.fileg, velocity, rate, 0.0)
        } else {
            Eg::IDLE
        };
        self.pitch_eg = if self.pitch_eg_depth != 0.0 {
            Eg::start(&desc.pitcheg, velocity, rate, 0.0)
        } else {
            Eg::IDLE
        };
        self.pitch_lfo = Lfo::start(&desc.pitchlfo, rate);
        self.amp_lfo = Lfo::start(&desc.amplfo, rate);
        self.fil_lfo = Lfo::start(&desc.fillfo, rate);

        self.filtered = desc.cutoff.is_some();
        if let Some(cutoff) = desc.cutoff {
            let cents = desc.fil_veltrack * velocity
                + desc.fil_keytrack * (key - desc.fil_keycenter as f32)
                + desc.fil_random * (2.0 * fil_random - 1.0);
            self.cutoff = cutoff * (cents / 1200.0).exp2();
            self.filter
                .start(desc.fil_type, self.cutoff, desc.resonance);
        }

        // Modulation starts where it is, with no ramp in from the last note.
        self.amp_mod = 1.0;
        self.update_control(control);
        self.amp_mod += self.amp_mod_step * CONTROL_INTERVAL as f32;
        self.amp_mod_step = 0.0;
        self.control_left = CONTROL_INTERVAL;
        true
    }

    /// Note-off: releases over `seconds`, and leaves a sustain loop.
    pub fn release(&mut self, seconds: f32) {
        self.key_held = false;
        if self.wait > 0 {
            self.active = false;
            return;
        }
        self.amp_eg.release_over(seconds);
        self.fil_eg.release();
        self.pitch_eg.release();
        if self.sustain_loop {
            self.leave_loop = true;
        }
    }

    /// Fades out over `seconds` to make way for another note.
    pub fn kill(&mut self, seconds: f32) {
        self.key_held = false;
        self.killing = true;
        if self.wait > 0 {
            self.active = false;
            return;
        }
        self.amp_eg.release_over(seconds);
    }

    fn update_control(&mut self, control: &Control) {
        let bend = f32::from(control.bend);
        let bend_cents = if bend >= 0.0 {
            bend / 8191.0 * self.bend_up
        } else {
            bend / 8192.0 * -self.bend_down
        };
        let pitch_eg = if self.pitch_eg_depth != 0.0 {
            self.pitch_eg.skip(CONTROL_INTERVAL) * self.pitch_eg_depth
        } else {
            0.0
        };
        let pitch_lfo = self.pitch_lfo.advance(CONTROL_INTERVAL);
        let mut cents = bend_cents + control.tune_cents + pitch_lfo + pitch_eg;
        if self.note_bend.is_away_from(0.0) {
            cents += self.note_bend.tick();
        }
        self.current_step = if cents == 0.0 {
            self.step
        } else {
            self.step * f64::from((cents / 1200.0).exp2())
        };

        let amp_target = if self.amp_lfo.is_on() {
            db_to_gain(self.amp_lfo.advance(CONTROL_INTERVAL))
        } else {
            1.0
        };
        self.amp_mod_step = (amp_target - self.amp_mod) / CONTROL_INTERVAL as f32;

        if self.filtered {
            let fil_eg = if self.fil_eg_depth != 0.0 {
                self.fil_eg.skip(CONTROL_INTERVAL) * self.fil_eg_depth
            } else {
                0.0
            };
            let cents = self.fil_lfo.advance(CONTROL_INTERVAL) + fil_eg;
            self.filter
                .set_cutoff(self.cutoff * (cents / 1200.0).exp2());
        }
    }

    /// Adds this voice into `left` and `right`.
    pub fn render(
        &mut self,
        region: &BankRegion,
        sample: &Sample,
        control: &Control,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        if !left.is_empty() {
            self.fresh = false;
        }
        let mut done = 0;
        while done < left.len() && self.active {
            if self.wait > 0 {
                let frames = (self.wait as usize).min(left.len() - done);
                self.wait -= frames as u32;
                done += frames;
                continue;
            }
            if self.control_left == 0 {
                self.update_control(control);
                self.control_left = CONTROL_INTERVAL;
            }
            let end = done + (self.control_left as usize).min(left.len() - done);
            self.render_frames(region, sample, &mut left[done..end], &mut right[done..end]);
            self.control_left -= (end - done) as u32;
            done = end;
        }
    }

    fn render_frames(
        &mut self,
        region: &BankRegion,
        sample: &Sample,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let stereo = sample.right.is_some();
        let [ll, lr, rl, rr] = self.matrix;
        for (out_l, out_r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut l, mut r) = self.read(region, sample);
            if self.filtered {
                l = self.filter.process(0, l);
                r = if stereo { self.filter.process(1, r) } else { l };
            }
            let env = self.amp_eg.next();
            self.amp_mod += self.amp_mod_step;
            let gain = env * self.gain * self.amp_mod;
            *out_l += (ll * l + lr * r) * gain;
            *out_r += (rl * l + rr * r) * gain;
            if self.amp_eg.is_silent() || !self.advance(region) {
                self.active = false;
                return;
            }
        }
    }

    /// The sample at the current position, both channels.
    #[inline]
    fn read(&self, region: &BankRegion, sample: &Sample) -> (f32, f32) {
        // Through `i64`: a cast straight to `usize` and back costs a
        // dozen instructions on x86 (see `dsp::split_index`).
        let whole = (self.pos as i64).max(0);
        let i = whole as usize;
        let t = (self.pos - whole as f64) as f32;
        if self.looping
            && let Some(range) = &region.loop_range
        {
            if range.crossfade > 0 {
                let fade_start = range.end - range.crossfade;
                if i >= fade_start {
                    // Blend toward the audio before the loop start, which is
                    // where playback continues after the jump.
                    let w = ((self.pos - fade_start as f64) / range.crossfade as f64) as f32;
                    let (out_l, out_r) = sample.read(i, t);
                    let (in_l, in_r) = sample.read(i - range.length(), t);
                    return (out_l + (in_l - out_l) * w, out_r + (in_r - out_r) * w);
                }
            } else if i + 2 >= range.end || (self.wrapped && i <= range.start) {
                return sample.read_looped(i, t, range, self.wrapped);
            }
        }
        sample.read(i, t)
    }

    /// Moves to the next frame. False when the sample has run out.
    #[inline]
    fn advance(&mut self, region: &BankRegion) -> bool {
        self.pos += self.current_step;
        if self.looping
            && let Some(range) = &region.loop_range
        {
            if self.leave_loop && self.pos < (range.end - range.crossfade) as f64 {
                self.looping = false;
            } else if self.pos >= range.end as f64 {
                let start = range.start as f64;
                self.pos = start + (self.pos - start) % range.length() as f64;
                self.wrapped = true;
            }
        }
        self.looping || self.pos < region.end as f64
    }
}

impl Sample {
    /// Both channels at frame `i` plus `t`, by 4-point cubic (Catmull-Rom)
    /// interpolation. `i` may be up to `frames`.
    #[inline]
    fn read(&self, i: usize, t: f32) -> (f32, f32) {
        let left = cubic(&self.left, i, t);
        match &self.right {
            Some(right) => (left, cubic(right, i, t)),
            None => (left, left),
        }
    }

    /// Like [`Self::read`], with frames past the loop's end read from its
    /// start, and (once it has looped) frames before its start read from
    /// its end.
    fn read_looped(&self, i: usize, t: f32, range: &LoopRange, wrapped: bool) -> (f32, f32) {
        let (start, end, length) = (range.start as i64, range.end as i64, range.length() as i64);
        let index = |offset: i64| {
            let j = i as i64 + offset;
            let j = if j >= end || (wrapped && j < start) {
                start + (j - start).rem_euclid(length)
            } else {
                j
            };
            (j + PAD as i64) as usize
        };
        let taps = [index(-1), index(0), index(1), index(2)];
        let left = hermite(taps.map(|k| self.left[k]), t);
        match &self.right {
            Some(right) => (left, hermite(taps.map(|k| right[k]), t)),
            None => (left, left),
        }
    }
}

#[inline]
fn cubic(data: &[f32], i: usize, t: f32) -> f32 {
    let x = &data[i + PAD - 1..i + PAD + 3];
    hermite([x[0], x[1], x[2], x[3]], t)
}

#[inline]
fn hermite([x0, x1, x2, x3]: [f32; 4], t: f32) -> f32 {
    dsp::catmull_rom(x0, x1, x2, x3, t)
}

/// Amplitude for `velocity` (`0..=1`) at `track` (`amp_veltrack` as a
/// fraction): `velocity²` at full tracking, 40 dB per decade as in
/// sforzando, flat at zero, inverted when negative.
fn velocity_gain(velocity: f32, track: f32) -> f32 {
    let velocity = velocity.clamp(0.0, 1.0);
    if track >= 0.0 {
        velocity.powf(2.0 * track)
    } else {
        (1.0 - velocity).powf(-2.0 * track)
    }
}

/// The velocity and key crossfade gains (`xfin_*`, `xfout_*`).
fn crossfades(d: &Region, key: u8, velocity: u8) -> f32 {
    let (by_velocity, by_key) = (d.xf_velcurve_power, d.xf_keycurve_power);
    fade_in(velocity, d.xfin_lovel, d.xfin_hivel, by_velocity)
        * fade_out(velocity, d.xfout_lovel, d.xfout_hivel, by_velocity)
        * fade_in(key, d.xfin_lokey, d.xfin_hikey, by_key)
        * fade_out(key, d.xfout_lokey, d.xfout_hikey, by_key)
}

fn fade_in(x: u8, low: u8, high: u8, power: bool) -> f32 {
    if x >= high {
        1.0
    } else if x <= low {
        0.0
    } else {
        fade_curve(f32::from(x - low) / f32::from(high - low), power)
    }
}

fn fade_out(x: u8, low: u8, high: u8, power: bool) -> f32 {
    if x <= low {
        1.0
    } else if x >= high {
        0.0
    } else {
        fade_curve(f32::from(high - x) / f32::from(high - low), power)
    }
}

fn fade_curve(t: f32, power: bool) -> f32 {
    if power { t.sqrt() } else { t }
}

/// The stereo mix for `pan` and `width` (both `-1..=1`). Mono samples pan
/// at constant power, unity in the center; stereo ones get a balance
/// control after `width` narrows them.
fn pan_matrix(pan: f32, width: f32, stereo: bool) -> [f32; 4] {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    let (left, right) = (SQRT_2 * angle.cos(), SQRT_2 * angle.sin());
    if stereo {
        let (left, right) = (left.min(1.0), right.min(1.0));
        let same = (1.0 + width) / 2.0;
        let cross = (1.0 - width) / 2.0;
        [left * same, left * cross, right * cross, right * same]
    } else {
        [left, 0.0, right, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 1000.0;

    fn desc() -> EgDesc {
        EgDesc {
            attack: 0.01,
            hold: 0.005,
            decay: 0.02,
            sustain: 0.5,
            release: 0.04,
            ..Region::default().ampeg
        }
    }

    fn run(eg: &mut Eg, frames: usize) -> Vec<f32> {
        (0..frames).map(|_| eg.next()).collect()
    }

    #[test]
    fn envelope_stages() {
        let delayed = EgDesc {
            delay: 0.003,
            ..desc()
        };
        let mut eg = Eg::start(&delayed, 1.0, RATE, 0.0);
        let out = run(&mut eg, 3);
        assert_eq!(out, [0.0; 3], "delay");
        let attack = run(&mut eg, 10);
        assert!(attack.windows(2).all(|w| w[1] > w[0]), "{attack:?}");
        assert!((attack[4] - 0.5).abs() < 0.11, "linear: {attack:?}");
        assert_eq!(attack[9], 1.0);
        assert_eq!(run(&mut eg, 5), [1.0; 5], "hold");
        let decay = run(&mut eg, 20);
        assert!(decay.windows(2).all(|w| w[1] < w[0]));
        assert_eq!(decay[19], 0.5);
        assert_eq!(run(&mut eg, 100), [0.5; 100], "sustain");
        eg.release();
        let release = run(&mut eg, 40);
        assert!(release.windows(2).all(|w| w[1] < w[0]));
        assert!(release[19] < 0.5 * 0.02, "exponential: {}", release[19]);
        assert_eq!(release[39], 0.0);
        assert!(eg.is_silent());
    }

    #[test]
    fn release_mid_attack_starts_where_it_is() {
        let mut eg = Eg::start(&desc(), 1.0, RATE, 0.0);
        run(&mut eg, 5);
        let before = eg.level();
        eg.release();
        let after = eg.next();
        assert!(
            after < before && after > before * 0.7,
            "{before} -> {after}"
        );
    }

    #[test]
    fn velocity_and_offsets_change_times() {
        let tracking = EgDesc {
            vel2attack: 0.05,
            ..desc()
        };
        let fast = Eg::start(&tracking, 0.0, RATE, 0.0);
        let slow = Eg::start(&tracking, 1.0, RATE, 0.1);
        assert_eq!(fast.attack, 10);
        assert_eq!(slow.attack, 160);
    }

    #[test]
    fn velocity_curve() {
        assert_eq!(velocity_gain(0.5, 1.0), 0.25);
        assert_eq!(velocity_gain(0.5, 0.0), 1.0);
        assert_eq!(velocity_gain(0.0, 0.0), 1.0);
        assert_eq!(velocity_gain(1.0, 1.0), 1.0);
        assert_eq!(velocity_gain(1.0, -1.0), 0.0);
    }

    #[test]
    fn pan_law() {
        let center = pan_matrix(0.0, 1.0, false);
        assert!((center[0] - 1.0).abs() < 1e-6 && (center[2] - 1.0).abs() < 1e-6);
        let left = pan_matrix(-1.0, 1.0, false);
        assert!(left[2].abs() < 1e-6 && (left[0] - SQRT_2).abs() < 1e-6);
        let mono = pan_matrix(0.0, 0.0, true);
        assert!(mono.iter().all(|g| (g - 0.5).abs() < 1e-6), "{mono:?}");
        let right = pan_matrix(1.0, 1.0, true);
        assert!(right[0].abs() < 1e-6 && (right[3] - 1.0).abs() < 1e-6);
    }
}
