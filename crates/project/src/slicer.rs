//! The Slicer's side of the document: where a track's slices are, how
//! fast its loop plays to match the project's tempo, and the arithmetic of
//! slicing (a tempo guessed from a loop's length, a grid of slices, the
//! notes that play every slice in order).
//!
//! Slices are stored in the file's own frames, so they land on the same
//! musical places whatever the session rate, the mode or the tempo.

use midi::{Pitch, Tick, Velocity};

use crate::{Stretch, StretchMode, TempoMap, Track};

/// Most slices a track has: slice `k` plays on key `root + k`, and this
/// many keep a root anywhere up to C6 inside the keyboard.
pub const MAX_SLICES: usize = 64;

/// The bar counts a loop's tempo is guessed from, and the tempos a guess
/// may land between.
const GUESS_BARS: [u32; 5] = [1, 2, 4, 8, 16];
const GUESS_LOWEST: f64 = 70.0;
const GUESS_HIGHEST: f64 = 180.0;
const GUESS_NEAREST: f64 = 120.0;

/// How the Slicer matches its loop to the project's tempo: its Mode
/// parameter, by label index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Varispeed: the pitch follows the speed.
    Resample,
    /// Time-stretched: the pitch stays.
    Stretch,
    /// The file as it is.
    Off,
}

impl Mode {
    pub fn from_index(index: f32) -> Mode {
        match index.round() as i32 {
            1 => Mode::Stretch,
            2 => Mode::Off,
            _ => Mode::Resample,
        }
    }
}

/// Slice starts as a track keeps them: sorted, without duplicates, the
/// first at 0, at most [`MAX_SLICES`]. Nothing stays nothing.
pub fn normalize(mut starts: Vec<u64>) -> Vec<u64> {
    if starts.is_empty() {
        return starts;
    }
    starts.push(0);
    starts.sort_unstable();
    starts.dedup();
    starts.truncate(MAX_SLICES);
    starts
}

/// The tempo of a loop `frames` long at `rate`, guessed as a whole number
/// of 4/4 bars (1, 2, 4, 8 or 16) that puts it between 70 and 180 BPM,
/// the one nearest 120 if several do: `(bars, bpm)`. A loop no bar count
/// fits is called one bar, at whatever tempo that makes.
pub fn guess_bars(frames: u64, rate: u32) -> (u32, f64) {
    let seconds = frames as f64 / f64::from(rate.max(1));
    if seconds <= 0.0 {
        return (1, GUESS_NEAREST);
    }
    let bpm = |bars: u32| f64::from(bars * 4) * 60.0 / seconds;
    GUESS_BARS
        .iter()
        .map(|&bars| (bars, bpm(bars)))
        .filter(|(_, bpm)| (GUESS_LOWEST..=GUESS_HIGHEST).contains(bpm))
        .min_by(|(_, a), (_, b)| {
            (a - GUESS_NEAREST)
                .abs()
                .total_cmp(&(b - GUESS_NEAREST).abs())
        })
        .unwrap_or((1, bpm(1)))
}

/// Slices every `1 / division` of a 4/4 bar (4 for quarter notes, 16 for
/// sixteenths) at `bpm`, over a file `frames` long at `rate`: their starts,
/// as many as fit up to [`MAX_SLICES`].
pub fn grid(frames: u64, rate: u32, bpm: f64, division: u32) -> Vec<u64> {
    let step = f64::from(rate) * 60.0 / bpm.max(1.0) * 4.0 / f64::from(division.max(1));
    if !step.is_finite() || step < 1.0 {
        return vec![0];
    }
    (0..MAX_SLICES)
        .map(|k| (k as f64 * step).round() as u64)
        .take_while(|&start| start < frames.max(1))
        .collect()
}

/// How fast the Slicer on `track` plays its loop to match `tempo`, as
/// audio clips' speeds are worked out ([`Stretch::effective_speed`]): the
/// project's tempo over the loop's own when Follow Tempo is on, 1 when it
/// is off or the mode is Off.
pub fn speed(track: &Track, tempo: &TempoMap) -> f64 {
    let param = |id: &str| track.instrument_param(id);
    let mode = Mode::from_index(param("mode").unwrap_or(0.0));
    let follow = param("follow").is_some_and(|v| v >= 0.5);
    let original = param("original_tempo").map(f64::from);
    let stretch = Stretch {
        mode: match mode {
            Mode::Stretch => StretchMode::Stretch,
            Mode::Resample | Mode::Off => StretchMode::Resample,
        },
        speed: 1.0,
        transpose: 0.0,
        original_bpm: original,
        sync: follow && mode != Mode::Off,
    };
    stretch.effective_speed(tempo)
}

