//! A pad: one sound of the kit. It plays up to two hits of its layers (a
//! new one and one fading out), then its own filter, drive, level and pan.

use std::f32::consts::{FRAC_PI_4, SQRT_2};

use dsp::{Smoothed, db_to_gain};

use super::layer::{CONTROL_SAMPLES, Filter, FilterKind, HitInfo, Layer, LayerHit};
use super::parts::{Noise, Shaper, semitones, velocity_gain};
use super::{LAYERS, Voice};

/// How long a hit fades when its pad plays again. Drum machine voices are
/// monophonic: cutting the old hit dead would click, and letting it ring
/// under the new one would blur rolls and flams.
const RETRIGGER_FADE_MS: f32 = 5.0;

/// How long a choked hit fades, e.g. the open hat when the closed one plays:
/// as quick as the pedal closing on the cymbals, without a click.
const CHOKE_FADE_MS: f32 = 8.0;

/// Ramp for a pad's level, pan, drive and cutoff, so turning them while it
/// plays doesn't zipper.
const MIX_RAMP_MS: f32 = 20.0;

/// Buffers the pads render with, a block long, allocated in `prepare`: two
/// for layers (one holds the layer above, for FM) and a stereo pair for
/// the pad's sum.
#[derive(Debug, Default)]
pub(super) struct Scratch {
    pub layer: Vec<f32>,
    pub above: Vec<f32>,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Scratch {
    pub fn new(len: usize) -> Self {
        Self {
            layer: vec![0.0; len],
            above: vec![0.0; len],
            left: vec![0.0; len],
            right: vec![0.0; len],
        }
    }

    pub fn len(&self) -> usize {
        self.layer.len()
    }
}

/// One hit: every layer's state.
#[derive(Debug, Clone, Default)]
struct Hit {
    layers: [LayerHit; LAYERS],
    /// The audible layers' levels, summed, as the hit starts.
    total: f32,
}

impl Hit {
    fn is_playing(&self) -> bool {
        self.layers.iter().any(|l| l.active)
    }

    /// Notes the layers' levels once they've started.
    fn started(&mut self) {
        self.total = self.audible().map(|l| l.peak).sum();
    }

    fn audible(&self) -> impl Iterator<Item = &LayerHit> {
        self.layers.iter().filter(|l| l.active && l.audible)
    }

    /// Its audible layers' envelopes, weighted by their levels: 1 at full
    /// strength, falling to 0 as the hit ends.
    fn level(&self) -> f32 {
        if self.total <= 0.0 {
            return 0.0;
        }
        self.audible().map(|l| l.level() * l.peak).sum::<f32>() / self.total
    }

    /// Adds the hit into `left` and `right` at `gain`, which falls by
    /// `fade` a sample. `layer` and `above` are scratch, as long.
    fn render<'a>(
        &mut self,
        mut layer: &'a mut [f32],
        mut above: &'a mut [f32],
        left: &mut [f32],
        right: &mut [f32],
        gain: f32,
        fade: f32,
    ) {
        let mut modulator = false;
        for hit in &mut self.layers {
            if !hit.active {
                modulator = false;
                continue;
            }
            hit.render(modulator.then_some(&*above), layer);
            if hit.audible {
                let [to_left, to_right] = hit.gains;
                let out = layer.iter().zip(left.iter_mut()).zip(right.iter_mut());
                if fade > 0.0 {
                    for (n, ((x, l), r)) in out.enumerate() {
                        let g = (gain - fade * n as f32).max(0.0);
                        *l += x * to_left * g;
                        *r += x * to_right * g;
                    }
                } else {
                    let (to_left, to_right) = (to_left * gain, to_right * gain);
                    for ((x, l), r) in out {
                        *l += x * to_left;
                        *r += x * to_right;
                    }
                }
            }
            std::mem::swap(&mut layer, &mut above);
            modulator = true;
        }
    }
}

/// Plays one pad.
pub(super) struct Pad {
    hits: [Hit; 2],
    playing: [bool; 2],
    /// Each hit's velocity gain, ramped down to zero when it fades out.
    gain: [f32; 2],
    /// How much `gain` falls each sample; zero unless fading.
    fade: [f32; 2],
    /// The pad's filter, a channel each.
    filters: [Filter; 2],
    filter_kind: FilterKind,
    cutoff: Smoothed,
    resonance: f32,
    /// Samples until the filter follows `cutoff` again.
    countdown: u32,
    /// Gains into each channel: Level, pan and the pad's place in the
    /// kit's balance together.
    left: Smoothed,
    right: Smoothed,
    drive: Smoothed,
    shaper: Shaper,
    /// The pad's share of the kit, so that a full-velocity hit of each pad
    /// at 0 dB sits where it would in a mixed kit.
    balance: f32,
    /// Level, as a gain, for the meter.
    level: f32,
    meter: f32,
    /// A hit started since the last render: the meter shows it at full
    /// strength for that block, however quickly it falls.
    struck: f32,
    sample_rate: f32,
    /// Where each hit's seed comes from when the pad varies its hits;
    /// restarted from `first_seed` on reset. Otherwise every hit takes the
    /// first of them, `same_seed`.
    seeds: Noise,
    first_seed: u32,
    same_seed: u32,
}

