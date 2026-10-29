//! The Slicer: which key plays which slice, at what pitch, cut or ringing,
//! forwards or backwards, and silence without a loop.

mod common;

use std::sync::Arc;

use common::{RATE, event, instrument, magnitude, note_on, off, on, peak, pitch, set};
use instruments::builtin::slicer::{METER_PLAYING, METER_SLICES, METER_VOICES, SliceData};
use instruments::{Instrument, MAX_METERS};
use midi::{NoteEvent, NoteEventKind, Pitch};

const BLOCK: usize = 64;
/// Each test slice is a tenth of a second.
const SLICE: usize = 4800;

/// Slice `k`'s tone.
fn hz(k: usize) -> f32 {
    300.0 + 150.0 * k as f32
}

/// Four slices, each a sine of its own pitch, played at `speed`.
fn tones(speed: f64) -> SliceData {
    let audio: Vec<f32> = (0..4 * SLICE)
        .map(|i| (std::f32::consts::TAU * hz(i / SLICE) * i as f32 / RATE).sin() * 0.5)
        .collect();
    SliceData {
        left: audio.clone().into(),
        right: audio.into(),
        sample_rate: RATE,
        starts: (0..4).map(|k| k * SLICE).collect(),
        speed,
    }
}

fn slicer(data: SliceData, settings: &[(&str, f32)]) -> Box<dyn Instrument> {
    let mut slicer = instrument("canopy.slicer", BLOCK, settings);
    slicer.set_data(Arc::new(data));
    slicer
}

/// Renders `frames`, with `events` at the frames they name.
fn render(slicer: &mut dyn Instrument, events: &[NoteEvent], frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    for at in (0..frames).step_by(BLOCK) {
        let block: Vec<NoteEvent> = events
            .iter()
            .filter(|e| (at..at + BLOCK).contains(&(e.frame as usize)))
            .map(|e| NoteEvent {
                frame: e.frame - at as u32,
                kind: e.kind,
            })
            .collect();
        slicer.process(&block, &mut left, &mut right);
        out.extend_from_slice(&left);
    }
    out.truncate(frames);
    out
}

#[test]
fn slice_k_plays_on_key_root_plus_k() {
    for k in 0..4 {
        let mut slicer = slicer(tones(1.0), &[]);
        let out = render(slicer.as_mut(), &[on(60 + k as u8)], SLICE);
        let heard = pitch(&out[480..4000]);
        assert!((heard - hz(k)).abs() < 1.0, "slice {k}: {heard} Hz");
    }
    // Keys below the root and past the last slice play nothing.
    let mut slicer = slicer(tones(1.0), &[("root", 48.0)]);
    let out = render(slicer.as_mut(), &[on(47), on(52)], SLICE);
    assert_eq!(peak(&out), 0.0);
    let out = render(slicer.as_mut(), &[on(49)], SLICE);
    assert!(
        (pitch(&out[480..4000]) - hz(1)).abs() < 1.0,
        "the root moves"
    );
}

#[test]
fn a_slice_stops_at_its_end_without_a_click() {
    let mut slicer = slicer(tones(1.0), &[("release", 2000.0)]);
    let out = render(slicer.as_mut(), &[on(60)], 2 * SLICE);
    assert!(peak(&out[..SLICE - 200]) > 0.4);
    assert_eq!(peak(&out[SLICE + 64..]), 0.0, "the next slice isn't played");
    // The last frames fade instead of dropping from full level.
    let tail = &out[SLICE - 40..SLICE];
    assert!(tail.last().unwrap().abs() < 0.05, "{tail:?}");
    let mut meters = [0.0; MAX_METERS];
    slicer.meters(&mut meters);
    assert_eq!(meters[METER_VOICES], 0.0);
    assert_eq!(meters[METER_SLICES], 4.0);
}

#[test]
fn resample_at_double_speed_is_an_octave_up() {
    let mut slicer = slicer(tones(2.0), &[]);
    let out = render(slicer.as_mut(), &[on(61)], SLICE);
    let heard = pitch(&out[240..2200]);
    assert!((heard - 2.0 * hz(1)).abs() < 2.0, "{heard} Hz");
    // And it's over in half the time.
    assert_eq!(peak(&out[SLICE / 2 + 64..]), 0.0);
}

#[test]
fn pitch_knobs_add_up() {
    let mut slicer = slicer(tones(1.0), &[("pitch", 12.0), ("slice3_pitch", -5.0)]);
    let out = render(slicer.as_mut(), &[on(62)], SLICE);
    let heard = pitch(&out[240..2000]);
    let expected = hz(2) * (7.0f32 / 12.0).exp2();
    assert!(
        (heard - expected).abs() < 2.0,
        "{heard} Hz, wanted {expected}"
    );
}

