//! Measures the third oscillator and user wavetables: osc C plays, routes,
//! modulates and cross-modulates like A and B, and starts off; a table
//! read from sampled frames plays in tune, matches the built-in shapes,
//! stays band-limited and morphs across any number of frames; "User" with
//! nothing loaded falls back to the first built-in table.

mod common;

use std::f64::consts::PI;
use std::sync::Arc;

use common::wavetable::{plain_sine, render};
use common::{alias_db, magnitude, on, peak, pitch, rms};
use instruments::builtin::wavetable::*;
use instruments::{Instrument, InstrumentData, MAX_METERS};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 256;

/// [`plain_sine`] played by oscillator `osc` alone, with its settings as A's.
fn plain_on(osc: usize) -> Wavetable {
    let mut synth = plain_sine();
    let p = |offset| osc_param(osc, offset);
    synth.set_param(osc_param(0, OSC_ON), 0.0);
    synth.set_param(p(OSC_ON), 1.0);
    synth.set_param(p(OSC_TABLE), 0.0);
    synth.set_param(p(OSC_POSITION), 0.0);
    synth.set_param(p(OSC_UNISON), 0.0);
    synth.set_param(p(OSC_RANDOM), 0.0);
    synth.set_param(p(OSC_PHASE), 0.0);
    synth.set_param(p(OSC_OCTAVE), 4.0);
    synth.set_param(p(OSC_SEMI), 12.0);
    synth.set_param(p(OSC_FINE), 0.0);
    synth.set_param(p(OSC_LEVEL), 0.7);
    synth.set_param(p(OSC_PAN), 0.0);
    synth.reset();
    synth
}

