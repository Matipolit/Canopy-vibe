//! The clipper's true-peak safety: what keeps its output under the ceiling
//! between the samples too.
//!
//! Clipping at a higher rate and filtering back down takes away the
//! harmonics that made the flat tops flat, and the filtered wave overshoots
//! the ceiling, by up to a dB or so on hard-clipped material, at the samples
//! and more between them. This stage catches that: a very short lookahead
//! gain, built as the limiter's is (see `limiter.rs`). The true-peak
//! detector of `dynamics_truepeak.rs` gives each stretch between two samples
//! its peak, read [`MARGIN`] high; the gain that brings it to the ceiling is
//! let go over [`RELEASE_MS`], held at its lowest for the lookahead plus
//! [`HOLD_EXTRA`] samples and averaged over the lookahead, so every drop is
//! a straight ramp that ends where the peak reaches the output. A final
//! clamp only catches rounding.
//!
//! It only ever moves the gain by the overshoot, a fraction of a dB to a
//! dB or two, over half a millisecond, so it is far gentler than the
//! clipping in front of it. Its latency is [`latency_samples`]: 43 samples
//! at 48 kHz.

use crate::dynamics::{DB_PER_LOG2, coefficient};
use crate::halfband::fir::{Design, Up};
use crate::line::Ring;
use crate::lookahead::{self, Lookahead, MIN_GAIN};
use crate::truepeak::{self, TruePeak, turn_height};

/// How far ahead the safety looks, and so how long each drop in gain takes.
const LOOKAHEAD_MS: f32 = 0.5;
/// How long a reduction holds after the peak that needed it, so that a
/// waveform's peaks a few milliseconds apart see one steady gain rather than
/// a ripple, which would put sidebands around every harmonic.
const HOLD_MS: f32 = 10.0;
/// How fast a reduction then lets go.
const RELEASE_MS: f32 = 60.0;
/// True peaks are read 0.05 dB high: more than the detector ever reads low
/// for content up to 0.45 of the sample rate, which is all the clipper's
/// oversampling lets through.
const MARGIN: f32 = 1.005_773;
/// A reduction this small (dB) is none, so a release ends exactly at 1.
const NO_REDUCTION: f32 = 1e-6;

/// The half-length of the halfband that takes the output and its
/// half-sample values on to four points a sample: (c - 1) / 2 must be even,
/// so its pass-through branch lands on the samples themselves.
const QUARTER_HALF: usize = 17;
/// The stretch that reading is about ends this many samples back: the
/// halfband's delay of a quarter of `QUARTER_HALF - 1` samples, and one more.
const QUARTER_DELAY: usize = (QUARTER_HALF - 1) / 4 + 1;

/// Peaks between samples read from the output and its values half a sample
/// on, which the oversampled clipper's last filter gives exactly: what lies
/// between them is band-limited to half their rate, so a short halfband
/// interpolates two more points each, and a parabola the peak between
/// those. Reads to within a few thousandths of a dB up to 0.45 of the
/// sample rate, where `truepeak.rs`, working from the samples
/// alone, under-reads dense material by a tenth of a dB or more.
#[derive(Debug, Clone)]
struct Quarters {
    design: Design,
    up: [Up; 2],
    /// Each channel's four points from the previous sample.
    last: [[f32; 4]; 2],
    /// Samples until the interpolators have filled since they last had
    /// half-sample values; readings until then don't count.
    filling: usize,
    /// Readings, delayed to line up with the true-peak detector's.
    pending: Ring,
}

impl Quarters {
    /// Allocates: not for the audio thread.
    fn new() -> Quarters {
        let design = Design::halfband(QUARTER_HALF, 100.0);
        let mut pending = Ring::default();
        pending.allocate(truepeak::DELAY - QUARTER_DELAY + 1);
        Quarters {
            up: [Up::new(&design), Up::new(&design)],
            design,
            last: [[0.0; 4]; 2],
            filling: FILL,
            pending,
        }
    }

    fn reset(&mut self) {
        for up in &mut self.up {
            up.clear();
        }
        self.last = [[0.0; 4]; 2];
        self.filling = FILL;
        self.pending.clear();
    }

    /// Takes the next sample and the value half a sample on, per channel
    /// (`None` if there are none: the clipper isn't oversampling); returns
    /// the peak of the stretch the true-peak detector reports this sample,
    /// if this one can tell.
    #[inline]
    fn push(&mut self, now: [f32; 2], halves: Option<[f32; 2]>) -> Option<f32> {
        let reading = match halves {
            None => {
                self.filling = FILL;
                0.0
            }
            Some(halves) => {
                let mut peak = 0.0f32;
                for channel in 0..2 {
                    let up = &mut self.up[channel];
                    let [a, b] = up.push(&self.design, now[channel]);
                    let [c, d] = up.push(&self.design, halves[channel]);
                    let last = self.last[channel];
                    // From a quarter before the stretch to a quarter after.
                    let points = [last[0], last[1], last[2], last[3], a, b, c];
                    for turn in points.windows(3) {
                        peak = peak.max(turn_height(turn[0], turn[1], turn[2]));
                    }
                    self.last[channel] = [a, b, c, d];
                }
                self.filling = self.filling.saturating_sub(1);
                peak
            }
        };
        self.pending.push(reading);
        // Good once the filters have filled and every reading since has
        // come through the delay.
        (self.filling == 0).then(|| self.pending.get(truepeak::DELAY - QUARTER_DELAY))
    }
}