#[test]
fn cut_stops_the_previous_slice() {
    let events = [on(60), event(1200, note_on(62, 127))];
    let mut cut = slicer(tones(1.0), &[]);
    let out = render(cut.as_mut(), &events, 4000);
    let after = &out[1200 + 480..4000];
    assert!(
        magnitude(after, f64::from(hz(0))) < 0.002,
        "slice 1 is gone"
    );
    assert!(magnitude(after, f64::from(hz(2))) > 0.1, "slice 3 plays");
    // The cut fades instead of dropping the first slice at once.
    let step = out[1199..1210]
        .windows(2)
        .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(step < 0.2, "{step}");

    let mut ringing = slicer(tones(1.0), &[("cut", 0.0)]);
    let out = render(ringing.as_mut(), &events, 4000);
    let after = &out[1200 + 480..4000];
    assert!(magnitude(after, f64::from(hz(0))) > 0.1, "slice 1 rings on");
    assert!(magnitude(after, f64::from(hz(2))) > 0.1);
    let mut meters = [0.0; MAX_METERS];
    ringing.meters(&mut meters);
    assert_eq!(meters[METER_PLAYING] as u32, 0b101, "slices 1 and 3 lit");
}

#[test]
fn reverse_plays_a_slice_backwards() {
    // One slice rising from silence to full.
    let ramp: Vec<f32> = (0..SLICE).map(|i| i as f32 / SLICE as f32).collect();
    let data = SliceData {
        left: ramp.clone().into(),
        right: ramp.into(),
        sample_rate: RATE,
        starts: Box::new([0]),
        speed: 1.0,
    };
    let mut forwards = slicer(data.clone(), &[]);
    let out = render(forwards.as_mut(), &[on(60)], SLICE);
    assert!(out[SLICE / 4] < out[3 * SLICE / 4]);
    let mut backwards = slicer(data, &[("slice1_reverse", 1.0)]);
    let out = render(backwards.as_mut(), &[on(60)], SLICE);
    assert!(out[SLICE / 4] > out[3 * SLICE / 4]);
    assert!((out[SLICE / 4] - 0.75).abs() < 0.01, "{}", out[SLICE / 4]);
}

#[test]
fn a_bend_moves_a_slices_pitch() {
    let mut slicer = slicer(tones(1.0), &[]);
    let bend = NoteEventKind::NoteBend {
        pitch: Pitch::new(61).unwrap(),
        cents: 1200,
    };
    let other = NoteEventKind::NoteBend {
        pitch: Pitch::new(70).unwrap(),
        cents: -1200,
    };
    let out = render(
        slicer.as_mut(),
        &[on(61), event(640, bend), event(640, other)],
        SLICE / 2,
    );
    let before = pitch(&out[100..600]);
    let after = pitch(&out[800..2200]);
    assert!((before - hz(1)).abs() < 2.0, "{before} Hz");
    assert!((after - 2.0 * hz(1)).abs() < 3.0, "{after} Hz");
}

#[test]
fn level_and_pan_shape_a_slice() {
    let mut slicer = slicer(tones(1.0), &[("slice1_level", -6.0), ("slice1_pan", 100.0)]);
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    let (mut l, mut r) = (0.0f32, 0.0f32);
    slicer.process(&[on(60)], &mut left, &mut right);
    for _ in 0..20 {
        slicer.process(&[], &mut left, &mut right);
        l = l.max(peak(&left));
        r = r.max(peak(&right));
    }
    assert!(l < 1e-3, "panned right: {l}");
    // Centred is unity on both sides, as is either side at its end: the
    // tone's 0.5, less 6 dB.
    assert!((r - 0.5 * 0.501).abs() < 0.01, "{r}");
}

#[test]
fn no_data_is_silent() {
    let mut slicer = instrument("canopy.slicer", BLOCK, &[]);
    let out = render(
        slicer.as_mut(),
        &[on(60), on(61), event(100, off(60).kind)],
        SLICE,
    );
    assert_eq!(peak(&out), 0.0);
    let mut meters = [0.0; MAX_METERS];
    slicer.meters(&mut meters);
    assert_eq!(meters[METER_SLICES], 0.0);
    assert_eq!(meters[METER_VOICES], 0.0);
    // Empty audio is no different.
    let mut empty = instrument("canopy.slicer", BLOCK, &[]);
    empty.set_data(Arc::new(SliceData {
        left: Vec::new().into(),
        right: Vec::new().into(),
        sample_rate: RATE,
        starts: Box::new([0]),
        speed: 1.0,
    }));
    let out = render(empty.as_mut(), &[on(60)], 1000);
    assert_eq!(peak(&out), 0.0);
    set(empty.as_mut(), "volume", 6.0);
}
