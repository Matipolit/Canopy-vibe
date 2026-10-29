//! Every built-in instrument follows a note's own bend
//! (`NoteEventKind::NoteBend`): the voice playing that note moves, and no
//! other.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::wavetable::plain_sine;
use common::{RATE, event, max_step, note_off, note_on, seconds, set};
use instruments::builtin::drums::{self, LAYERS, Layer, Voice, voices};
use instruments::builtin::sampler::{self, Decoded};
use instruments::builtin::wavetable::{OSC_ON, SUB_ON, osc_param};
use instruments::{Instrument, find_builtin};
use midi::{NoteEvent, NoteEventKind, Pitch};

/// The engine's block, and how often it sends a moving bend.
const BLOCK: usize = 64;

/// An instrument set up to sound two notes as near-plain tones.
struct Case {
    name: &'static str,
    make: fn() -> Box<dyn Instrument>,
    /// The keys played, and roughly the pitch each sounds at.
    notes: [u8; 2],
    hz: [f32; 2],
    /// Plays both at once (the Simple Oscillator plays one note at a time).
    chords: bool,
}

fn note_hz(note: u8) -> f32 {
    440.0 * ((f32::from(note) - 69.0) / 12.0).exp2()
}

fn builtin(id: &str, settings: &[(&str, f32)]) -> Box<dyn Instrument> {
    let mut instrument = (find_builtin(id).unwrap().create)();
    instrument.prepare(RATE, BLOCK);
    for &(id, value) in settings {
        set(&mut *instrument, id, value);
    }
    instrument.reset();
    instrument
}

fn poly_synth() -> Box<dyn Instrument> {
    builtin(
        "canopy.poly_synth",
        &[("waveform", 0.0), ("detune", 0.0), ("cutoff", 20_000.0)],
    )
}

fn simple_osc() -> Box<dyn Instrument> {
    builtin("canopy.simple_osc", &[("waveform", 0.0)])
}

fn wavetable() -> Box<dyn Instrument> {
    Box::new(plain_sine())
}

/// The wavetable synth with only its sub (a sine an octave down) on.
fn wavetable_sub() -> Box<dyn Instrument> {
    let mut synth = plain_sine();
    synth.set_param(osc_param(0, OSC_ON), 0.0);
    synth.set_param(SUB_ON, 1.0);
    synth.reset();
    Box::new(synth)
}

/// A second of a 440 Hz sine, whatever the file is called.
fn decode(_: &Path) -> Result<Decoded, String> {
    let rate = 44_100u32;
    let samples = (0..rate as usize)
        .map(|i| (std::f64::consts::TAU * 440.0 * i as f64 / f64::from(rate)).sin() as f32)
        .collect();
    Ok(Decoded::mono(samples, rate))
}

fn sfz_sampler() -> Box<dyn Instrument> {
    let text = "<region> sample=sine440.wav pitch_keycenter=a4";
    let bank = sampler::load_str(text, Path::new("/test/test.sfz"), &decode).unwrap();
    let mut sampler = (sampler::DESCRIPTOR.create)();
    sampler.set_data(Arc::new(bank));
    sampler.prepare(RATE, BLOCK);
    sampler
}

/// The pads the drum case plays: their first layer alone, a steady sine.
const DRUM_PADS: [&str; 2] = ["Kick", "Low Tom"];

fn drum_kit() -> Box<dyn Instrument> {
    let mut kit = (drums::DESCRIPTOR.create)();
    kit.prepare(RATE, BLOCK);
    let kick = pad("Kick");
    let sine = drums::DESCRIPTOR.params[kick.layer_param(0, Layer::SOURCE)].default_value();
    for (name, hz) in DRUM_PADS.into_iter().zip([220.0, 330.0]) {
        let voice = pad(name);
        for layer in 1..LAYERS {
            kit.set_param(voice.layer_param(layer, Layer::MODE), 0.0);
        }
        for (offset, value) in [
            (Layer::SOURCE, sine),
            (Layer::PITCH, hz),
            (Layer::PITCH_ENV, 0.0),
            (Layer::DECAY, 3000.0),
        ] {
            kit.set_param(voice.layer_param(0, offset), value);
        }
        kit.set_param(voice.param(Voice::FILTER), 0.0);
        kit.set_param(voice.param(Voice::DRIVE), 0.0);
    }
    kit.reset();
    kit
}

fn pad(name: &str) -> &'static Voice {
    voices().iter().find(|v| v.name == name).unwrap()
}

fn guitar() -> Box<dyn Instrument> {
    builtin(
        "canopy.guitar",
        &[("humanize", 0.0), ("resonance", 0.0), ("strum", 0.0)],
    )
}

