//! The guitar measured from outside, through the `Instrument` interface, the
//! way recordings of real guitars were measured: partial frequencies and
//! inharmonicity, per-partial decay times and their two stages, beating, the
//! pitch glide of a hard pluck, body resonances, pluck-position and pickup
//! combs, bends, vibrato and slides, strum timing, palm mutes, harmonics,
//! dead notes, releases, clicks, and robustness.
//!
//! Where a check compares with real guitars, the reference numbers come from
//! recordings measured with the same methods (Freesound CC0 nylon notes by
//! SpeedY, packs 469 and 472; IDMT-SMT-Guitar's Fender Stratocaster;
//! ntnvsl's clean electric notes; a steel-string low E by FenrirFangs) and
//! from the literature (Fletcher & Rossing 1998; Välimäki et al. 1996;
//! Woodhouse 2004). Measurements are printed (`--nocapture`) as well as
//! checked.

mod common;

use common::{max_step, note_off, peak, seconds, set};
use instruments::builtin::guitar::{
    KEY_SWITCH_FIRST, METER_ARTICULATION, METER_BEND, METER_FRET, METER_HELD, METER_KIND,
    METER_LEVEL, METER_MUTE, METER_NOTE, METER_STRUM, SOFT_PEDAL, STRINGS, body_response_db,
};
use instruments::{Instrument, MAX_METERS, ParameterKind, find_builtin};
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind, Pitch, Velocity};

const RATE: f32 = 48_000.0;
/// Small blocks, so meters are read every 1.3 ms.
const BLOCK: usize = 64;

const STEEL: f32 = 0.0;
const NYLON: f32 = 1.0;
const ELECTRIC: f32 = 2.0;

fn guitar_at(rate: f32) -> Box<dyn Instrument> {
    let descriptor = find_builtin("canopy.guitar").expect("the guitar is built in");
    let mut guitar = (descriptor.create)();
    guitar.prepare(rate, BLOCK);
    guitar
}

fn fresh() -> Box<dyn Instrument> {
    guitar_at(RATE)
}

/// A guitar with `settings`, settled.
fn with(settings: &[(&str, f32)]) -> Box<dyn Instrument> {
    let mut guitar = fresh();
    for &(id, value) in settings {
        set(&mut *guitar, id, value);
    }
    guitar.reset();
    guitar
}

fn velocity(v: f32) -> Velocity {
    Velocity::new((v.clamp(0.0, 1.0) * 65535.0).round() as u16)
}

fn on(note: u8, v: f32) -> NoteEventKind {
    NoteEventKind::On {
        pitch: Pitch::new(note).unwrap(),
        velocity: velocity(v),
    }
}

fn control(controller: u8, value: u8) -> NoteEventKind {
    NoteEventKind::Control { controller, value }
}

/// A latched key-switch: open, palm mute, harmonics, dead notes.
fn switch(articulation: u8) -> NoteEventKind {
    on(KEY_SWITCH_FIRST + articulation, 0.8)
}

#[derive(Default)]
struct Take {
    left: Vec<f32>,
    right: Vec<f32>,
    /// Meters after each block.
    meters: Vec<[f32; MAX_METERS]>,
}

impl Take {
    /// The meters at `sample`.
    fn meters_at(&self, sample: usize) -> &[f32; MAX_METERS] {
        &self.meters[(sample / BLOCK).min(self.meters.len() - 1)]
    }

    /// First sample of the block after which string `s` sounds `note`.
    fn onset(&self, string: usize, note: u8) -> Option<usize> {
        self.meters
            .iter()
            .position(|m| m[METER_NOTE + string] == f32::from(note))
            .map(|block| block * BLOCK)
    }

    /// Left and right summed.
    fn mono(&self) -> Vec<f32> {
        self.left
            .iter()
            .zip(&self.right)
            .map(|(l, r)| 0.5 * (l + r))
            .collect()
    }
}

/// Plays `events` (sample time, event) over `samples` samples.
fn play(guitar: &mut dyn Instrument, events: &[(usize, NoteEventKind)], samples: usize) -> Take {
    play_at(guitar, events, samples, BLOCK)
}

fn play_at(
    guitar: &mut dyn Instrument,
    events: &[(usize, NoteEventKind)],
    samples: usize,
    block: usize,
) -> Take {
    let mut take = Take::default();
    let (mut left, mut right) = (vec![0.0; block], vec![0.0; block]);
    let mut block_events = Vec::new();
    let mut next = 0;
    let mut sorted = events.to_vec();
    sorted.sort_by_key(|&(at, kind)| (at, NoteEvent { frame: 0, kind }));
    for start in (0..samples).step_by(block) {
        block_events.clear();
        while next < sorted.len() && sorted[next].0 < start + block {
            block_events.push(NoteEvent {
                frame: (sorted[next].0 - start) as u32,
                kind: sorted[next].1,
            });
            next += 1;
        }
        guitar.process(&block_events, &mut left, &mut right);
        assert!(
            left.iter().chain(&right).all(|s| s.is_finite()),
            "not finite at {start}"
        );
        take.left.extend_from_slice(&left);
        take.right.extend_from_slice(&right);
        let mut meters = [0.0; MAX_METERS];
        guitar.meters(&mut meters);
        take.meters.push(meters);
    }
    take
}

/// One note held for `hold` seconds, `length` seconds long in all.
fn ring(settings: &[(&str, f32)], note: u8, v: f32, hold: f32, length: f32) -> Take {
    let mut guitar = with(settings);
    play(
        &mut *guitar,
        &[(0, on(note, v)), (seconds(hold), note_off(note))],
        seconds(length),
    )
}

fn rms_db(signal: &[f32]) -> f32 {
    let power = signal.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / signal.len() as f64;
    (10.0 * power.max(1e-30).log10()) as f32
}

/// Hann-windowed DFT at `hz` over `signal`: (amplitude, phase).
fn dft(signal: &[f32], hz: f32, rate: f32) -> (f64, f64) {
    let n = signal.len();
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &x) in signal.iter().enumerate() {
        let hann = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
        let arg = std::f64::consts::TAU * f64::from(hz) * i as f64 / f64::from(rate);
        re += f64::from(x) * hann * arg.cos();
        im -= f64::from(x) * hann * arg.sin();
    }
    (4.0 * (re * re + im * im).sqrt() / n as f64, im.atan2(re))
}

/// Frequency of the partial near `hz` in `signal`, from how far its phase
/// turns between two windows. Accurate to a small fraction of a cent.
fn partial_hz(signal: &[f32], hz: f32, rate: f32) -> f32 {
    partial_hz_over(signal, hz, rate, ((8.0 * rate / hz) as usize).max(2048))
}

fn partial_hz_over(signal: &[f32], hz: f32, rate: f32, window: usize) -> f32 {
    let window = window.min(signal.len() / 2);
    let hop = ((rate / (0.06 * hz)) as usize).clamp(1, signal.len() - window);
    let (_, first) = dft(&signal[..window], hz, rate);
    let (_, second) = dft(&signal[hop..hop + window], hz, rate);
    let expected = std::f64::consts::TAU * f64::from(hz) * hop as f64 / f64::from(rate);
    let error = (second - first - expected + std::f64::consts::PI)
        .rem_euclid(std::f64::consts::TAU)
        - std::f64::consts::PI;
    ((expected + error) * f64::from(rate) / (std::f64::consts::TAU * hop as f64)) as f32
}

fn cents(measured: f32, expected: f32) -> f32 {
    1200.0 * (measured / expected).log2()
}

fn note_hz(note: f32) -> f32 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// The same over a window `length` seconds long: longer separates partials
/// closer together.
fn partial_level_over(signal: &[f32], hz: f32, at: f32, length: f32) -> f64 {
    let window = seconds(length);
    let start = seconds(at).saturating_sub(window / 2);
    dft(&signal[start..start + window], hz, RATE).0
}