fn hz(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

// ---------------------------------------------------------------------------
// Osc C

#[test]
fn osc_c_starts_off_so_the_default_patch_is_unchanged() {
    let synth = Wavetable::new();
    let c = &PARAMS[osc_param(2, OSC_ON)];
    assert_eq!(c.id, "osc_c_on");
    assert_eq!(c.default_value(), 0.0);
    let ids: Vec<&str> = PARAMS[OSC_C].iter().map(|p| p.id).collect();
    for (offset, a) in PARAMS[OSC_A].iter().enumerate() {
        assert_eq!(ids[offset], a.id.replacen("osc_a_", "osc_c_", 1));
    }
    assert_eq!(PARAMS[FILTER_OSC_C].id, "filter_osc_c");
    assert_eq!(PARAMS[FILTER_TARGET_OSC_C].id, "filter_osc_c_target");
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    let position = PARAMS[osc_param(2, OSC_POSITION)].default_value();
    assert_eq!(meters[METER_POSITION + 2], position);

    // With C on but silent, A and B play exactly as with it off.
    let mut off = Wavetable::new();
    off.prepare(RATE, BLOCK);
    let mut silent = Wavetable::new();
    silent.prepare(RATE, BLOCK);
    silent.set_param(osc_param(2, OSC_ON), 1.0);
    silent.set_param(osc_param(2, OSC_LEVEL), 0.0);
    silent.reset();
    let chord = [on(48), on(55), on(60)];
    let (a, _) = render(&mut off, &chord, 0.5);
    let (b, _) = render(&mut silent, &chord, 0.5);
    let worst = a
        .iter()
        .zip(&b)
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
    assert!(worst < 1e-6, "{worst}");
}

#[test]
fn osc_c_plays_in_tune_and_transposes_like_a() {
    for osc in 0..OSC_COUNT {
        let mut synth = plain_on(osc);
        let (l, _) = render(&mut synth, &[on(69)], 1.0);
        let measured = pitch(&l[4800..]);
        assert!((measured - 440.0).abs() < 0.05, "osc {osc}: {measured}");
        let p = |offset| osc_param(osc, offset);
        synth.set_param(p(OSC_OCTAVE), 5.0);
        synth.set_param(p(OSC_SEMI), 14.0);
        synth.set_param(p(OSC_FINE), 50.0);
        synth.reset();
        let (l, _) = render(&mut synth, &[on(57)], 1.0);
        let expected = 220.0 * 2f32.powf((12.0 + 2.0 + 0.5) / 12.0);
        let measured = pitch(&l[4800..]);
        assert!((measured - expected).abs() < 0.05, "osc {osc}: {measured}");
    }
    // C plays at the same level as A with the same settings.
    let (a, _) = render(&mut plain_on(0), &[on(60)], 0.5);
    let (c, _) = render(&mut plain_on(2), &[on(60)], 0.5);
    assert!((rms(&a[4800..]) / rms(&c[4800..]) - 1.0).abs() < 0.01);
}

#[test]
fn osc_c_goes_where_it_is_routed() {
    // A 110 Hz saw from C through a low-pass at 400 Hz: its 3960 Hz
    // harmonic is gone if it goes into the filter, there if it doesn't.
    let level = |into_filter: bool, target: usize| {
        let mut synth = plain_on(2);
        synth.set_param(osc_param(2, OSC_POSITION), 2.0 / 3.0);
        synth.set_param(FILTER_ON, 1.0);
        synth.set_param(FILTER_TYPE, LP24 as f32);
        synth.set_param(FILTER_CUTOFF, 400.0);
        synth.set_param(FILTER_RESONANCE, 0.0);
        synth.set_param(FILTER_KEYTRACK, 0.0);
        synth.set_param(FILTER_DRIVE, 0.0);
        synth.set_param(FILTER2_ON, 1.0);
        synth.set_param(FILTER2_TYPE, LP24 as f32);
        synth.set_param(FILTER2_CUTOFF, 400.0);
        synth.set_param(FILTER_ROUTING, 1.0);
        let (switch, which) = SOURCE_ROUTES[OSC_ROUTES[2]];
        synth.set_param(switch, if into_filter { 1.0 } else { 0.0 });
        synth.set_param(which, target as f32);
        synth.reset();
        let (l, _) = render(&mut synth, &[on(45)], 1.0);
        magnitude(&l[9600..], 3960.0)
    };
    let direct = level(false, 0);
    for target in [0, 1] {
        let filtered = level(true, target);
        println!(
            "osc C into filter {}: {filtered:.6} vs {direct:.6}",
            target + 1
        );
        assert!(filtered < direct * 0.01, "filter {}", target + 1);
    }
}

#[test]
fn the_matrix_drives_osc_c() {
    // Macro 1 at full: C's pitch up 12 semitones, then its level down to
    // nothing, then its fine tune up 50 cents.
    let with_slot = |dest: usize, amount: f32| {
        let mut synth = plain_on(2);
        synth.set_param(MACROS.start, 1.0);
        synth.set_param(slot_param(0, SLOT_SOURCE), SRC_MACRO1 as f32);
        synth.set_param(slot_param(0, SLOT_DEST), dest as f32);
        synth.set_param(slot_param(0, SLOT_AMOUNT), amount);
        synth.reset();
        render(&mut synth, &[on(57)], 1.0).0
    };
    let up = with_slot(osc_dest(2) + DEST_PITCH, 0.5);
    let measured = pitch(&up[4800..]);
    assert!((measured - 440.0).abs() < 0.1, "{measured}");
    let quiet = with_slot(osc_dest(2) + DEST_LEVEL, -1.0);
    assert!(peak(&quiet[4800..]) < 1e-4, "{}", peak(&quiet[4800..]));
    let fine = with_slot(osc_dest(2) + DEST_FINE, 0.5);
    let measured = pitch(&fine[4800..]);
    let expected = 220.0 * 2f32.powf(0.5 / 12.0);
    assert!((measured - expected).abs() < 0.1, "{measured}");
    // A's destinations leave C alone.
    let other = with_slot(osc_dest(0) + DEST_PITCH, 0.5);
    let measured = pitch(&other[4800..]);
    assert!((measured - 220.0).abs() < 0.05, "{measured}");
    assert_eq!(DESTINATIONS[warp2_dest(2)], "C Warp 2");
}

#[test]
fn osc_c_cross_modulates_from_a() {
    assert_eq!(MODULATORS, [1, 0, 0]);
    assert_eq!(WARPS_C[WARP_FM], "FM from A");
    let fm = |a_on: bool| {
        let mut synth = plain_on(2);
        let a = |offset| osc_param(0, offset);
        synth.set_param(a(OSC_ON), if a_on { 1.0 } else { 0.0 });
        synth.set_param(a(OSC_LEVEL), 0.0);
        synth.set_param(a(OSC_OCTAVE), 5.0);
        synth.set_param(osc_param(2, OSC_WARP_MODE), WARP_FM as f32);
        synth.set_param(osc_param(2, OSC_WARP), 0.5);
        synth.reset();
        render(&mut synth, &[on(57)], 0.5).0
    };
    let (heard, alone) = (fm(true), fm(false));
    let (plain, _) = render(&mut plain_on(2), &[on(57)], 0.5);
    // A silent but on: C hears it, so its sound changes: sidebands at
    // 220 ± 440 Hz (660 Hz) appear.
    let sideband = |s: &[f32]| magnitude(&s[4800..], 660.0);
    assert!(sideband(&heard) > 0.01, "{}", sideband(&heard));
    // With A off there is nothing to hear: FM is off.
    let worst = alone
        .iter()
        .zip(&plain)
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
    assert!(worst < 1e-6, "{worst}");

    // A listening to B while C listens to A: everything renders, finite.
    let mut synth = Wavetable::new();
    synth.prepare(RATE, BLOCK);
    synth.set_param(osc_param(0, OSC_WARP_MODE), WARP_RM as f32);
    synth.set_param(osc_param(0, OSC_WARP), 0.7);
    synth.set_param(osc_param(1, OSC_WARP2_MODE), WARP_AM as f32);
    synth.set_param(osc_param(1, OSC_WARP2), 0.7);
    synth.set_param(osc_param(2, OSC_ON), 1.0);
    synth.set_param(osc_param(2, OSC_UNISON), 6.0);
    synth.set_param(osc_param(2, OSC_WARP_MODE), WARP_FM as f32);
    synth.set_param(osc_param(2, OSC_WARP), 1.0);
    synth.reset();
    let (l, r) = render(&mut synth, &[on(40), on(64), on(96)], 0.5);
    assert!(peak(&l).max(peak(&r)) > 0.05);
}

// ---------------------------------------------------------------------------
// User tables

/// `frames` cycles of `wave` (a function of the phase, `0..1`), `size`
/// samples each, one after the other.
fn cycles(frames: usize, size: usize, wave: impl Fn(usize, f64) -> f64) -> Vec<f32> {
    (0..frames * size)
        .map(|i| wave(i / size, (i % size) as f64 / size as f64) as f32)
        .collect()
}

fn sine(p: f64) -> f64 {
    (2.0 * PI * p).sin()
}

/// The built-in saw's shape: up from 0, the jump at the middle (sampled at
/// its midpoint), and back up to 0.
fn saw(p: f64) -> f64 {
    if p == 0.5 {
        0.0
    } else {
        2.0 * (p + 0.5).fract() - 1.0
    }
}

fn square(p: f64) -> f64 {
    if p == 0.0 || p == 0.5 {
        0.0
    } else if p < 0.5 {
        1.0
    } else {
        -1.0
    }
}

/// Hands `tables` (by oscillator) to `synth`, as the host does.
fn load(synth: &mut Wavetable, tables: [Option<UserTable>; OSC_COUNT]) {
    let tables = UserTables {
        tables: tables.map(|t| t.map(Arc::new)),
    };
    let data: InstrumentData = Arc::new(tables);
    synth.set_data(data);
}

fn user_table(samples: &[f32], frame_size: usize) -> UserTable {
    UserTable::from_samples(samples, frame_size).unwrap()
}

/// [`plain_sine`] with osc A playing `table` as its user table.
fn playing(table: UserTable) -> Wavetable {
    let mut synth = plain_sine();
    load(&mut synth, [Some(table), None, None]);
    synth.set_param(osc_param(0, OSC_TABLE), USER_TABLE as f32);
    synth.reset();
    synth
}

#[test]
fn a_user_sine_plays_in_tune_and_sounds_like_the_builtin_one() {
    assert_eq!(TABLE_LABELS[USER_TABLE], "User");
    assert_eq!(
        PARAMS[osc_param(0, OSC_TABLE)].format(USER_TABLE as f32),
        "User"
    );
    let table = user_table(&cycles(1, 2048, |_, p| sine(p)), 2048);
    assert_eq!(table.frames(), 1);
    let mut synth = playing(table);
    let (user, _) = render(&mut synth, &[on(69)], 1.0);
    let measured = pitch(&user[4800..]);
    println!("user sine at A4: {measured:.4} Hz");
    assert!((measured - 440.0).abs() < 0.05, "{measured}");
    let (builtin, _) = render(&mut plain_sine(), &[on(69)], 1.0);
    let worst = user
        .iter()
        .zip(&builtin)
        .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    println!("against the built-in sine: worst difference {worst:.6}");
    assert!(worst < 2e-3, "{worst}");
}

#[test]
fn a_user_saw_sounds_like_the_builtin_saw() {
    // As in a file: 16-bit samples, 2048 to the frame.
    let samples: Vec<f32> = cycles(1, 2048, |_, p| saw(p))
        .into_iter()
        .map(|s| (s * 32767.0).round() / 32767.0)
        .collect();
    let mut synth = playing(user_table(&samples, 2048));
    let (user, _) = render(&mut synth, &[on(45)], 1.0);
    let mut builtin = plain_sine();
    builtin.set_param(osc_param(0, OSC_POSITION), 2.0 / 3.0);
    builtin.reset();
    let (builtin, _) = render(&mut builtin, &[on(45)], 1.0);
    let measured = pitch(&user[4800..]);
    assert!((measured - 110.0).abs() < 0.02, "{measured}");
    let difference: Vec<f32> = user.iter().zip(&builtin).map(|(a, b)| a - b).collect();
    let error = rms(&difference[4800..]) / rms(&builtin[4800..]);
    println!(
        "user saw against the built-in saw: {:.2}% off",
        error * 100.0
    );
    assert!(error < 0.04, "{error}");
    for harmonic in 1..=8 {
        let f = 110.0 * harmonic as f64;
        let (a, b) = (magnitude(&user[4800..], f), magnitude(&builtin[4800..], f));
        assert!(
            (a / b - 1.0).abs() < 0.05,
            "harmonic {harmonic}: {a} vs {b}"
        );
    }
}

#[test]
fn band_limiting_holds_for_user_tables() {
    // A naive saw, as a file would have it: every harmonic it can hold.
    let naive = cycles(1, 2048, |_, p| 2.0 * (p + 0.5).fract() - 1.0);
    let mut synth = playing(user_table(&naive, 2048));
    synth.set_param(osc_param(0, OSC_TABLE), USER_TABLE as f32);
    // Note 111 is 4978 Hz: harmonics at 5, 10, 15 and 20 kHz.
    let (l, _) = render(&mut synth, &[on(111)], 1.5);
    let alias = alias_db(&l[9600..], hz(111.0));
    println!("5 kHz user saw: non-harmonic energy {alias:.1} dB below the harmonics");
    assert!(alias < -70.0, "{alias} dB");

    // A full 256-frame table, a short single cycle (by DFT) and odd frame
    // sizes, at a high note, through a smooth warp too.
    let morph = cycles(256, 2048, |f, p| {
        let t = f as f64 / 255.0;
        sine(p) * (1.0 - t) + square(p) * t
    });
    let short = cycles(1, 600, |_, p| saw(p) * 0.5 + square(p) * 0.5);
    let odd = cycles(5, 1000, |f, p| sine(p * (f + 1) as f64));
    for (name, table, warp) in [
        ("256 frames", user_table(&morph, 2048), 0.0),
        ("600-sample cycle", user_table(&short, 2048), 0.0),
        ("1000-sample frames", user_table(&odd, 1000), 0.0),
        ("256 frames, bend", user_table(&morph, 2048), 1.0),
    ] {
        let mut synth = playing(table);
        let a = |offset| osc_param(0, offset);
        synth.set_param(a(OSC_POSITION), 0.6);
        synth.set_param(a(OSC_WARP_MODE), WARP_BEND_POS as f32);
        synth.set_param(a(OSC_WARP), warp);
        synth.reset();
        let (l, _) = render(&mut synth, &[on(99)], 1.0);
        let alias = alias_db(&l[9600..], hz(99.0));
        println!("2.5 kHz, {name}: {alias:.1} dB");
        let limit = if warp > 0.0 { -40.0 } else { -65.0 };
        assert!(alias < limit, "{name}: {alias} dB");
    }
}

#[test]
fn position_morphs_through_any_number_of_frames() {
    // Sine, then square, then a sine an octave up.
    let three = cycles(3, 2048, |f, p| match f {
        0 => sine(p),
        1 => square(p),
        _ => sine(2.0 * p),
    });
    let ratio = |table: UserTable, position: f32, harmonic: f64| {
        let mut synth = playing(table);
        synth.set_param(osc_param(0, OSC_POSITION), position);
        synth.reset();
        let (l, _) = render(&mut synth, &[on(57)], 0.5);
        let tail = &l[4800..];
        magnitude(tail, 220.0 * harmonic) / magnitude(tail, 220.0).max(1e-9)
    };
    let third = |position| ratio(user_table(&three, 2048), position, 3.0);
    assert!(third(0.0) < 0.01, "a sine: {}", third(0.0));
    assert!(
        (third(0.5) - 1.0 / 3.0).abs() < 0.03,
        "a square: {}",
        third(0.5)
    );
    let octave = ratio(user_table(&three, 2048), 1.0, 2.0);
    assert!(octave > 100.0, "an octave up: {octave}");
    // Half way between the sine and the square.
    let between = third(0.25);
    assert!((0.1..0.25).contains(&between), "{between}");

    // 256 frames: the square fades in steadily along the position.
    let many = cycles(256, 2048, |f, p| {
        let t = f as f64 / 255.0;
        sine(p) * (1.0 - t) + square(p) * t
    });
    let mut last = -1.0;
    for position in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let now = ratio(user_table(&many, 2048), position, 3.0);
        assert!(now > last, "{position}: {now} after {last}");
        last = now;
    }

    // The meters show the position moving through the user table's frames.
    let mut synth = playing(user_table(&many, 2048));
    synth.set_param(osc_param(0, OSC_POSITION), 0.4);
    render(&mut synth, &[on(60)], 0.1);
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    assert!(
        (meters[METER_POSITION] - 0.4).abs() < 0.01,
        "{}",
        meters[METER_POSITION]
    );
}