/// Keys away from the open strings' harmonics, so nothing else on the
/// guitar rings at the frequencies measured.
fn cases() -> Vec<Case> {
    let plain = |name, make| Case {
        name,
        make,
        notes: [57, 64],
        hz: [note_hz(57), note_hz(64)],
        chords: true,
    };
    vec![
        plain("Poly Synth", poly_synth),
        Case {
            chords: false,
            ..plain("Simple Oscillator", simple_osc)
        },
        plain("Wavetable", wavetable),
        Case {
            name: "Wavetable sub",
            make: wavetable_sub,
            notes: [69, 76],
            hz: [note_hz(57), note_hz(64)],
            chords: true,
        },
        plain("SFZ Sampler", sfz_sampler),
        Case {
            name: "Drum Synth",
            make: drum_kit,
            notes: [pad(DRUM_PADS[0]).notes[0], pad(DRUM_PADS[1]).notes[0]],
            hz: [220.0, 330.0],
            chords: true,
        },
        Case {
            name: "Guitar",
            make: guitar,
            notes: [58, 65],
            hz: [note_hz(58), note_hz(65)],
            chords: true,
        },
    ]
}

fn bend(note: u8, cents: i16) -> NoteEventKind {
    NoteEventKind::NoteBend {
        pitch: Pitch::new(note).unwrap(),
        cents,
    }
}

/// Plays `len` frames a block at a time, with `events` (frame, event)
/// counted from the first; returns the left channel.
fn play(
    instrument: &mut dyn Instrument,
    events: &[(usize, NoteEventKind)],
    len: usize,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(len);
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    while out.len() < len {
        let start = out.len();
        let block: Vec<NoteEvent> = events
            .iter()
            .filter(|(frame, _)| (start..start + BLOCK).contains(frame))
            .map(|&(frame, kind)| event(frame - start, kind))
            .collect();
        instrument.process(&block, &mut left, &mut right);
        assert!(left.iter().chain(&right).all(|s| s.is_finite()));
        out.extend_from_slice(&left);
    }
    out
}

/// Hann-windowed DFT amplitude at `hz` over `signal`.
fn level(signal: &[f32], hz: f32) -> f64 {
    let n = signal.len();
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &x) in signal.iter().enumerate() {
        let hann = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
        let arg = std::f64::consts::TAU * f64::from(hz) * i as f64 / f64::from(RATE);
        re += f64::from(x) * hann * arg.cos();
        im -= f64::from(x) * hann * arg.sin();
    }
    4.0 * (re * re + im * im).sqrt() / n as f64
}

/// Where `signal` is strongest between `low` and `high`, to a fraction of
/// a hertz.
fn strongest_between(signal: &[f32], low: f32, high: f32) -> f32 {
    let mut best = (0.0f64, low);
    let mut hz = low;
    while hz <= high {
        let level = level(signal, hz);
        if level > best.0 {
            best = (level, hz);
        }
        hz += 0.25;
    }
    let (a, b, c) = (
        level(signal, best.1 - 0.25),
        best.0,
        level(signal, best.1 + 0.25),
    );
    let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
    best.1 + 0.25 * shift as f32
}

/// The pitch near `hz` (within a semitone and a half).
fn pitch_near(signal: &[f32], hz: f32) -> f32 {
    strongest_between(signal, hz * 0.92, hz * 1.09)
}

fn cents(measured: f32, expected: f32) -> f32 {
    1200.0 * (measured / expected).log2()
}

#[test]
fn a_held_note_bent_an_octave_up_sounds_an_octave_up() {
    for case in cases() {
        let mut instrument = (case.make)();
        let [note, _] = case.notes;
        let [hz, _] = case.hz;
        let out = play(
            &mut *instrument,
            &[(0, note_on(note, 100)), (seconds(0.4), bend(note, 1200))],
            seconds(0.8),
        );
        let before = &out[seconds(0.15)..seconds(0.4)];
        let after = &out[seconds(0.5)..seconds(0.8)];
        let plain = pitch_near(before, hz);
        assert!(
            cents(plain, hz).abs() < 15.0,
            "{}: plays {plain} Hz, not {hz}",
            case.name
        );
        let bent = pitch_near(after, 2.0 * plain);
        let off = cents(bent, 2.0 * plain);
        assert!(
            off.abs() < 5.0,
            "{}: bent to {bent} Hz ({off:+.1} cents)",
            case.name
        );
        // Nothing is left at the old pitch.
        let (gone, now) = (level(after, plain), level(after, bent));
        assert!(gone < 0.05 * now, "{}: {gone} left of {now}", case.name);
    }
}