/// The partial near `hz`'s level, in dB, every 10 ms: the signal shifted
/// down by `hz` and smoothed over three periods of the fundamental `f0`, as
/// the recordings were measured.
fn envelope_db(signal: &[f32], hz: f32, f0: f32) -> Vec<f32> {
    let width = ((3.0 / f0 * RATE) as usize).max(seconds(0.02));
    let hop = seconds(0.01);
    let mut out = Vec::new();
    let mut start = 0;
    while start + width <= signal.len() {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &x) in signal[start..start + width].iter().enumerate() {
            let hann = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / width as f64).cos();
            let arg = std::f64::consts::TAU * f64::from(hz) * (start + i) as f64 / f64::from(RATE);
            re += f64::from(x) * hann * arg.cos();
            im -= f64::from(x) * hann * arg.sin();
        }
        out.push(
            (20.0
                * (2.0 * (re * re + im * im).sqrt() / width as f64)
                    .max(1e-12)
                    .log10()) as f32,
        );
        start += hop;
    }
    out
}

/// Seconds for an envelope (10 ms steps) to fall 60 dB, from a straight
/// line fitted between `from` and `to` seconds.
fn t60_of(envelope: &[f32], from: f32, to: f32) -> f32 {
    let (a, b) = (
        (from * 100.0) as usize,
        ((to * 100.0) as usize).min(envelope.len()),
    );
    let points: Vec<(f32, f32)> = (a..b).map(|i| (i as f32 * 0.01, envelope[i])).collect();
    let n = points.len() as f32;
    let (mx, my) = (
        points.iter().map(|p| p.0).sum::<f32>() / n,
        points.iter().map(|p| p.1).sum::<f32>() / n,
    );
    let slope = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f32>()
        / points.iter().map(|p| (p.0 - mx).powi(2)).sum::<f32>();
    -60.0 / slope
}