#[test]
fn user_with_nothing_loaded_plays_the_first_builtin_table() {
    let render_with = |table: f32, data: Option<[Option<UserTable>; OSC_COUNT]>| {
        let mut synth = plain_on(2);
        synth.set_param(osc_param(2, OSC_POSITION), 0.5);
        if let Some(tables) = data {
            load(&mut synth, tables);
        }
        synth.set_param(osc_param(2, OSC_TABLE), table);
        synth.reset();
        render(&mut synth, &[on(60)], 0.3).0
    };
    let first = render_with(0.0, None);
    assert!(peak(&first) > 0.1);
    // Nothing loaded at all.
    assert_eq!(render_with(USER_TABLE as f32, None), first);
    // Tables for the other oscillators only.
    let saw = cycles(1, 2048, |_, p| saw(p));
    let others = [
        Some(user_table(&saw, 2048)),
        Some(user_table(&saw, 2048)),
        None,
    ];
    assert_eq!(render_with(USER_TABLE as f32, Some(others)), first);
    // Data that isn't user tables is ignored.
    let mut synth = plain_on(2);
    synth.set_data(Arc::new(42u32));
    synth.set_param(osc_param(2, OSC_TABLE), USER_TABLE as f32);
    synth.set_param(osc_param(2, OSC_POSITION), 0.5);
    synth.reset();
    assert_eq!(render(&mut synth, &[on(60)], 0.3).0, first);
    // And C's own table, loaded, is what it plays.
    let own = [None, None, Some(user_table(&saw, 2048))];
    assert_ne!(render_with(USER_TABLE as f32, Some(own)), first);
}