#[test]
fn bending_one_note_of_a_chord_leaves_the_other_where_it_is() {
    for case in cases().into_iter().filter(|c| c.chords) {
        let [low, high] = case.notes;
        let chord = |bent: bool| {
            let mut events = vec![(0, note_on(low, 100)), (0, note_on(high, 100))];
            if bent {
                events.push((seconds(0.4), bend(low, 1200)));
            }
            play(&mut *(case.make)(), &events, seconds(0.8))
        };
        let (plain, bent) = (chord(false), chord(true));
        let [low_hz, high_hz] = case.hz;
        let window = seconds(0.45)..seconds(0.75);
        // The other note sounds as it does in the chord left alone.
        let other = pitch_near(&plain[window.clone()], high_hz);
        let other_bent = pitch_near(&bent[window.clone()], high_hz);
        let moved = cents(other_bent, other);
        assert!(
            moved.abs() < 0.5,
            "{}: the other note moved {moved:+.2} cents",
            case.name
        );
        let level_plain = level(&plain[window.clone()], other);
        let level_bent = level(&bent[window.clone()], other);
        assert!(
            (level_bent / level_plain - 1.0).abs() < 0.05,
            "{}: the other note went from {level_plain} to {level_bent}",
            case.name
        );
        // And the bent one is an octave up.
        let own = pitch_near(&plain[window.clone()], low_hz);
        let up = pitch_near(&bent[window.clone()], 2.0 * own);
        let off = cents(up, 2.0 * own);
        assert!(
            off.abs() < 5.0,
            "{}: bent to {up} Hz ({off:+.1} cents)",
            case.name
        );
        let (gone, was) = (
            level(&bent[window.clone()], own),
            level(&plain[window], own),
        );
        assert!(gone < 0.05 * was, "{}: {gone} left of {was}", case.name);
    }
}

#[test]
fn a_bend_follows_its_pitch_when_two_notes_overlap() {
    for case in cases() {
        let mut instrument = (case.make)();
        let [note, _] = case.notes;
        let [hz, _] = case.hz;
        // The same key again before the first is let go, then one let go
        // and the other bent, as overlapping clips can play them.
        let out = play(
            &mut *instrument,
            &[
                (0, note_on(note, 100)),
                (seconds(0.1), note_on(note, 100)),
                (seconds(0.2), bend(note, 1200)),
                (seconds(0.3), note_off(note)),
                (seconds(0.3), bend(note, 700)),
                (seconds(0.5), note_off(note)),
            ],
            seconds(0.6),
        );
        let bent = &out[seconds(0.22)..seconds(0.3)];
        let up = pitch_near(bent, 2.0 * hz);
        assert!(
            cents(up, 2.0 * hz).abs() < 20.0,
            "{}: bent to {up} Hz, not {}",
            case.name,
            2.0 * hz
        );
    }
}

#[test]
fn a_note_starting_bent_sounds_bent_from_its_start() {
    for case in cases() {
        let mut instrument = (case.make)();
        let [note, _] = case.notes;
        let [hz, _] = case.hz;
        // The engine sends a bent note's first value right after its note-on.
        let out = play(
            &mut *instrument,
            &[(0, note_on(note, 100)), (0, bend(note, 1200))],
            seconds(0.3),
        );
        let start = &out[..seconds(0.1)];
        let up = pitch_near(start, 2.0 * hz);
        assert!(
            cents(up, 2.0 * hz).abs() < 15.0,
            "{}: starts at {up} Hz, not {}",
            case.name,
            2.0 * hz
        );
        assert!(
            level(start, hz) < 0.05 * level(start, up),
            "{}: starts at its own pitch",
            case.name
        );
    }
}

#[test]
fn a_bend_sent_a_block_at_a_time_glides_without_clicks() {
    // The engine sends a moving bend once a block; each voice glides
    // between the values, so a sine stays a sine all the way up.
    for case in cases().iter().filter(|c| c.name != "Guitar") {
        let mut instrument = (case.make)();
        let [note, _] = case.notes;
        let [hz, _] = case.hz;
        let start = seconds(0.1);
        let steps = seconds(0.4) / BLOCK;
        let mut events = vec![(0, note_on(note, 100))];
        for step in 1..=steps {
            let cents = (1200 * step / steps) as i16;
            events.push((start + step * BLOCK, bend(note, cents)));
        }
        let out = play(&mut *instrument, &events, seconds(0.6));
        let glide = &out[start..start + steps * BLOCK];
        let peak = common::peak(glide);
        // The steepest a sine as loud at the top pitch gets, with a little
        // room.
        let steepest = 1.1 * peak * std::f32::consts::TAU * 2.0 * hz / RATE;
        let step = max_step(glide);
        assert!(
            step < steepest,
            "{}: steps of {step} while gliding (a sine at the top takes {steepest})",
            case.name
        );
    }
}