/// RMS deviation, in dB, of an envelope from a cubic fitted to it between
/// `from` and `to` seconds: how much it wobbles (beats).
fn wobble_db(envelope: &[f32], from: f32, to: f32) -> f32 {
    let (a, b) = (
        (from * 100.0) as usize,
        ((to * 100.0) as usize).min(envelope.len()),
    );
    let xs: Vec<f64> = (a..b).map(|i| i as f64 * 0.01).collect();
    let ys: Vec<f64> = (a..b).map(|i| f64::from(envelope[i])).collect();
    // Least squares on 1, x, x², x³.
    let mut ata = [[0.0f64; 4]; 4];
    let mut aty = [0.0f64; 4];
    for (&x, &y) in xs.iter().zip(&ys) {
        let row = [1.0, x, x * x, x * x * x];
        for i in 0..4 {
            aty[i] += row[i] * y;
            for j in 0..4 {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    // Gauss-Jordan elimination.
    for i in 0..4 {
        let pivot = ata[i][i];
        for value in &mut ata[i] {
            *value /= pivot;
        }
        aty[i] /= pivot;
        let row = ata[i];
        for k in (0..4).filter(|&k| k != i) {
            let f = ata[k][i];
            for (value, &above) in ata[k].iter_mut().zip(&row) {
                *value -= f * above;
            }
            aty[k] -= f * aty[i];
        }
    }
    let residual: f64 = xs
        .iter()
        .zip(&ys)
        .map(|(&x, &y)| (y - (aty[0] + aty[1] * x + aty[2] * x * x + aty[3] * x * x * x)).powi(2))
        .sum();
    (residual / xs.len() as f64).sqrt() as f32
}

fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}

/// Inharmonicity fitted to partials 2 to `last` of a note at `f0`, as
/// `(f_n / n)² = f0² (1 + B n²)`.
fn inharmonicity(signal: &[f32], f0: f32, last: usize) -> f32 {
    let mut points = Vec::new();
    for n in 2..=last {
        let n = n as f32;
        let guess = n * f0 * (1.0 + 1e-4 * n * n).sqrt();
        let hz = partial_hz_over(signal, guess, RATE, seconds(0.4));
        points.push((n * n, (hz / n).powi(2)));
    }
    let count = points.len() as f32;
    let (mx, my) = (
        points.iter().map(|p| p.0).sum::<f32>() / count,
        points.iter().map(|p| p.1).sum::<f32>() / count,
    );
    let slope = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f32>()
        / points.iter().map(|p| (p.0 - mx).powi(2)).sum::<f32>();
    let intercept = my - slope * mx;
    slope / intercept
}

/// The frequency between `low` and `high` where `signal` is strongest,
/// to a fraction of a hertz: for following a pitch that moves further
/// than [`partial_hz`] can follow.
fn strongest_between(signal: &[f32], low: f32, high: f32) -> f32 {
    let mut best = (0.0f64, low);
    let mut hz = low;
    while hz <= high {
        let level = dft(signal, hz, RATE).0;
        if level > best.0 {
            best = (level, hz);
        }
        hz += 0.25;
    }
    // A parabola through the peak and its neighbours.
    let (a, b, c) = (
        dft(signal, best.1 - 0.25, RATE).0,
        best.0,
        dft(signal, best.1 + 0.25, RATE).0,
    );
    let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
    best.1 + 0.25 * shift as f32
}

/// Energy-weighted mean frequency of `signal`'s spectrum, from the slope
/// and the level.
fn brightness(signal: &[f32]) -> f32 {
    let slope: Vec<f32> = signal.windows(2).map(|w| w[1] - w[0]).collect();
    10f32.powf((rms_db(&slope) - rms_db(signal)) / 20.0)
}

// ---------------------------------------------------------------- parameters

#[test]
fn shipped_parameters_keep_their_ids_and_places() {
    // Projects store parameters by id; the engine addresses them by index.
    let shipped = [
        "tone",
        "body",
        "pluck_position",
        "pick",
        "mute",
        "sustain",
        "strum",
        "vibrato",
        "amp",
        "drive",
        "amp_tone",
        "level",
    ];
    let params = fresh().params();
    for (index, id) in shipped.iter().enumerate() {
        assert_eq!(params[index].id, *id, "parameter {index}");
    }
    let mut ids: Vec<&str> = params.iter().map(|p| p.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), params.len(), "ids are unique");
    // The amp's labels are as shipped.
    let ParameterKind::Enum { labels, .. } = params[8].kind else {
        panic!("amp is a choice");
    };
    assert_eq!(labels, ["Off", "Clean", "Crunch", "Lead"]);
}

// ---------------------------------------------------------------- silence

#[test]
fn silent_without_notes_and_silent_again_after_them() {
    let mut guitar = fresh();
    let take = play(&mut *guitar, &[], seconds(0.5));
    assert_eq!(peak(&take.left), 0.0);
    assert_eq!(peak(&take.right), 0.0);

    for model in [STEEL, NYLON, ELECTRIC] {
        let mut guitar = with(&[("model", model)]);
        let take = play(
            &mut *guitar,
            &[
                (0, on(40, 1.0)),
                (0, on(64, 1.0)),
                (seconds(0.3), note_off(40)),
                (seconds(0.3), note_off(64)),
            ],
            seconds(4.0),
        );
        assert!(peak(&take.left[..seconds(0.3)]) > 0.05);
        let tail = &take.left[seconds(3.5)..];
        assert_eq!(peak(tail), 0.0, "model {model}: exactly silent once over");
        let meters = take.meters.last().unwrap();
        assert!((0..STRINGS).all(|s| meters[METER_NOTE + s] == -1.0));
    }
}

// ---------------------------------------------------------------- pitch

/// The pitch of the partial near `hz` over a long window from `from` to
/// `to` seconds, in cents from `hz`, as a tuner would read it: the two
/// polarizations beat, and over a short window near a null between them
/// the frequency seems to swing far from either.
fn tuning(signal: &[f32], hz: f32, from: f32, to: f32) -> f32 {
    cents(
        strongest_between(&signal[seconds(from)..seconds(to)], hz * 0.99, hz * 1.01),
        hz,
    )
}

#[test]
fn open_strings_are_in_tune_and_fretted_notes_pulled_a_little_sharp() {
    for model in [STEEL, NYLON, ELECTRIC] {
        // Open strings, once a soft pluck's pitch glide is over.
        for note in [40u8, 45, 50, 55, 59, 64] {
            let take = note_take(model, note, 0.4, 0.0);
            let off = tuning(&take, note_hz(f32::from(note)), 0.3, 1.9);
            // Within half the planes' split either side.
            assert!(off.abs() < 3.0, "model {model}, open {note}: {off} cents");
        }
        // Fretted notes (5th and 12th frets of the low E and G strings):
        // pressing the string onto the fret stretches it, a few cents.
        for note in [45u8, 52, 60, 67] {
            let take = note_take(model, note, 0.4, 0.0);
            let off = tuning(&take, note_hz(f32::from(note)), 0.3, 1.9);
            println!("model {model}: fretted {note} is {off:+.2} cents");
            assert!((-2.5..6.5).contains(&off), "model {model}, {note}: {off}");
        }
    }
    // Other sample rates.
    for rate in [44_100.0, 96_000.0] {
        for note in [28u8, 40, 64, 88] {
            let mut guitar = guitar_at(rate);
            set(&mut *guitar, "humanize", 0.0);
            guitar.reset();
            let samples = (1.5 * rate) as usize;
            let take = play(&mut *guitar, &[(0, on(note, 0.4))], samples);
            let hz = note_hz(f32::from(note));
            let start = (0.5 * rate) as usize;
            let off = cents(partial_hz(&take.left[start..], hz, rate), hz);
            assert!(off.abs() < 6.0, "note {note} at {rate} Hz: {off} cents");
        }
    }
}

/// A note of `model` at `velocity` with humanizing at `humanize`, held 2 s.
fn note_take(model: f32, note: u8, velocity: f32, humanize: f32) -> Vec<f32> {
    ring(
        &[("model", model), ("humanize", humanize)],
        note,
        velocity,
        2.0,
        2.0,
    )
    .left
}

#[test]
fn notes_beyond_the_neck_still_play_in_tune() {
    for (note, string, plays) in [
        (28u8, 0usize, 28u8),
        (100, 5, 100),
        (24, 0, 24),
        (127, 5, 115),
    ] {
        let take = ring(&[("humanize", 0.0)], note, 0.4, 1.5, 1.5);
        let meters = take.meters_at(seconds(0.1));
        assert_eq!(meters[METER_NOTE + string], f32::from(plays), "note {note}");
        let off = tuning(&take.left, note_hz(f32::from(plays)), 0.3, 1.4);
        println!("note {note} plays {plays} on string {string}, {off:.2} cents");
        assert!(off.abs() < 6.0);
    }
}

#[test]
fn partials_are_stretched_as_the_strings_stiffness_says() {
    // Measured on recordings, partials 2 to 12 fitted the same way:
    // electric low E 1.57e-4 (IDMT Strat) and 1.69e-4 (ntnvsl); electric
    // A 0.88e-4 to 0.92e-4; nylon low E 1.2e-5 to 1.6e-5; nylon B 7.2e-5
    // to 7.4e-5; steel low E 0.56e-4 (FenrirFangs) to 2.1e-4 (NSynth).
    let cases = [
        (ELECTRIC, 40u8, 1.6e-4f32),
        (ELECTRIC, 45, 0.9e-4),
        (NYLON, 40, 1.4e-5),
        (NYLON, 59, 7.3e-5),
        (STEEL, 40, 1.0e-4),
    ];
    for (model, note, measured) in cases {
        let take = note_take(model, note, 0.6, 0.0);
        let f0 = note_hz(f32::from(note));
        let b = inharmonicity(&take[seconds(0.3)..seconds(1.5)], f0, 12);
        println!("model {model} note {note}: B {b:.2e} (recordings {measured:.2e})");
        assert!(
            (0.55..1.6).contains(&(b / measured)),
            "model {model} note {note}: {b:.2e} vs {measured:.2e}"
        );
    }
}

// ---------------------------------------------------------------- decay

/// The open A's (or `note`'s) partials `range` of `model`: their decay
/// times early (0.04 to 0.4 s) and late (0.8 to 3 s), and how much their
/// envelopes wobble, medians over the partials.
fn decays(model: f32, note: u8, range: std::ops::RangeInclusive<usize>) -> (f32, f32, f32) {
    let take = ring(
        &[("model", model), ("humanize", 0.0), ("noises", 0.0)],
        note,
        0.8,
        3.5,
        3.5,
    );
    let signal = take.mono();
    let f0 = note_hz(f32::from(note));
    let mut early = Vec::new();
    let mut late = Vec::new();
    let mut wobble = Vec::new();
    for n in range {
        let hz = n as f32 * f0;
        let envelope = envelope_db(&signal, hz, f0);
        early.push(t60_of(&envelope, 0.04, 0.4));
        late.push(t60_of(&envelope, 0.8, 3.0));
        wobble.push(wobble_db(&envelope, 0.1, 3.0));
    }
    (median(early), median(late), median(wobble))
}

#[test]
fn partials_decay_as_on_recorded_guitars() {
    // (model, note, partials, late T60 range of the low partials (1-4) and
    // of partials 5-12) from the recordings: nylon A 9-19 s and 6.1 s;
    // nylon high E 3.5-3.9 s and 3.7-4.7 s; Strat A 9.8-11 s and 3.9-4.7 s;
    // Strat high E 6.3-7.3 s and 3.8-5.2 s.
    let cases = [
        (NYLON, 45u8, (5.0, 20.0), (2.5, 8.0)),
        (NYLON, 64, (2.0, 6.0), (2.0, 6.0)),
        (ELECTRIC, 45, (5.0, 15.0), (2.5, 9.0)),
        (ELECTRIC, 64, (3.5, 10.0), (2.5, 7.0)),
        (STEEL, 45, (4.0, 15.0), (2.5, 9.0)),
    ];
    for (model, note, low_range, mid_range) in cases {
        let (_, low, _) = decays(model, note, 1..=4);
        let (_, mid, _) = decays(model, note, 5..=12);
        println!("model {model} note {note}: low partials T60 {low:.1} s, mid {mid:.1} s");
        assert!(
            (low_range.0..low_range.1).contains(&low),
            "model {model} note {note}: low {low}"
        );
        assert!(
            (mid_range.0..mid_range.1).contains(&mid),
            "model {model} note {note}: mid {mid}"
        );
    }
}

#[test]
fn decay_comes_in_two_stages_and_the_partials_beat() {
    // Recordings: partials 5-12 die about twice as fast in the first
    // fraction of a second as later (early/late 0.3 to 0.6), and their
    // envelopes wobble by 0.5 to 4.8 dB around a smooth curve.
    for model in [STEEL, NYLON, ELECTRIC] {
        let (early, late, wobble) = decays(model, 45, 5..=12);
        println!(
            "model {model}: partials 5-12 early T60 {early:.2} s, late {late:.2} s, wobble {wobble:.2} dB"
        );
        assert!(early < 0.8 * late, "model {model}: {early} vs {late}");
        assert!((0.3..6.0).contains(&wobble), "model {model}: {wobble}");
    }
}

#[test]
fn a_hard_pluck_starts_sharp_and_settles() {
    // Recordings: 2 to 5 cents sharp in the first 0.15 s on nylon, 1 to
    // 2.5 on an electric, then settling.
    for (model, most) in [(NYLON, 8.0), (ELECTRIC, 4.0)] {
        let glide = |v: f32| {
            let take = note_take(model, 45, v, 0.0);
            let hz = note_hz(45.0);
            let early = partial_hz_over(&take[seconds(0.05)..], hz, RATE, seconds(0.1));
            let late = partial_hz_over(&take[seconds(1.6)..], hz, RATE, seconds(0.1));
            cents(early, late)
        };
        let (hard, soft) = (glide(1.0), glide(0.3));
        println!("model {model}: hard pluck starts {hard:.2} cents sharp, soft {soft:.2}");
        assert!(hard > 0.8 && hard < most, "model {model}: {hard}");
        assert!(soft < 0.5 * hard, "model {model}: {soft}");
    }
}

#[test]
fn sustain_and_palm_mute_set_the_decay_time() {
    // The open A's partials 1 to 4, late, with no resonance, on the
    // electric (its polarizations are tuned closest, so beating slows the
    // fit least): the median, so what beating there is averages out.
    let decay = |sustain: f32, mute: f32| {
        let take = ring(
            &[
                ("model", ELECTRIC),
                ("sustain", sustain),
                ("mute", mute),
                ("humanize", 0.0),
                ("resonance", 0.0),
            ],
            45,
            0.8,
            3.5,
            3.5,
        );
        assert_eq!(take.meters_at(seconds(0.1))[METER_NOTE + 1], 45.0);
        median(
            (1..=4)
                .map(|n| {
                    let envelope = envelope_db(&take.left, 110.0 * n as f32, 110.0);
                    if mute > 0.5 {
                        t60_of(&envelope, 0.05, 0.25)
                    } else {
                        t60_of(&envelope, 0.6, 3.3)
                    }
                })
                .collect(),
        )
    };
    let short = decay(4.0, 0.0);
    let long = decay(16.0, 0.0);
    let muted = decay(10.0, 1.0);
    println!(
        "open A, partials 1-4, to -60 dB: sustain 4 s -> {short:.2} s, 16 s -> {long:.2} s, palm mute -> {muted:.3} s"
    );
    assert!((2.8..5.5).contains(&(long / short)), "{short} {long}");
    assert!((2.5..6.0).contains(&short), "{short}");
    assert!(muted < 0.6, "palm mute: {muted}");
}

// ---------------------------------------------------------------- body, plucks, pickups

#[test]
fn the_bodies_resonate_where_guitars_do() {
    // Fletcher & Rossing 1998 and Christensen & Vistisen 1980: the air
    // mode near 100 Hz, the top's first mode near 200 Hz.
    let hz: Vec<f32> = (600..2600).map(|i| i as f32 * 0.1).collect();
    for (model, air, top) in [(STEEL, 101.0, 186.0), (NYLON, 96.0, 193.0)] {
        let mut db = vec![0.0; hz.len()];
        body_response_db(model as usize, 0.6, RATE, &hz, &mut db);
        let peak_near = |target: f32| {
            let (mut best, mut at) = (f32::MIN, 0.0);
            for (&h, &d) in hz.iter().zip(&db) {
                if (h / target - 1.0).abs() < 0.12 && d > best {
                    (best, at) = (d, h);
                }
            }
            at
        };
        let (a, t) = (peak_near(air), peak_near(top));
        println!("model {model}: air mode at {a:.1} Hz, top at {t:.1} Hz");
        assert!((a / air - 1.0).abs() < 0.04 && (t / top - 1.0).abs() < 0.04);
    }
    let mut flat = vec![0.0; 3];
    body_response_db(
        ELECTRIC as usize,
        0.6,
        RATE,
        &[100.0, 200.0, 1000.0],
        &mut flat,
    );
    assert!(flat.iter().all(|d| d.abs() < 0.5), "{flat:?}");
}

#[test]
fn the_pluck_position_leaves_notches_in_the_spectrum() {
    // Plucked a quarter of the way along (pluck position 0.5 puts the
    // pick at 0.25 of the open string), every fourth partial is missing.
    let take = ring(
        &[
            ("pluck_position", 0.5),
            ("humanize", 0.0),
            ("body", 0.0),
            ("resonance", 0.0),
        ],
        45,
        0.8,
        1.0,
        1.0,
    );
    let level = |n: f32| partial_level_over(&take.left, 110.0 * n, 0.3, 0.4);
    let notch = level(4.0);
    let around = level(3.0).min(level(5.0));
    let db = 20.0 * (around / notch).log10();
    println!("4th partial {db:.1} dB under its neighbours");
    assert!(db > 12.0, "{db}");
}

#[test]
fn pickups_hear_the_string_where_they_sit() {
    let pickup = |position: f32| {
        let take = ring(
            &[("model", ELECTRIC), ("pickup", position), ("humanize", 0.0)],
            45,
            0.8,
            1.0,
            1.0,
        );
        brightness(&take.left[seconds(0.05)..seconds(0.5)])
    };
    let (neck, middle, bridge) = (pickup(0.0), pickup(2.0), pickup(4.0));
    println!("brightness: neck {neck:.3}, middle {middle:.3}, bridge {bridge:.3}");
    assert!(neck < middle && middle < bridge);
    // The neck pickup, about a quarter of the open string from the saddle,
    // barely hears the 4th partial.
    let take = ring(
        &[
            ("model", ELECTRIC),
            ("pickup", 0.0),
            ("humanize", 0.0),
            ("pluck_position", 0.15),
        ],
        45,
        0.8,
        1.0,
        1.0,
    );
    let level = |n: f32| partial_level_over(&take.left, 110.0 * n, 0.3, 0.4);
    let db = 20.0 * (level(3.0).min(level(5.0)) / level(4.0)).log10();
    println!("neck pickup: 4th partial {db:.1} dB under its neighbours");
    assert!(db > 8.0, "{db}");
}

#[test]
fn each_model_sounds_like_itself() {
    let sound = |model: f32| {
        let take = ring(&[("model", model), ("humanize", 0.0)], 52, 0.8, 1.0, 1.0);
        brightness(&take.left[seconds(0.05)..seconds(0.6)])
    };
    let (steel, nylon, electric) = (sound(STEEL), sound(NYLON), sound(ELECTRIC));
    println!("brightness: steel {steel:.3}, nylon {nylon:.3}, electric {electric:.3}");
    assert!(nylon < steel, "nylon is mellower than steel");
    assert!(nylon < electric);
}

// ---------------------------------------------------------------- strings

/// Notes and frets per string a moment after `notes` start together.
fn shape(notes: &[u8]) -> ([f32; STRINGS], [f32; STRINGS]) {
    let mut guitar = fresh();
    let events: Vec<_> = notes.iter().map(|&n| (0, on(n, 0.8))).collect();
    let take = play(&mut *guitar, &events, seconds(0.15));
    let meters = take.meters.last().unwrap();
    (
        std::array::from_fn(|s| meters[METER_NOTE + s]),
        std::array::from_fn(|s| meters[METER_FRET + s]),
    )
}

#[test]
fn chords_take_the_strings_a_guitarist_would() {
    let (notes, frets) = shape(&[40, 47, 52, 55, 59, 64]);
    assert_eq!(notes, [40.0, 47.0, 52.0, 55.0, 59.0, 64.0]);
    assert_eq!(frets, [0.0, 2.0, 2.0, 0.0, 0.0, 0.0], "E minor");
    let (notes, frets) = shape(&[48, 52, 55, 60, 64]);
    assert_eq!(notes, [-1.0, 48.0, 52.0, 55.0, 60.0, 64.0]);
    assert_eq!(frets, [0.0, 3.0, 2.0, 0.0, 1.0, 0.0], "C major");
    let (_, frets) = shape(&[45, 52, 57, 61, 64, 69]);
    assert_eq!(frets, [5.0, 7.0, 7.0, 6.0, 5.0, 5.0], "A major barre");
}

#[test]
fn picking_a_ringing_string_again_cuts_its_note() {
    let mut guitar = with(&[("humanize", 0.0), ("resonance", 0.0)]);
    let repick = seconds(0.5);
    // G#2 is on the low E string at the fourth fret; E2 can only be played
    // there, so it cuts G#2 even though G#2 is still held.
    let take = play(
        &mut *guitar,
        &[(0, on(44, 0.8)), (repick, on(40, 0.8))],
        seconds(1.2),
    );
    assert_eq!(take.meters_at(repick - BLOCK)[METER_NOTE], 44.0);
    assert_eq!(take.meters_at(repick + seconds(0.05))[METER_NOTE], 40.0);
    // G#2's second partial (208 Hz, well clear of E2's 165 and 247): in a
    // window ending before the re-pick, and one starting 150 ms after it.
    let g_sharp = 2.0 * note_hz(44.0);
    let before = partial_level_over(&take.left, g_sharp, 0.35, 0.25);
    let after = partial_level_over(&take.left, g_sharp, 0.8, 0.25);
    let drop = 20.0 * (before / after).log10();
    println!("G#2 fell {drop:.1} dB within 150 ms of its string being picked again");
    assert!(drop > 35.0, "{drop}");
    let active = (0..STRINGS)
        .filter(|s| take.meters.last().unwrap()[METER_NOTE + s] >= 0.0)
        .count();
    assert_eq!(active, 1);

    // The same note again goes to the same string.
    let mut guitar = fresh();
    let take = play(
        &mut *guitar,
        &[(0, on(57, 0.8)), (seconds(0.2), on(57, 0.8))],
        seconds(0.4),
    );
    let meters = take.meters.last().unwrap();
    let strings: Vec<usize> = (0..STRINGS)
        .filter(|s| meters[METER_NOTE + s] == 57.0)
        .collect();
    assert_eq!(strings.len(), 1);
}

#[test]
fn idle_strings_ring_along_with_the_played_ones() {
    // E2 played alone: with resonance, the open A, D, G... strings pick
    // up its vibration through the bridge and ring at their own pitches,
    // which E2 doesn't have (110 Hz, the A string's fundamental).
    let at_110 = |resonance: f32| {
        let take = ring(
            &[("resonance", resonance), ("humanize", 0.0)],
            40,
            1.0,
            2.0,
            2.0,
        );
        partial_level_over(&take.left, 110.0, 1.0, 0.5)
    };
    let (with, without) = (at_110(1.0), at_110(0.0));
    let db = 20.0 * (with / without).log10();
    println!("the open A string rings along {db:.1} dB louder with resonance");
    assert!(db > 10.0, "{db}");
}

// ---------------------------------------------------------------- strum

/// Onset sample of each string's note in a chord played at `velocity`.
fn strum_onsets(settings: &[(&str, f32)], velocity: f32) -> (Vec<usize>, f32) {
    let chord = [40, 47, 52, 55, 59, 64];
    let mut guitar = with(settings);
    let events: Vec<_> = chord.iter().map(|&n| (0, on(n, velocity))).collect();
    let take = play(&mut *guitar, &events, seconds(0.4));
    let times = (0..STRINGS)
        .map(|s| take.onset(s, chord[s]).expect("every string sounds"))
        .collect();
    (times, take.meters.last().unwrap()[METER_STRUM])
}

#[test]
fn strums_go_the_stroke_direction_faster_when_harder() {
    let fixed = |direction: f32, velocity: f32| {
        strum_onsets(
            &[
                ("strum", 100.0),
                ("strum_direction", direction),
                ("humanize", 0.0),
            ],
            velocity,
        )
    };
    let (down, direction) = fixed(2.0, 0.6);
    println!("down-stroke onsets (samples): {down:?}");
    assert_eq!(direction, 1.0);
    assert!(down.windows(2).all(|w| w[0] < w[1]), "low string first");
    let (up, direction) = fixed(3.0, 0.6);
    assert_eq!(direction, -1.0);
    assert!(
        up.windows(2).all(|w| w[0] > w[1]),
        "high string first: {up:?}"
    );
    // Harder is faster: the strum knob is the time at a medium velocity.
    let spread = |onsets: &[usize]| (onsets[5] as f32 - onsets[0] as f32).abs() / RATE;
    let (soft, _) = fixed(2.0, 0.3);
    let (hard, _) = fixed(2.0, 1.0);
    println!(
        "spread: soft {:.3} s, medium {:.3} s, hard {:.3} s",
        spread(&soft),
        spread(&down),
        spread(&hard)
    );
    assert!(spread(&hard) < spread(&down) && spread(&down) < spread(&soft));
    assert!((0.06..0.14).contains(&spread(&down)), "{}", spread(&down));
    // With no strum time, all at once.
    let (together, _) = strum_onsets(&[("strum", 0.0), ("humanize", 0.0)], 0.9);
    assert!(together.iter().all(|&t| t == together[0]));
    // By velocity, as the guitar used to: soft chords are up-strokes.
    let (_, direction) = strum_onsets(&[("strum_direction", 1.0)], 0.3);
    assert_eq!(direction, -1.0);
}

#[test]
fn a_human_strum_is_uneven_and_the_same_every_time() {
    let settings = [
        ("strum", 100.0),
        ("strum_direction", 2.0),
        ("humanize", 1.0),
    ];
    let (first, _) = strum_onsets(&settings, 0.6);
    let (again, _) = strum_onsets(&settings, 0.6);
    assert_eq!(first, again, "deterministic");
    let gaps: Vec<i64> = first
        .windows(2)
        .map(|w| w[1] as i64 - w[0] as i64)
        .collect();
    let (least, most) = (gaps.iter().min().unwrap(), gaps.iter().max().unwrap());
    println!("humanized gaps between strings: {gaps:?}");
    assert!(most - least >= 2 * BLOCK as i64, "uneven: {gaps:?}");
    assert!(gaps.iter().all(|&g| g >= 0), "still in order");
}

#[test]
fn the_strumming_hand_goes_down_on_the_beat_and_up_between() {
    // At 120 bpm an eighth is 0.25 s: D D U U D U.
    let mut guitar = with(&[("strum_direction", 0.0)]);
    guitar.set_tempo(120.0);
    let chord = [43u8, 47, 50, 55, 59, 67];
    let pattern = [0.0f32, 0.5, 0.75, 1.25, 1.5, 1.75];
    let mut events = Vec::new();
    for &at in &pattern {
        for &n in &chord {
            events.push((seconds(at), on(n, 0.7)));
            events.push((seconds(at + 0.2), note_off(n)));
        }
    }
    let take = play(&mut *guitar, &events, seconds(2.1));
    let directions: Vec<f32> = pattern
        .iter()
        .map(|&at| take.meters_at(seconds(at + 0.1))[METER_STRUM])
        .collect();
    assert_eq!(directions, [1.0, 1.0, -1.0, -1.0, 1.0, -1.0]);
}

#[test]
fn notes_a_moment_apart_join_the_strum() {
    // A keyboard player's chord: notes 0, 8 and 16 ms apart.
    let mut guitar = with(&[("strum", 60.0), ("strum_direction", 2.0), ("humanize", 0.0)]);
    let take = play(
        &mut *guitar,
        &[
            (0, on(40, 0.7)),
            (seconds(0.008), on(52, 0.7)),
            (seconds(0.016), on(47, 0.7)),
        ],
        seconds(0.3),
    );
    let low = take.onset(0, 40).unwrap();
    let middle = take.onset(1, 47).unwrap();
    let high = take.onset(2, 52).unwrap();
    assert!(low < middle && middle <= high, "{low} {middle} {high}");
}

#[test]
fn a_strum_is_heard_string_by_string() {
    // The audio, not just the meters: with a slow strum the low E's attack
    // comes well before the high E's.
    let mut guitar = with(&[
        ("strum", 200.0),
        ("strum_direction", 2.0),
        ("body", 0.0),
        ("humanize", 0.0),
        ("resonance", 0.0),
    ]);
    // E2 on the low string, F#4 (between E2's 4th and 5th harmonics) on
    // the high one.
    let take = play(
        &mut *guitar,
        &[(0, on(40, 0.5)), (0, on(66, 0.5))],
        seconds(0.6),
    );
    let early = partial_level_over(&take.left, note_hz(66.0), 0.08, 0.08);
    let late = partial_level_over(&take.left, note_hz(66.0), 0.45, 0.08);
    println!("F#4 at 80 ms: {early:.5}, at 450 ms: {late:.5}");
    assert!(late > 20.0 * early, "{early} then {late}");
}

// ---------------------------------------------------------------- legato, bends, vibrato

#[test]
fn a_note_overlapping_a_held_one_slides_on_the_same_string() {
    let mut guitar = with(&[("humanize", 0.0)]);
    let slide = seconds(0.3);
    // A3 on the G string, then C4 while A3 is still held: a 3-fret slide.
    let take = play(
        &mut *guitar,
        &[
            (0, on(57, 0.8)),
            (slide, on(60, 0.8)),
            (slide + seconds(0.05), note_off(57)),
        ],
        seconds(1.2),
    );
    let string = 3;
    assert_eq!(take.meters_at(slide - BLOCK)[METER_FRET + string], 2.0);
    let frets: Vec<f32> = take.meters[slide / BLOCK..(slide + seconds(0.1)) / BLOCK]
        .iter()
        .map(|m| m[METER_FRET + string])
        .collect();
    assert!(
        frets.iter().any(|&f| f > 2.5 && f < 4.5),
        "passes the frets between: {frets:?}"
    );
    let meters = take.meters.last().unwrap();
    assert_eq!(meters[METER_NOTE + string], 60.0);
    assert_eq!(
        meters[METER_HELD + string],
        1.0,
        "A3's note-off doesn't stop C4"
    );
    let others = (0..STRINGS)
        .filter(|&s| s != string && meters[METER_NOTE + s] >= 0.0)
        .count();
    assert_eq!(others, 0, "no string was picked");
    // Mid-slide the pitch is between the two notes.
    let middle = partial_hz_over(&take.left[slide + seconds(0.03)..], 255.0, RATE, 1024);
    assert!((note_hz(57.0)..note_hz(60.0)).contains(&middle), "{middle}");
    let off = tuning(&take.left, note_hz(60.0), 0.5, 1.2);
    assert!((-1.0..6.0).contains(&off), "lands in tune: {off}");
}

#[test]
fn hammer_ons_and_pull_offs_stay_on_the_string() {
    let mut guitar = fresh();
    let take = play(
        &mut *guitar,
        &[
            (0, on(57, 0.8)),
            (seconds(0.2), on(59, 0.8)),
            (seconds(0.25), note_off(57)),
            (seconds(0.4), on(57, 0.8)),
            (seconds(0.45), note_off(59)),
        ],
        seconds(0.8),
    );
    let at = |t: f32| take.meters_at(seconds(t))[METER_NOTE + 3];
    assert_eq!((at(0.1), at(0.3), at(0.6)), (57.0, 59.0, 57.0));
    let level = take.meters_at(seconds(0.3))[METER_LEVEL + 3];
    assert!(
        level > 0.05,
        "the hammer-on keeps the string sounding: {level}"
    );
}

#[test]
fn chords_and_far_notes_are_picked_not_slid() {
    let mut guitar = fresh();
    let take = play(
        &mut *guitar,
        &[(0, on(45, 0.8)), (seconds(0.2), on(64, 0.8))],
        seconds(0.4),
    );
    let meters = take.meters.last().unwrap();
    assert_eq!(meters[METER_NOTE + 1], 45.0, "A2 still rings");
    assert_eq!(
        meters[METER_NOTE + 5],
        64.0,
        "E4, too far to slide, is picked"
    );
    // With legato off, even a near note is picked.
    let mut guitar = with(&[("legato", 3.0)]);
    let take = play(
        &mut *guitar,
        &[(0, on(57, 0.8)), (seconds(0.2), on(59, 0.8))],
        seconds(0.4),
    );
    let meters = take.meters.last().unwrap();
    assert_eq!(
        (meters[METER_NOTE + 3], meters[METER_NOTE + 4]),
        (57.0, 59.0),
        "B3 picked on the open B while A3 rings"
    );
}

#[test]
fn bends_move_only_the_strings_a_finger_holds() {
    // E4 picked and let go (it rings on, released), then A2 held and bent
    // a whole tone: only A2 moves.
    let mut guitar = with(&[("humanize", 0.0), ("resonance", 0.0)]);
    let bend = seconds(0.6);
    let take = play(
        &mut *guitar,
        &[
            (0, on(64, 0.9)),
            (seconds(0.05), note_off(64)),
            (seconds(0.1), on(45, 0.9)),
            (bend, NoteEventKind::PitchBend(8191)),
        ],
        seconds(1.2),
    );
    let a2 = note_hz(45.0);
    let before = partial_hz(&take.left[seconds(0.2)..seconds(0.58)], a2, RATE);
    let up = a2 * (2.0f32 / 12.0).exp2();
    let bent = partial_hz(&take.left[seconds(0.7)..], up, RATE);
    println!(
        "A2 bent: {:.2} cents before, {:.2} cents from a whole tone up",
        cents(before, a2),
        cents(bent, up)
    );
    assert!(cents(before, a2).abs() < 6.0);
    assert!(cents(bent, up).abs() < 6.0);
    // E4 (let go) is quiet by now but where it was: its partial at 330 Hz
    // hasn't moved to 370.
    let e4 = note_hz(64.0);
    let stayed = partial_level_over(&take.left, e4, 0.15, 0.08);
    assert!(stayed > 0.0);
    assert!((take.meters_at(bend + seconds(0.1))[METER_BEND] - 2.0).abs() < 1e-3);
}

#[test]
fn a_bend_follows_the_wheels_curve() {
    // The wheel pushed up over 200 ms, held, let back over 200 ms: the
    // pitch follows within 10 ms.
    let mut guitar = with(&[("humanize", 0.0)]);
    let mut events = vec![(0, on(57, 0.9))];
    for k in 0..=20 {
        let t = k as f32 / 20.0;
        let up = (0.5 - 0.5 * (std::f32::consts::PI * t).cos()) * 8191.0;
        events.push((seconds(0.3 + 0.2 * t), NoteEventKind::PitchBend(up as i16)));
        events.push((
            seconds(0.8 + 0.2 * t),
            NoteEventKind::PitchBend((8191.0 - up) as i16),
        ));
    }
    let take = play(&mut *guitar, &events, seconds(1.3));
    let pitch = |at: f32| {
        let hz = note_hz(57.0);
        let window = &take.left[seconds(at) - 1024..seconds(at) + 1024];
        cents(strongest_between(window, hz * 0.97, hz * 1.15), hz)
    };
    let curve: Vec<f32> = [0.25, 0.4, 0.6, 0.9, 1.2]
        .iter()
        .map(|&t| pitch(t))
        .collect();
    println!("pitch through the bend (cents): {curve:?}");
    assert!(curve[0].abs() < 30.0);
    assert!((60.0..140.0).contains(&curve[1]), "halfway up: {curve:?}");
    assert!((170.0..225.0).contains(&curve[2]), "up: {curve:?}");
    assert!((60.0..140.0).contains(&curve[3]), "halfway down: {curve:?}");
    assert!(curve[4].abs() < 30.0, "back: {curve:?}");
}

#[test]
fn vibrato_bends_up_on_steel_and_rocks_both_ways_on_nylon() {
    for (model, low, high) in [(STEEL, 0.0, 0.5), (NYLON, -0.25, 0.25)] {
        let mut guitar = with(&[("model", model), ("vibrato", 50.0)]);
        let take = play(
            &mut *guitar,
            &[(0, control(MOD_WHEEL, 127)), (0, on(57, 0.8))],
            seconds(1.5),
        );
        let swing: Vec<f32> = take.meters[seconds(0.2) / BLOCK..]
            .iter()
            .map(|m| m[METER_BEND])
            .collect();
        let (lowest, highest) = swing
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)));
        println!("model {model}: vibrato swings {lowest:.3} to {highest:.3} semitones");
        assert!((lowest - low).abs() < 0.02 && (highest - high).abs() < 0.02);
    }
}