/// Samples [`Quarters`] takes to give readings again after it had no
/// half-sample values: its interpolators' memory, and the readings delayed
/// to line up with the true-peak detector's.
const FILL: usize = QUARTER_HALF + truepeak::DELAY;

/// How many samples late the safety's output is at `sample_rate`.
pub(crate) fn latency_samples(sample_rate: f32) -> usize {
    lookahead::latency(LOOKAHEAD_MS, sample_rate)
}

pub(crate) struct Safety {
    detector: TruePeak,
    /// The detector was left out while [`Quarters`] read.
    detector_idle: bool,
    /// The safety isn't heard: only its delay lines run.
    asleep: bool,
    quarters: Quarters,
    /// The reduction needed (dB), let go at the release rate.
    reduction: f32,
    release_keep: f32,
    lookahead: Lookahead,
    /// The sound, and the clipper's dry signal beside it, delayed to meet
    /// their gain.
    lines: [Ring; 2],
    dry: [Ring; 2],
    delay: usize,
    /// The gain applied to the latest sample.
    pub gain: f32,
}

impl Safety {
    pub fn new() -> Safety {
        let mut safety = Safety {
            detector: TruePeak::new(),
            detector_idle: false,
            asleep: false,
            quarters: Quarters::new(),
            reduction: 0.0,
            release_keep: 0.0,
            lookahead: Lookahead::default(),
            lines: [Ring::default(), Ring::default()],
            dry: [Ring::default(), Ring::default()],
            delay: 0,
            gain: 1.0,
        };
        safety.prepare(48_000.0);
        safety
    }

    /// Allocates: not for the audio thread.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.delay = latency_samples(sample_rate);
        let hold = (HOLD_MS * 0.001 * sample_rate) as usize;
        self.lookahead
            .allocate(lookahead::samples(LOOKAHEAD_MS, sample_rate), hold);
        for line in self.lines.iter_mut().chain(&mut self.dry) {
            line.allocate(self.delay + 1);
        }
        self.release_keep = coefficient(RELEASE_MS, sample_rate);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.detector.reset();
        self.detector_idle = false;
        self.asleep = false;
        self.quarters.reset();
        self.reduction = 0.0;
        self.lookahead.clear();
        for line in self.lines.iter_mut().chain(&mut self.dry) {
            line.clear();
        }
        self.gain = 1.0;
    }

    /// Keeps the delay lines going while the safety isn't heard, so that
    /// when it's switched on, the detector can catch up from them.
    #[inline]
    pub fn idle(&mut self, wet: [f32; 2], dry: [f32; 2]) {
        if !std::mem::replace(&mut self.asleep, true) {
            // It starts again from no reduction.
            self.reduction = 0.0;
            self.lookahead.clear();
            self.gain = 1.0;
        }
        self.detector_idle = true;
        self.quarters.push(wet, None);
        for channel in 0..2 {
            self.lines[channel].push(wet[channel]);
            self.dry[channel].push(dry[channel]);
        }
    }

    /// One stereo sample, `wet`, measured against `ceiling` (with its
    /// values half a sample on, if the clipper has them), with the dry
    /// signal that goes with it; returns them both `delay` samples later,
    /// the sound turned down as far as needed and clamped to `limit`.
    #[inline]
    pub fn tick(
        &mut self,
        wet: [f32; 2],
        halves: Option<[f32; 2]>,
        dry: [f32; 2],
        ceiling: f32,
        limit: f32,
    ) -> ([f32; 2], [f32; 2]) {
        self.asleep = false;
        // The exact reading when there is one; the detector from the
        // samples alone otherwise, brought up to date from the delay line
        // if it sat out.
        let peak = match self.quarters.push(wet, halves) {
            Some(between) => {
                self.detector_idle = true;
                between
            }
            None => {
                if std::mem::take(&mut self.detector_idle) {
                    self.detector.reset();
                    let [left, right] = &self.lines;
                    for age in (0..truepeak::TAPS).rev() {
                        self.detector.push(left.get(age), right.get(age), true);
                    }
                }
                self.detector.push(wet[0], wet[1], true)
            }
        } * MARGIN;
        let (needed, needed_db) = if peak > ceiling {
            let needed = (ceiling / peak).max(MIN_GAIN);
            (needed, -DB_PER_LOG2 * needed.log2())
        } else {
            (1.0, 0.0)
        };
        self.reduction = needed_db.max(self.reduction * self.release_keep);
        if self.reduction < NO_REDUCTION {
            self.reduction = 0.0;
        }
        // Never more than this stretch needs, even by a rounding error.
        let envelope = if self.reduction > 0.0 {
            (-self.reduction / DB_PER_LOG2).exp2().min(needed)
        } else {
            1.0
        };
        let gain = self.lookahead.push(envelope);
        self.gain = gain;
        let mut out = [0.0; 2];
        let mut dry_out = [0.0; 2];
        for channel in 0..2 {
            self.lines[channel].push(wet[channel]);
            self.dry[channel].push(dry[channel]);
            out[channel] = (self.lines[channel].get(self.delay) * gain).clamp(-limit, limit);
            dry_out[channel] = self.dry[channel].get(self.delay);
        }
        (out, dry_out)
    }
}
