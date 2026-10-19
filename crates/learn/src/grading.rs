//! Grading what was played: single notes, chords played together within a
//! window, steps in order, and timing against a beat. Times are seconds on
//! one clock; nothing here knows about the UI or audio.

use serde::{Deserialize, Serialize};

/// Whether a note must be played in the octave it's written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Octave {
    #[default]
    Exact,
    Any,
}

pub fn note_matches(target: u8, played: u8, octave: Octave) -> bool {
    match octave {
        Octave::Exact => target == played,
        Octave::Any => target % 12 == played % 12,
    }
}

/// Signed timing error in milliseconds: negative is early.
pub fn timing_error_ms(played: f64, expected: f64) -> f64 {
    (played - expected) * 1000.0
}

/// How close to the beat a note was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rating {
    Perfect,
    Good,
    Fair,
    Off,
}

impl Rating {
    pub const PERFECT_MS: f64 = 35.0;
    pub const GOOD_MS: f64 = 80.0;
    pub const FAIR_MS: f64 = 150.0;

    pub fn of(error_ms: f64) -> Rating {
        match error_ms.abs() {
            e if e <= Self::PERFECT_MS => Rating::Perfect,
            e if e <= Self::GOOD_MS => Rating::Good,
            e if e <= Self::FAIR_MS => Rating::Fair,
            _ => Rating::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Rating::Perfect => "Perfect",
            Rating::Good => "Good",
            Rating::Fair => "OK",
            Rating::Off => "Off",
        }
    }
}

/// Running timing statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    pub count: u32,
    sum: f64,
    sum_abs: f64,
}

impl Timing {
    pub fn add(&mut self, error_ms: f64) {
        self.count += 1;
        self.sum += error_ms;
        self.sum_abs += error_ms.abs();
    }

    /// Average distance from the beat, in ms.
    pub fn mean_abs(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum_abs / f64::from(self.count))
    }

    /// Average lean: negative rushes, positive drags.
    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / f64::from(self.count))
    }

    /// "±23 ms (5 ms late)".
    pub fn summary(&self) -> Option<String> {
        let (abs, mean) = (self.mean_abs()?, self.mean()?);
        let lean = if mean.abs() < 3.0 {
            "centered".to_owned()
        } else if mean < 0.0 {
            format!("{:.0} ms early", -mean)
        } else {
            format!("{mean:.0} ms late")
        };
        Some(format!("±{abs:.0} ms ({lean})"))
    }
}

/// What a press or the passing of time did to a step.
#[derive(Debug, Clone, PartialEq)]
pub enum StepEvent {
    /// Nothing decided yet.
    Pending,
    /// The right note joined a chord still being played.
    Partial,
    /// Every note of the step, within the window. `time` is when its first
    /// note went down, `spread` how far apart the first and last were.
    Hit { time: f64, spread: f64 },
    /// A note that isn't in the step.
    Wrong(u8),
    /// The window closed with notes missing: not played together.
    Apart,
}

/// One step: a note or a chord, whose notes must all go down within
/// `window` seconds of the first.
#[derive(Debug, Clone)]
pub struct Step {
    target: Vec<u8>,
    window: f64,
    octave: Octave,
    /// Target notes pressed so far, with when.
    pressed: Vec<(u8, f64)>,
}

/// The default window for a chord's notes: humans roll chords a little.
pub const CHORD_WINDOW: f64 = 0.15;

impl Step {
    pub fn new(target: &[u8], window: f64, octave: Octave) -> Step {
        let mut target = target.to_vec();
        target.sort_unstable();
        target.dedup();
        Step {
            target,
            window,
            octave,
            pressed: Vec::new(),
        }
    }

    pub fn target(&self) -> &[u8] {
        &self.target
    }