impl Pad {
    pub fn new(balance: f32, seed: u32) -> Self {
        let mut pad = Self {
            hits: Default::default(),
            playing: [false; 2],
            gain: [0.0; 2],
            fade: [0.0; 2],
            filters: Default::default(),
            filter_kind: FilterKind::Off,
            cutoff: Smoothed::new(1000.0, MIX_RAMP_MS),
            resonance: 0.0,
            countdown: CONTROL_SAMPLES,
            left: Smoothed::new(balance, MIX_RAMP_MS),
            right: Smoothed::new(balance, MIX_RAMP_MS),
            drive: Smoothed::new(0.0, MIX_RAMP_MS),
            shaper: Shaper::new(),
            balance,
            level: 1.0,
            meter: 0.0,
            struck: 0.0,
            sample_rate: 48_000.0,
            seeds: Noise::new(seed),
            first_seed: seed,
            same_seed: Noise::new(seed).next_u32(),
        };
        pad.prepare(pad.sample_rate);
        pad
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for smoothed in [
            &mut self.left,
            &mut self.right,
            &mut self.drive,
            &mut self.cutoff,
        ] {
            smoothed.prepare(sample_rate);
        }
        self.restart_filters();
        self.reset();
    }

    /// Silence immediately, and restart the random sequences so a render
    /// from the top always sounds the same.
    pub fn reset(&mut self) {
        self.playing = [false; 2];
        self.gain = [0.0; 2];
        self.fade = [0.0; 2];
        self.meter = 0.0;
        self.struck = 0.0;
        self.seeds = Noise::new(self.first_seed);
        self.settle();
        self.restart_filters();
    }

    fn is_playing(&self) -> bool {
        self.playing[0] || self.playing[1]
    }

    /// Jumps level, pan, drive and cutoff to their targets.
    fn settle(&mut self) {
        for smoothed in [
            &mut self.left,
            &mut self.right,
            &mut self.drive,
            &mut self.cutoff,
        ] {
            smoothed.settle();
        }
        self.shaper.set(self.drive.target());
        let cutoff = self.cutoff.target();
        for filter in &mut self.filters {
            filter.set_cutoff(cutoff);
        }
    }

    /// The filters from silence, at the current settings.
    fn restart_filters(&mut self) {
        let (kind, cutoff, resonance) = (self.filter_kind, self.cutoff.current(), self.resonance);
        for filter in &mut self.filters {
            filter.start(kind, cutoff, resonance, self.sample_rate);
        }
    }

    fn fade_out(&mut self, hit: usize, ms: f32) {
        let step = self.gain[hit] / (ms * 0.001 * self.sample_rate).max(1.0);
        // Already fading faster (a choke during a retrigger fade): keep that.
        self.fade[hit] = self.fade[hit].max(step);
    }

    /// Plays a hit at `velocity` (`0..=1`) with the pad's parameters `p`
    /// (see [`Voice`]), its pitch multiplied by `ratio` (the kit's Tune and
    /// the note's).
    pub fn trigger(&mut self, p: &[f32], velocity: f32, ratio: f32) {
        let gain = velocity_gain(velocity, p[Voice::VELOCITY]);
        if gain <= 0.0 {
            return;
        }
        if !self.is_playing() {
            // Nothing to ramp for, so the hit starts at the settings as they are now.
            self.settle();
        }
        // A free hit, else the quieter one: normally the one already fading.
        let index = match self.playing {
            [false, _] => 0,
            [_, false] => 1,
            _ => {
                let loudness = |i: usize| self.hits[i].level() * self.gain[i];
                usize::from(loudness(1) < loudness(0))
            }
        };
        // The same hit every time, unless the pad varies them.
        let seed = if p[Voice::VARY] >= 0.5 {
            self.seeds.next_u32()
        } else {
            self.same_seed
        };
        let info = HitInfo {
            sample_rate: self.sample_rate,
            ratio: ratio * semitones(p[Voice::TUNE]),
            length: p[Voice::LENGTH] * 0.01,
            velocity,
            seed,
        };
        for (i, layer) in self.hits[index].layers.iter_mut().enumerate() {
            let start = Voice::COMMON + i * Layer::COUNT;
            let info = HitInfo {
                // Unrelated noise in each layer.
                seed: seed.wrapping_add((i as u32 + 1).wrapping_mul(0x9E37_79B9)),
                ..info
            };
            layer.start(&p[start..start + Layer::COUNT], &info);
        }
        if !self.hits[index].is_playing() {
            // Every layer is off: nothing to play, so nothing to cut off either.
            return;
        }
        self.hits[index].started();
        self.struck = self.struck.max(gain);
        let other = 1 - index;
        if self.playing[other] {
            self.fade_out(other, RETRIGGER_FADE_MS);
        }
        self.playing[index] = true;
        self.gain[index] = gain;
        self.fade[index] = 0.0;
    }