/// The notes that play every slice once, in order, each where its slice
/// falls in the loop played at `speed` and as long as the slice: starts
/// from 0, in ticks at `tempo`'s tempo. `starts` are in frames of a file
/// `frames` long at `rate`; slice `k` plays on key `root + k` (none past
/// the top of the keyboard).
pub fn slice_notes(
    starts: &[u64],
    frames: u64,
    rate: u32,
    speed: f64,
    tempo: &TempoMap,
    root: u8,
) -> Vec<(Tick, Tick, Pitch, Velocity)> {
    let ticks_per_frame = tempo.tempo.to_bpm() / 60.0 * midi::PPQ as f64
        / f64::from(rate.max(1))
        / speed.max(Stretch::MIN_SPEED);
    let tick = |frame: u64| Tick::new((frame as f64 * ticks_per_frame).round() as i64);
    let one = [0];
    let starts = if starts.is_empty() { &one[..] } else { starts };
    starts
        .iter()
        .enumerate()
        .filter_map(|(k, &start)| {
            let pitch = Pitch::new(u8::try_from(usize::from(root) + k).ok()?)?;
            let end = starts.get(k + 1).copied().unwrap_or(frames).max(start);
            let (from, to) = (tick(start), tick(end));
            (to > from).then_some((from, to - from, pitch, Velocity::from_midi1(100)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizing_sorts_dedups_starts_at_zero_and_caps_the_count() {
        assert_eq!(normalize(vec![300, 100, 300, 200]), [0, 100, 200, 300]);
        assert_eq!(normalize(vec![0, 5]), [0, 5]);
        assert!(normalize(Vec::new()).is_empty());
        let many: Vec<u64> = (1..=200).map(|k| k * 10).collect();
        let kept = normalize(many);
        assert_eq!(kept.len(), MAX_SLICES);
        assert_eq!(kept[0], 0);
        assert!(kept.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn the_tempo_guess_lands_between_70_and_180() {
        let rate = 44_100;
        // Two bars at 120: 4 seconds.
        assert_eq!(guess_bars(4 * 44_100, rate), (2, 120.0));
        // One bar at 95.
        let (bars, bpm) = guess_bars((4.0 * 60.0 / 95.0 * 44_100.0) as u64, rate);
        assert_eq!(bars, 1);
        assert!((bpm - 95.0).abs() < 0.01, "{bpm}");
        // Four bars at 174 could as well be two at 87: both are in range,
        // and 87 is nearer 120.
        let seconds = 16.0 * 60.0 / 174.0;
        let (bars, bpm) = guess_bars((seconds * 44_100.0) as u64, rate);
        assert_eq!(bars, 2);
        assert!((bpm - 87.0).abs() < 0.01, "{bpm}");
        // Every length gets a guess in range when some bar count fits.
        for frames in (20_000..2_000_000).step_by(37_123) {
            let (_, bpm) = guess_bars(frames, rate);
            let seconds = frames as f64 / 44_100.0;
            if seconds <= 4.0 * 60.0 / 180.0 * 16.0 && seconds >= 4.0 * 60.0 / 70.0 / 2.0 {
                assert!((70.0..=180.0).contains(&bpm), "{frames}: {bpm}");
            }
        }
    }

    #[test]
    fn a_grid_slices_at_note_values_and_stops_at_64() {
        // One bar at 120 is 2 s: sixteenths every 0.125 s.
        let starts = grid(96_000, 48_000, 120.0, 16);
        assert_eq!(starts.len(), 16);
        assert_eq!(starts[1], 6000);
        assert_eq!(grid(96_000, 48_000, 120.0, 4), [0, 24_000, 48_000, 72_000]);
        // Eight bars of sixteenths would be 128.
        assert_eq!(grid(8 * 96_000, 48_000, 120.0, 16).len(), MAX_SLICES);
    }

    #[test]
    fn slice_notes_play_every_slice_in_order_and_never_past_the_top_key() {
        let tempo = TempoMap::default();
        let bpm = tempo.tempo.to_bpm();
        // One beat at the project's tempo, in four slices, at speed 1.
        let frames = (48_000.0 * 60.0 / bpm) as u64;
        let starts = [0, frames / 4, frames / 2, 3 * frames / 4];
        let notes = slice_notes(&starts, frames, 48_000, 1.0, &tempo, 60);
        assert_eq!(notes.len(), 4);
        for (k, (start, len, pitch, _)) in notes.iter().enumerate() {
            assert_eq!(pitch.get(), 60 + k as u8);
            assert!((start.get() - 240 * k as i64).abs() <= 1, "{start:?}");
            assert!((len.get() - 240).abs() <= 1, "{len:?}");
        }
        // At double speed the loop takes half as long.
        let fast = slice_notes(&starts, frames, 48_000, 2.0, &tempo, 60);
        assert!((fast[3].0.get() - 360).abs() <= 1);
        // A root near the top drops the slices past key 127.
        let starts: Vec<u64> = (0..64).map(|k| k * 100).collect();
        let notes = slice_notes(&starts, 6400, 48_000, 1.0, &tempo, 120);
        assert_eq!(notes.len(), 8);
        assert_eq!(notes.last().unwrap().2.get(), 127);
    }
}