    /// Target notes already down, during a chord.
    pub fn pressed(&self) -> impl Iterator<Item = u8> + '_ {
        self.pressed.iter().map(|p| p.0)
    }

    fn matching(&self, played: u8) -> Option<u8> {
        self.target
            .iter()
            .copied()
            .find(|t| note_matches(*t, played, self.octave))
    }

    /// A key went down at `time`.
    pub fn press(&mut self, played: u8, time: f64) -> StepEvent {
        if let Some(event) = self.expire(time) {
            // The previous attempt ran out of time; this press starts anew.
            let next = self.press(played, time);
            return match next {
                StepEvent::Hit { .. } | StepEvent::Wrong(_) => next,
                _ => event,
            };
        }
        let Some(target) = self.matching(played) else {
            self.pressed.clear();
            return StepEvent::Wrong(played);
        };
        if !self.pressed.iter().any(|p| p.0 == target) {
            self.pressed.push((target, time));
        }
        if self.pressed.len() == self.target.len() {
            let first = self
                .pressed
                .iter()
                .map(|p| p.1)
                .fold(f64::INFINITY, f64::min);
            let last = self
                .pressed
                .iter()
                .map(|p| p.1)
                .fold(f64::NEG_INFINITY, f64::max);
            self.pressed.clear();
            StepEvent::Hit {
                time: first,
                spread: last - first,
            }
        } else {
            StepEvent::Partial
        }
    }

    /// Call as time passes: says `Apart` once the window has closed on an
    /// incomplete chord.
    pub fn tick(&mut self, now: f64) -> StepEvent {
        self.expire(now).unwrap_or(StepEvent::Pending)
    }

    fn expire(&mut self, now: f64) -> Option<StepEvent> {
        let first = self
            .pressed
            .iter()
            .map(|p| p.1)
            .fold(f64::INFINITY, f64::min);
        if !self.pressed.is_empty() && now - first > self.window {
            self.pressed.clear();
            return Some(StepEvent::Apart);
        }
        None
    }

    /// Forgets a chord in progress.
    pub fn reset(&mut self) {
        self.pressed.clear();
    }
}

/// How one step of a sequence went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepResult {
    /// Played right the first time.
    pub clean: bool,
    pub time: f64,
    /// Against the beat, when there is one.
    pub error_ms: Option<f64>,
}

/// What happened in a sequence.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Nothing,
    /// A chord is under way.
    Partial,
    /// Step `index` is done.
    Done {
        index: usize,
        error_ms: Option<f64>,
    },
    Wrong(u8),
    Apart,
    /// The last step is done.
    Finished,
}

/// Steps to play in order: a scale one note at a time, chords one after
/// another. A mistake marks the step but doesn't move on.
#[derive(Debug, Clone)]
pub struct Sequence {
    steps: Vec<Vec<u8>>,
    window: f64,
    octave: Octave,
    index: usize,
    current: Option<Step>,
    results: Vec<StepResult>,
    /// The current step has had a mistake.
    stumbled: bool,
    pub mistakes: u32,
    /// When each step should be played, after `origin`, for timing
    /// against a metronome.
    schedule: Option<Vec<f64>>,
    origin: f64,
    pub timing: Timing,
}

impl Sequence {
    pub fn new(steps: Vec<Vec<u8>>, window: f64, octave: Octave) -> Sequence {
        let current = steps.first().map(|s| Step::new(s, window, octave));
        Sequence {
            steps,
            window,
            octave,
            index: 0,
            current,
            results: Vec::new(),
            stumbled: false,
            mistakes: 0,
            schedule: None,
            origin: 0.0,
            timing: Timing::default(),
        }
    }

    /// Times each step is due, after the origin, to grade timing.
    pub fn with_schedule(mut self, times: Vec<f64>) -> Sequence {
        self.schedule = Some(times);
        self
    }

