//! What the instruments' integration tests share: note events, levels,
//! pitch and spectra, parameter ranges, CPU timings, and the wavetable
//! synth set up plainly.

#![allow(dead_code)]

pub mod wavetable;

use std::f64::consts::PI;
use std::sync::{Mutex, MutexGuard};

use instruments::{Instrument, ParamDescriptor, ParameterKind, find_builtin};
use midi::{NoteEvent, NoteEventKind, Pitch, Velocity};

pub const RATE: f32 = 48_000.0;

pub fn event(frame: usize, kind: NoteEventKind) -> NoteEvent {
    NoteEvent {
        frame: frame as u32,
        kind,
    }
}

/// `note` struck at MIDI velocity `velocity` (127 is the hardest).
pub fn note_on(note: u8, velocity: u8) -> NoteEventKind {
    NoteEventKind::On {
        pitch: Pitch::new(note).unwrap(),
        velocity: Velocity::from_midi1(velocity),
    }
}

pub fn note_off(note: u8) -> NoteEventKind {
    NoteEventKind::Off {
        pitch: Pitch::new(note).unwrap(),
        velocity: Velocity::CENTER,
    }
}

/// `note` struck as hard as it goes, at the start of the block.
pub fn on(note: u8) -> NoteEvent {
    event(0, note_on(note, 127))
}

/// `note` let go at the start of the block.
pub fn off(note: u8) -> NoteEvent {
    event(0, note_off(note))
}

/// A note every 8 frames of a `block`-frame block, each let go three notes
/// later, so voices start, retrigger, release and get stolen: the `i`th is
/// `note(i)` at MIDI velocity `velocity(i)`. Unsorted, so callers can add
/// more.
pub fn cascade(
    block: usize,
    note: impl Fn(usize) -> u8,
    velocity: impl Fn(usize) -> u8,
) -> Vec<NoteEvent> {
    let mut events = Vec::new();
    for i in 0..block / 8 {
        if i >= 3 {
            events.push(event(i * 8, note_off(note(i - 3))));
        }
        events.push(event(i * 8, note_on(note(i), velocity(i))));
    }
    events
}

/// The lowest and highest value `param` takes.
pub fn range(param: &ParamDescriptor) -> (f32, f32) {
    match param.kind {
        ParameterKind::Float { min, max, .. } => (min, max),
        ParameterKind::Enum { labels, .. } => (0.0, (labels.len() - 1) as f32),
    }
}

pub fn peak(signal: &[f32]) -> f32 {
    signal.iter().fold(0.0, |m, s| m.max(s.abs()))
}

pub fn rms(signal: &[f32]) -> f32 {
    (signal.iter().map(|s| s * s).sum::<f32>() / signal.len().max(1) as f32).sqrt()
}

/// The largest jump between neighbouring samples: what a click is made of.
pub fn max_step(signal: &[f32]) -> f32 {
    signal
        .windows(2)
        .fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
}

/// Frames in `s` seconds.
pub fn seconds(s: f32) -> usize {
    (s * RATE) as usize
}

/// Frequency from rising zero crossings, interpolated between samples, so
/// it resolves far finer than one sample's period.
pub fn pitch(signal: &[f32]) -> f32 {
    let mut crossings = Vec::new();
    for (i, w) in signal.windows(2).enumerate() {
        if w[0] < 0.0 && w[1] >= 0.0 {
            crossings.push(i as f32 + w[0] / (w[0] - w[1]));
        }
    }
    let (first, last) = (crossings[0], crossings[crossings.len() - 1]);
    (crossings.len() - 1) as f32 * RATE / (last - first)
}

/// Magnitude at `hz` over `signal`, Hann-windowed (Goertzel).
pub fn magnitude(signal: &[f32], hz: f64) -> f64 {
    let n = signal.len();
    let w = 2.0 * PI * hz / RATE as f64;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for (i, x) in signal.iter().enumerate() {
        let window = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
        let s = *x as f64 * window + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / n as f64 * 4.0
}

/// Power spectrum by FFT of the first power-of-two samples, Blackman-Harris windowed.
pub fn power_spectrum(signal: &[f32]) -> Vec<f64> {
    let n = signal.len().next_power_of_two() / 2;
    let mut re: Vec<f64> = (0..n)
        .map(|i| {
            let t = 2.0 * PI * i as f64 / n as f64;
            let window =
                0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos();
            signal[i] as f64 * window
        })
        .collect();
    let mut im = vec![0.0; n];
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (w_im, w_re) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let t_re = re[b] * w_re - im[b] * w_im;
                let t_im = re[b] * w_im + im[b] * w_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
            }
        }
        len <<= 1;
    }
    (0..n / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect()
}

/// Energy of harmonics of `f0` against everything else above 30 Hz, in dB.
pub fn alias_db(signal: &[f32], f0: f64) -> f64 {
    let spectrum = power_spectrum(signal);
    let bin_hz = RATE as f64 / (2 * spectrum.len()) as f64;
    let (mut harmonic, mut other) = (0.0, 0.0);
    for (k, power) in spectrum.iter().enumerate() {
        let hz = k as f64 * bin_hz;
        if hz < 30.0 {
            continue;
        }
        let nearest = (hz / f0).round() * f0;
        if nearest > 0.0 && (hz - nearest).abs() <= 6.0 * bin_hz {
            harmonic += power;
        } else {
            other += power;
        }
    }
    10.0 * (other / harmonic).log10()
}

/// Holds timings in one test binary apart: two at once would slow each
/// other down.
/// The built-in `id`, prepared at [`RATE`] for blocks of `block`, set as
/// `settings` say.
pub fn instrument(id: &str, block: usize, settings: &[(&str, f32)]) -> Box<dyn Instrument> {
    let mut instrument = (find_builtin(id).unwrap_or_else(|| panic!("no {id}")).create)();
    instrument.prepare(RATE, block);
    for &(param, value) in settings {
        set(instrument.as_mut(), param, value);
    }
    instrument
}

/// Sets the parameter called `id`.
pub fn set(instrument: &mut dyn Instrument, id: &str, value: f32) {
    let index = instrument
        .params()
        .iter()
        .position(|p| p.id == id)
        .unwrap_or_else(|| panic!("no parameter {id}"));
    instrument.set_param(index, value);
}

pub fn timing() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