#[test]
fn each_oscillator_plays_its_own_table() {
    // A plays a sine, B a sine two octaves up and C a sine an octave and a
    // fifth up, all at the same note, from their own tables.
    let mut synth = plain_sine();
    let mut tables = [None, None, None];
    for (osc, multiple) in [(0, 1.0), (1, 4.0), (2, 3.0)] {
        tables[osc] = Some(user_table(
            &cycles(1, 2048, |_, p| sine(p * multiple)),
            2048,
        ));
        let p = |offset| osc_param(osc, offset);
        synth.set_param(p(OSC_ON), 1.0);
        synth.set_param(p(OSC_TABLE), USER_TABLE as f32);
        synth.set_param(p(OSC_POSITION), 0.0);
        synth.set_param(p(OSC_UNISON), 0.0);
        synth.set_param(p(OSC_OCTAVE), 4.0);
        synth.set_param(p(OSC_FINE), 0.0);
        synth.set_param(p(OSC_LEVEL), 0.5);
    }
    load(&mut synth, tables);
    synth.reset();
    let (l, _) = render(&mut synth, &[on(45)], 1.0);
    let tail = &l[4800..];
    for multiple in [1.0, 3.0, 4.0] {
        let m = magnitude(tail, 110.0 * multiple);
        assert!(m > 0.05, "{multiple}x: {m}");
    }
    for missing in [2.0, 5.0] {
        let m = magnitude(tail, 110.0 * missing);
        assert!(m < 0.005, "{missing}x: {m}");
    }
}

#[test]
fn ui_drawing_matches_what_plays() {
    // A sine, then a quieter saw: the sine is the loudest frame, at full level.
    let frames = cycles(2, 2048, |f, p| if f == 0 { sine(p) } else { 0.5 * saw(p) });
    let table = user_table(&frames, 2048);
    let mut drawn = [0.0; 128];
    table.draw(0.0, [(WARP_OFF, 0.0); 2], &mut drawn);
    let mut builtin = [0.0; 128];
    frame(0, 0.0, WARP_OFF, 0.0, &mut builtin);
    for (a, b) in drawn.iter().zip(&builtin) {
        assert!((a - b).abs() < 2e-3, "{a} vs {b}");
    }
    table.draw(1.0, [(WARP_MIRROR, 0.5), (WARP_QUANTIZE, 0.3)], &mut drawn);
    assert!(drawn.iter().all(|s| s.is_finite() && s.abs() <= 1.5));
}