    /// Fades out whatever is sounding.
    pub fn choke(&mut self) {
        for hit in 0..2 {
            if self.playing[hit] {
                self.fade_out(hit, CHOKE_FADE_MS);
            }
        }
    }

    /// Takes level, pan, drive and the filter from `p`, ramping to them.
    pub fn mix(&mut self, p: &[f32]) {
        self.level = db_to_gain(p[Voice::LEVEL]);
        // Constant power, scaled so the center is unity in each channel: a
        // centered pad comes out at its own level.
        let angle = (p[Voice::PAN] + 1.0) * FRAC_PI_4;
        let gain = self.balance * self.level * SQRT_2;
        self.left.set_target(gain * angle.cos());
        self.right.set_target(gain * angle.sin());
        self.drive.set_target(p[Voice::DRIVE]);
        self.cutoff.set_target(p[Voice::CUTOFF]);
        let resonance = p[Voice::RESONANCE];
        let kind = FilterKind::from_value(p[Voice::FILTER]);
        if kind != self.filter_kind || resonance != self.resonance {
            self.resonance = resonance;
            if kind != self.filter_kind {
                // Another response: from silence, rather than from a state
                // that was shaped by the old one.
                self.filter_kind = kind;
                self.restart_filters();
            } else {
                for filter in &mut self.filters {
                    filter.set_resonance(resonance);
                }
            }
        }
    }

    /// Adds the pad into `left` and `right` (no longer than `scratch`).
    /// Returns whether it was sounding.
    pub fn render(&mut self, scratch: &mut Scratch, left: &mut [f32], right: &mut [f32]) -> bool {
        if !self.is_playing() {
            self.settle();
            self.meter = 0.0;
            self.struck = 0.0;
            return false;
        }
        let len = left.len();
        if len == 0 {
            return false;
        }
        let layer = &mut scratch.layer[..len];
        let above = &mut scratch.above[..len];
        let sum_left = &mut scratch.left[..len];
        let sum_right = &mut scratch.right[..len];
        sum_left.fill(0.0);
        sum_right.fill(0.0);
        for hit in 0..2 {
            if !self.playing[hit] {
                continue;
            }
            let (gain, fade) = (self.gain[hit], self.fade[hit]);
            self.hits[hit].render(layer, above, sum_left, sum_right, gain, fade);
            self.gain[hit] = (gain - fade * len as f32).max(0.0);
            if self.gain[hit] <= 0.0 || !self.hits[hit].is_playing() {
                self.playing[hit] = false;
            }
        }

        if self.filter_kind != FilterKind::Off {
            let [filter_left, filter_right] = &mut self.filters;
            for (l, r) in sum_left.iter_mut().zip(sum_right.iter_mut()) {
                if self.cutoff.is_smoothing() {
                    let cutoff = self.cutoff.tick();
                    self.countdown -= 1;
                    if self.countdown == 0 {
                        self.countdown = CONTROL_SAMPLES;
                        filter_left.set_cutoff(cutoff);
                        filter_right.set_cutoff(cutoff);
                    }
                }
                *l = filter_left.process(*l);
                *r = filter_right.process(*r);
            }
        }

        let driven = self.drive.is_smoothing() || self.drive.target() > 0.0;
        let channels = sum_left.iter().zip(sum_right.iter());
        for (((x, y), l), r) in channels.zip(left.iter_mut()).zip(right.iter_mut()) {
            let (mut x, mut y) = (*x, *y);
            if driven {
                self.shaper.set(self.drive.tick());
                x = self.shaper.process(x);
                y = self.shaper.process(y);
            }
            *l += x * self.left.tick();
            *r += y * self.right.tick();
        }

        let mut envelope = 0.0f32;
        for hit in 0..2 {
            if self.playing[hit] {
                envelope = envelope.max(self.hits[hit].level().min(1.0) * self.gain[hit]);
            }
        }
        self.meter = envelope.max(self.struck) * self.level;
        self.struck = 0.0;
        true
    }

    /// The pad's envelope after velocity and Level; see [`super::Drums`].
    pub fn meter(&self) -> f32 {
        self.meter
    }
}