// ---------------------------------------------------------------- articulations

#[test]
fn palm_mute_by_knob_key_switch_or_pedal_damps_and_darkens() {
    let open = ring(&[("humanize", 0.0)], 40, 0.9, 1.0, 1.0);
    type Way<'a> = (&'a str, Vec<(usize, NoteEventKind)>, Vec<(&'a str, f32)>);
    let ways: [Way; 3] = [
        ("knob", vec![], vec![("mute", 0.8)]),
        ("key-switch", vec![(0, switch(1))], vec![]),
        ("soft pedal", vec![(0, control(SOFT_PEDAL, 102))], vec![]),
    ];
    let open_envelope = envelope_db(&open.left, 82.41, 82.41);
    let open_decay = t60_of(&open_envelope, 0.1, 0.5);
    for (way, mut events, mut settings) in ways {
        settings.push(("humanize", 0.0));
        let mut guitar = with(&settings);
        events.push((seconds(0.05), on(40, 0.9)));
        let take = play(&mut *guitar, &events, seconds(1.0));
        let envelope = envelope_db(&take.left, 82.41, 82.41);
        let decay = t60_of(&envelope, 0.15, 0.5);
        let dark = brightness(&take.left[seconds(0.06)..seconds(0.3)])
            / brightness(&open.left[seconds(0.01)..seconds(0.25)]);
        let meters = take.meters_at(seconds(0.15));
        println!(
            "palm mute by {way}: T60 {decay:.2} s (open {open_decay:.1} s), brightness x{dark:.2}, mute meter {:.2}",
            meters[METER_MUTE]
        );
        assert!(decay < 0.15 * open_decay, "{way}: {decay}");
        assert!(dark < 0.8, "{way}: {dark}");
        assert!(meters[METER_MUTE] > 0.7, "{way}");
        assert_eq!(meters[METER_KIND], 1.0, "{way}: a palm-muted note");
    }
    // The key-switch latches until the open one.
    let mut guitar = fresh();
    let take = play(
        &mut *guitar,
        &[(0, switch(1)), (seconds(0.1), switch(0))],
        seconds(0.2),
    );
    assert_eq!(take.meters_at(seconds(0.05))[METER_ARTICULATION], 1.0);
    assert_eq!(take.meters.last().unwrap()[METER_ARTICULATION], 0.0);
}

#[test]
fn harmonics_ring_at_their_partial_without_the_fundamental() {
    // D0 then E3: the low E's 12th-fret harmonic, an octave over E2.
    let mut guitar = with(&[("humanize", 0.0), ("resonance", 0.0)]);
    let take = play(
        &mut *guitar,
        &[(0, switch(2)), (seconds(0.05), on(52, 0.9))],
        seconds(1.5),
    );
    let meters = take.meters_at(seconds(0.3));
    assert_eq!(meters[METER_KIND], 2.0, "a harmonic on the low E");
    assert_eq!(meters[METER_FRET], 12.0, "touched over the 12th fret");
    let harmonic = partial_level_over(&take.left, 164.8, 0.6, 0.4);
    let fundamental = partial_level_over(&take.left, 82.41, 0.6, 0.4);
    let db = 20.0 * (harmonic / fundamental).log10();
    println!("harmonic: 165 Hz {db:.1} dB over 82 Hz");
    assert!(db > 25.0, "{db}");
    // In tune with the open string's second partial.
    let hz = partial_hz(&take.left[seconds(0.5)..], 164.8, RATE);
    assert!(cents(hz, 2.0 * 82.41).abs() < 6.0, "{hz}");
}

#[test]
fn dead_notes_are_short_thuds() {
    let mut guitar = with(&[("humanize", 0.0)]);
    let take = play(
        &mut *guitar,
        &[(0, switch(3)), (seconds(0.05), on(45, 0.9))],
        seconds(0.6),
    );
    assert_eq!(take.meters_at(seconds(0.06))[METER_KIND + 1], 3.0);
    let attack = rms_db(&take.left[seconds(0.05)..seconds(0.07)]);
    let after = rms_db(&take.left[seconds(0.25)..seconds(0.27)]);
    println!(
        "dead note: {:.1} dB down 200 ms after the pick",
        attack - after
    );
    assert!(attack - after > 35.0);
}

#[test]
fn key_switches_can_be_turned_off() {
    let mut guitar = with(&[("key_switches", 0.0)]);
    let take = play(
        &mut *guitar,
        &[(0, on(KEY_SWITCH_FIRST + 1, 0.8))],
        seconds(0.2),
    );
    assert!(peak(&take.left) > 0.01, "played as a note");
    assert_eq!(take.meters.last().unwrap()[METER_ARTICULATION], 0.0);
    let mut guitar = fresh();
    let take = play(
        &mut *guitar,
        &[(0, on(KEY_SWITCH_FIRST + 1, 0.8))],
        seconds(0.2),
    );
    assert_eq!(peak(&take.left), 0.0, "a key-switch makes no sound");
}

// ---------------------------------------------------------------- release, clicks

#[test]
fn releasing_a_note_damps_it_quickly_but_not_abruptly() {
    let mut guitar = with(&[("noises", 0.0)]);
    let release = seconds(0.5);
    let take = play(
        &mut *guitar,
        &[(0, on(52, 0.8)), (release, note_off(52))],
        seconds(2.0),
    );
    let ms = |m: f32| seconds(m / 1000.0);
    let before = rms_db(&take.left[release - ms(10.0)..release]);
    let just_after = rms_db(&take.left[release..release + ms(10.0)]);
    let later = rms_db(&take.left[release + ms(300.0)..release + ms(310.0)]);
    println!(
        "release: {before:.1} dB before, {just_after:.1} dB in the first 10 ms, {later:.1} dB 300 ms on"
    );
    assert!(before - just_after < 3.0, "no sudden cut");
    assert!(before - later > 30.0, "damped within 300 ms");
    let string = 2; // E3 on the D string
    assert_eq!(take.meters_at(release - BLOCK)[METER_HELD + string], 1.0);
    assert_eq!(take.meters_at(release + ms(20.0))[METER_HELD + string], 0.0);
    assert_eq!(take.meters_at(seconds(1.9))[METER_NOTE + string], -1.0);
}

#[test]
fn a_finger_lifting_off_is_heard() {
    // Renders repeat exactly, so the difference with and without noises is
    // the noise alone: a quiet thump as the finger lifts, nothing before.
    let take = |noises: f32| ring(&[("noises", noises), ("humanize", 0.0)], 47, 0.8, 0.5, 0.7).left;
    let (quiet, noisy) = (take(0.0), take(1.0));
    let difference: Vec<f32> = noisy.iter().zip(&quiet).map(|(a, b)| a - b).collect();
    assert_eq!(peak(&difference[..seconds(0.5)]), 0.0);
    let thump = rms_db(&difference[seconds(0.5)..seconds(0.56)]);
    let note = rms_db(&quiet[seconds(0.44)..seconds(0.5)]);
    println!("finger noise {:.1} dB under the note", note - thump);
    assert!((15.0..50.0).contains(&(note - thump)), "{note} {thump}");
}

#[test]
fn no_clicks_on_retrigger_release_or_slide() {
    // The steepest sample-to-sample step around each event, against the
    // steepest while the note simply rings just before it.
    let mut guitar = with(&[("noises", 0.0), ("humanize", 0.0)]);
    let (repick, release, slide) = (seconds(0.4), seconds(1.0), seconds(1.4));
    let take = play(
        &mut *guitar,
        &[
            (0, on(57, 0.8)),
            (repick, on(57, 0.8)),
            (release, note_off(57)),
            (seconds(1.2), on(57, 0.8)),
            (slide, on(62, 0.8)),
        ],
        seconds(1.8),
    );
    let signal = take.mono();
    let ringing = |at: usize| max_step(&signal[at - seconds(0.05)..at]);
    // A re-pick is a new attack, but the old note mustn't leave a step: the
    // 2 ms the pick takes to stop the string are smooth.
    let choke = max_step(&signal[repick..repick + seconds(0.002)]);
    let after_release = max_step(&signal[release..release + seconds(0.05)]);
    let during_slide = max_step(&signal[slide..slide + seconds(0.1)]);
    println!(
        "steepest steps: ringing {:.4}, re-pick {choke:.4}, release {after_release:.4} (ringing {:.4}), slide {during_slide:.4} (ringing {:.4})",
        ringing(repick),
        ringing(release),
        ringing(slide)
    );
    assert!(choke < 4.0 * ringing(repick));
    assert!(after_release < 1.2 * ringing(release));
    assert!(during_slide < 1.5 * ringing(slide));
}

// ---------------------------------------------------------------- variation

#[test]
fn repeated_notes_vary_but_renders_repeat_exactly() {
    let events: Vec<_> = (0..4)
        .flat_map(|k| [(seconds(0.4 * k as f32), on(57, 0.8))])
        .collect();
    let first = play(&mut *fresh(), &events, seconds(1.6));
    let again = play(&mut *fresh(), &events, seconds(1.6));
    assert_eq!(first.left, again.left, "the same render, sample for sample");
    // Each pluck differs a little from the last.
    let pluck =
        |k: usize| &first.left[seconds(0.4 * k as f32)..seconds(0.4 * k as f32) + seconds(0.1)];
    for k in 1..4 {
        let (a, b) = (pluck(k - 1), pluck(k));
        let difference: Vec<f32> = a.iter().zip(b).map(|(x, y)| x - y).collect();
        let db = rms_db(&difference) - rms_db(a);
        println!("pluck {k} differs from the one before by {db:.1} dB");
        assert!(db > -40.0 && db < 0.0, "{db}");
    }
}

// ---------------------------------------------------------------- levels

#[test]
fn levels_leave_headroom() {
    for model in [STEEL, NYLON, ELECTRIC] {
        let single = ring(&[("model", model)], 52, 1.0, 0.5, 0.5);
        let single = peak(&single.left).max(peak(&single.right));
        let mut guitar = with(&[("model", model)]);
        let events: Vec<_> = [40, 47, 52, 55, 59, 64]
            .iter()
            .map(|&n| (0, on(n, 1.0)))
            .collect();
        let strum = play(&mut *guitar, &events, seconds(0.6));
        let chord = peak(&strum.left).max(peak(&strum.right));
        let soft = ring(&[("model", model)], 52, 0.3, 0.5, 0.5);
        let quiet = peak(&soft.left);
        println!(
            "model {model} peaks: note {single:.3}, full strum {chord:.3}, soft note {quiet:.3}"
        );
        assert!((0.1..0.6).contains(&single), "model {model}: {single}");
        assert!(chord < 1.0, "model {model}: {chord}");
        assert!(quiet < 0.6 * single);
    }
    let events: Vec<_> = [40, 47, 52, 55, 59, 64]
        .iter()
        .map(|&n| (0, on(n, 1.0)))
        .collect();
    for amp in 1..=3 {
        let mut guitar = with(&[("model", ELECTRIC), ("amp", amp as f32)]);
        let take = play(&mut *guitar, &events, seconds(0.6));
        let level = peak(&take.left).max(peak(&take.right));
        println!("amp {amp}: strum peaks at {level:.3}");
        assert!((0.1..1.5).contains(&level), "amp {amp}: {level}");
    }
}

// ---------------------------------------------------------------- robustness

#[test]
fn extreme_settings_and_busy_playing_stay_finite_and_bounded() {
    let mut events = Vec::new();
    for i in 0..400usize {
        let t = i * 241;
        let note = (i * 37 % 128) as u8;
        events.push((t, on(note, (i % 7) as f32 / 6.0)));
        if i >= 5 {
            events.push((t, note_off(((i - 5) * 37 % 128) as u8)));
        }
        if i % 13 == 0 {
            events.push((
                t,
                NoteEventKind::PitchBend(((i * 997) % 16384) as i16 - 8192),
            ));
        }
        if i % 17 == 0 {
            events.push((t, control(MOD_WHEEL, (i % 128) as u8)));
        }
        if i % 23 == 0 {
            events.push((t, control(SOFT_PEDAL, (i * 5 % 128) as u8)));
        }
        if i % 101 == 0 {
            events.push((t, NoteEventKind::AllNotesOff));
        }
    }
    let length = 400 * 241 + seconds(0.5);
    let ranges: Vec<[f32; 2]> = fresh()
        .params()
        .iter()
        .map(|p| match p.kind {
            ParameterKind::Float { min, max, .. } => [min, max],
            ParameterKind::Enum { labels, .. } => [0.0, (labels.len() - 1) as f32],
        })
        .collect();
    let mut loudest = 0.0f32;
    for extreme in 0..3 {
        let mut guitar = fresh();
        for (index, range) in ranges.iter().enumerate() {
            // All low, all high, then alternating.
            let pick = match extreme {
                0 => 0,
                1 => 1,
                _ => index % 2,
            };
            guitar.set_param(index, range[pick]);
        }
        let take = play_at(&mut *guitar, &events, length, 256);
        loudest = loudest.max(peak(&take.left)).max(peak(&take.right));
    }
    println!("loudest at extreme settings: {loudest:.3}");
    assert!(loudest < 8.0, "{loudest}");
}

#[test]
fn switching_models_while_playing_does_not_click() {
    let mut guitar = fresh();
    let mut events = vec![(0, on(45, 0.8)), (0, on(52, 0.8))];
    let model = guitar
        .params()
        .iter()
        .position(|p| p.id == "model")
        .unwrap();
    // Play, then switch the model mid-note through the parameter.
    let mut take = play(&mut *guitar, &events, seconds(0.5));
    guitar.set_param(model, ELECTRIC);
    events.clear();
    let more = play(&mut *guitar, &events, seconds(0.5));
    take.left.extend_from_slice(&more.left);
    let before = max_step(&take.left[seconds(0.4)..seconds(0.5)]);
    let around = max_step(&take.left[seconds(0.5)..seconds(0.55)]);
    assert!(around < 1.5 * before, "{around} vs {before}");
}

#[test]
fn nan_and_unknown_parameters_are_ignored() {
    let mut guitar = fresh();
    for index in 0..guitar.params().len() + 2 {
        guitar.set_param(index, f32::NAN);
    }
    guitar.set_param(usize::MAX, 1.0);
    let take = play(&mut *guitar, &[(0, on(60, 0.8))], seconds(0.2));
    assert!(peak(&take.left) > 0.05);
}

#[test]
fn oversized_blocks_and_odd_sample_rates_work() {
    let mut guitar = guitar_at(22_050.0);
    guitar.prepare(22_050.0, 64);
    // Twice the prepared block size.
    let events = [NoteEvent {
        frame: 100,
        kind: on(100, 1.0),
    }];
    let (mut left, mut right) = (vec![0.0; 128], vec![0.0; 128]);
    guitar.process(&events, &mut left, &mut right);
    assert_eq!(peak(&left[..100]), 0.0);
    for _ in 0..100 {
        guitar.process(&[], &mut left, &mut right);
        assert!(left.iter().all(|s| s.is_finite()));
    }
    assert!(peak(&left) > 0.0);
}