    /// Where the schedule's zero is on the clock: the metronome's first beat.
    pub fn set_origin(&mut self, origin: f64) {
        self.origin = origin;
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn is_finished(&self) -> bool {
        self.index >= self.steps.len()
    }

    pub fn current(&self) -> Option<&[u8]> {
        self.steps.get(self.index).map(Vec::as_slice)
    }

    /// Notes of the current chord already down.
    pub fn partial(&self) -> Vec<u8> {
        self.current
            .as_ref()
            .map_or_else(Vec::new, |s| s.pressed().collect())
    }

    pub fn results(&self) -> &[StepResult] {
        &self.results
    }

    pub fn due(&self, index: usize) -> Option<f64> {
        Some(self.origin + self.schedule.as_ref()?.get(index)?)
    }

    /// Steps played right the first time, out of all of them.
    pub fn accuracy(&self) -> f64 {
        if self.steps.is_empty() {
            return 1.0;
        }
        self.results.iter().filter(|r| r.clean).count() as f64 / self.steps.len() as f64
    }

    pub fn press(&mut self, played: u8, time: f64) -> Progress {
        let Some(step) = self.current.as_mut() else {
            return Progress::Nothing;
        };
        let event = step.press(played, time);
        self.apply(event)
    }

    pub fn tick(&mut self, now: f64) -> Progress {
        let Some(step) = self.current.as_mut() else {
            return Progress::Nothing;
        };
        let event = step.tick(now);
        self.apply(event)
    }

    fn apply(&mut self, event: StepEvent) -> Progress {
        match event {
            StepEvent::Pending => Progress::Nothing,
            StepEvent::Partial => Progress::Partial,
            StepEvent::Wrong(p) => {
                self.stumble();
                Progress::Wrong(p)
            }
            StepEvent::Apart => {
                self.stumble();
                Progress::Apart
            }
            StepEvent::Hit { time, .. } => {
                let index = self.index;
                let error_ms = self.due(index).map(|due| timing_error_ms(time, due));
                if let Some(e) = error_ms {
                    self.timing.add(e);
                }
                self.results.push(StepResult {
                    clean: !self.stumbled,
                    time,
                    error_ms,
                });
                self.stumbled = false;
                self.index += 1;
                self.current = self
                    .steps
                    .get(self.index)
                    .map(|s| Step::new(s, self.window, self.octave));
                if self.is_finished() {
                    Progress::Finished
                } else {
                    Progress::Done { index, error_ms }
                }
            }
        }
    }

    fn stumble(&mut self) {
        if !self.stumbled {
            self.mistakes += 1;
        }
        self.stumbled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_match_exactly_or_in_any_octave() {
        assert!(note_matches(60, 60, Octave::Exact));
        assert!(!note_matches(60, 72, Octave::Exact));
        assert!(note_matches(60, 72, Octave::Any));
        assert!(note_matches(61, 49, Octave::Any));
        assert!(!note_matches(60, 61, Octave::Any));
    }

    #[test]
    fn timing_errors_and_ratings() {
        assert!((timing_error_ms(1.020, 1.0) - 20.0).abs() < 1e-9);
        assert!((timing_error_ms(0.95, 1.0) + 50.0).abs() < 1e-9);
        assert_eq!(Rating::of(-20.0), Rating::Perfect);
        assert_eq!(Rating::of(60.0), Rating::Good);
        assert_eq!(Rating::of(-120.0), Rating::Fair);
        assert_eq!(Rating::of(400.0), Rating::Off);
        let mut t = Timing::default();
        assert_eq!(t.summary(), None);
        for e in [10.0, -20.0, 40.0] {
            t.add(e);
        }
        assert!((t.mean_abs().unwrap() - 70.0 / 3.0).abs() < 1e-9);
        assert!((t.mean().unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(t.summary().unwrap(), "±23 ms (10 ms late)");
    }

    #[test]
    fn a_single_note_step() {
        let mut step = Step::new(&[60], CHORD_WINDOW, Octave::Exact);
        assert_eq!(step.press(62, 0.0), StepEvent::Wrong(62));
        assert_eq!(
            step.press(60, 0.5),
            StepEvent::Hit {
                time: 0.5,
                spread: 0.0
            }
        );
    }

    #[test]
    fn chords_must_be_played_within_the_window() {
        let mut step = Step::new(&[60, 64, 67], 0.1, Octave::Exact);
        assert_eq!(step.press(64, 1.00), StepEvent::Partial);
        assert_eq!(step.press(60, 1.03), StepEvent::Partial);
        assert_eq!(step.tick(1.05), StepEvent::Pending);
        let hit = step.press(67, 1.06);
        let StepEvent::Hit { time, spread } = hit else {
            panic!("{hit:?}")
        };
        assert!((time - 1.0).abs() < 1e-9 && (spread - 0.06).abs() < 1e-9);

        // Rolled too slowly: the window closes.
        assert_eq!(step.press(60, 2.0), StepEvent::Partial);
        assert_eq!(step.press(64, 2.05), StepEvent::Partial);
        assert_eq!(step.tick(2.2), StepEvent::Apart);
        // And the next try starts fresh.
        assert_eq!(step.press(60, 3.0), StepEvent::Partial);
        assert_eq!(step.press(64, 3.01), StepEvent::Partial);
        assert!(matches!(step.press(67, 3.02), StepEvent::Hit { .. }));

        // A late press both closes the old window and starts a new one.
        assert_eq!(step.press(60, 4.0), StepEvent::Partial);
        assert_eq!(step.press(64, 4.5), StepEvent::Apart);
        assert_eq!(step.pressed().collect::<Vec<_>>(), [64]);

        // A wrong note spoils the chord.
        step.reset();
        assert_eq!(step.press(60, 5.0), StepEvent::Partial);
        assert_eq!(step.press(65, 5.01), StepEvent::Wrong(65));
        assert_eq!(step.press(64, 5.02), StepEvent::Partial);
    }

    #[test]
    fn doubled_presses_count_once() {
        let mut step = Step::new(&[60, 64], 0.2, Octave::Exact);
        assert_eq!(step.press(60, 0.0), StepEvent::Partial);
        assert_eq!(step.press(60, 0.05), StepEvent::Partial);
        assert!(matches!(step.press(64, 0.1), StepEvent::Hit { .. }));
    }

    #[test]
    fn scales_are_checked_in_order() {
        let steps: Vec<Vec<u8>> = [60, 62, 64, 65].iter().map(|p| vec![*p]).collect();
        let mut seq = Sequence::new(steps, CHORD_WINDOW, Octave::Exact);
        assert_eq!(
            seq.press(60, 0.0),
            Progress::Done {
                index: 0,
                error_ms: None
            }
        );
        assert_eq!(seq.press(64, 0.5), Progress::Wrong(64), "skipped D");
        assert_eq!(seq.press(63, 0.6), Progress::Wrong(63));
        assert_eq!(seq.mistakes, 1, "one stumble per step");
        assert_eq!(seq.current(), Some(&[62][..]));
        assert_eq!(
            seq.press(62, 0.7),
            Progress::Done {
                index: 1,
                error_ms: None
            }
        );
        assert_eq!(
            seq.press(64, 1.0),
            Progress::Done {
                index: 2,
                error_ms: None
            }
        );
        assert_eq!(seq.press(65, 1.5), Progress::Finished);
        assert!(seq.is_finished());
        assert_eq!(seq.press(67, 2.0), Progress::Nothing);
        assert_eq!(seq.results().iter().filter(|r| r.clean).count(), 3);
        assert!((seq.accuracy() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn timing_against_a_schedule() {
        let steps = vec![vec![60], vec![62], vec![64]];
        let mut seq =
            Sequence::new(steps, CHORD_WINDOW, Octave::Exact).with_schedule(vec![0.0, 0.5, 1.0]);
        seq.set_origin(1.0);
        assert_eq!(seq.due(2), Some(2.0));
        let Progress::Done {
            error_ms: Some(e), ..
        } = seq.press(60, 1.02)
        else {
            panic!()
        };
        assert!((e - 20.0).abs() < 1e-6);
        seq.press(62, 1.47);
        seq.press(64, 2.0);
        assert_eq!(seq.timing.count, 3);
        assert!((seq.timing.mean_abs().unwrap() - 50.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn chord_sequences_report_apart() {
        let mut seq = Sequence::new(vec![vec![60, 64, 67], vec![65, 69, 72]], 0.1, Octave::Exact);
        assert_eq!(seq.press(60, 0.0), Progress::Partial);
        assert_eq!(seq.partial(), [60]);
        assert_eq!(seq.tick(0.3), Progress::Apart);
        assert_eq!(seq.mistakes, 1);
        seq.press(60, 1.0);
        seq.press(64, 1.01);
        assert!(matches!(
            seq.press(67, 1.02),
            Progress::Done { index: 0, .. }
        ));
        assert!(!seq.results()[0].clean);
    }
}
