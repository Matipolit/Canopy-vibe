//! Measures the Serum 2-style additions to the wavetable synth: the second
//! filter in serial and parallel, per-source filter routing, the second
//! warp slot, drawn LFO shapes, matrix curves and polarity, unison stacking
//! and warp spread, envelope delay and hold, and that none of it breaks
//! under extreme settings.

mod common;

use common::wavetable::{default_synth, render};
use common::{magnitude, off, on, peak, pitch, rms};
use instruments::builtin::wavetable::*;
use instruments::{Instrument, MAX_METERS, ParameterKind};
use midi::{NoteEvent, NoteEventKind};

const RATE: f32 = 48_000.0;

/// A plain sine from osc A alone: no unison, filters, matrix or velocity,
/// a fast attack and full sustain, so tests start from a known sound.
fn plain_sine() -> Wavetable {
    let mut synth = common::wavetable::plain_sine();
    synth.set_param(ENV1.attack, 0.5);
    synth.reset();
    synth
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(1e-12).log10()
}

/// Each parameter's value as the synth was given it, for the UI helpers.
fn values_of(changes: &[(usize, f32)]) -> Vec<f32> {
    let mut values: Vec<f32> = PARAMS.iter().map(|p| p.default_value()).collect();
    for (index, value) in changes {
        values[*index] = *value;
    }
    values
}

// ---------------------------------------------------------------------------
// Filter 2 and routing

/// The filter settings shared by the filter tests: no drive, key tracking
/// or modulation, full mix.
fn filters(f1: (usize, f32, f32), f2: (usize, f32, f32), parallel: bool) -> Vec<(usize, f32)> {
    let mut changes = Vec::new();
    for (filter, (kind, cutoff, resonance)) in [f1, f2].into_iter().enumerate() {
        let p = |index| filter_param(filter, index);
        changes.extend([
            (p(FILTER_ON), 1.0),
            (p(FILTER_TYPE), kind as f32),
            (p(FILTER_CUTOFF), cutoff),
            (p(FILTER_RESONANCE), resonance),
            (p(FILTER_DRIVE), 0.0),
            (p(FILTER_KEYTRACK), 0.0),
            (p(FILTER_MIX), 1.0),
        ]);
    }
    changes.push((FILTER_ROUTING, if parallel { 1.0 } else { 0.0 }));
    changes
}

/// A 110 Hz saw from osc A through `changes`; the tail after the first 0.2 s.
fn saw_through(changes: &[(usize, f32)]) -> Vec<f32> {
    let mut synth = plain_sine();
    synth.set_param(osc_param(0, OSC_POSITION), 2.0 / 3.0);
    for (index, value) in changes {
        synth.set_param(*index, *value);
    }
    synth.reset();
    let (l, _) = render(&mut synth, &[on(45)], 1.2);
    l[(0.2 * RATE) as usize..].to_vec()
}

/// The saw's response through `changes` at harmonics of 110 Hz, measured
/// against the unfiltered saw, next to the response `expected` gives.
fn compare_response(
    name: &str,
    changes: &[(usize, f32)],
    expected: impl Fn(&[f32], f32) -> f32,
) -> usize {
    let reference = saw_through(&[]);
    let filtered = saw_through(changes);
    let values = values_of(changes);
    let mut checked = 0;
    for harmonic in [1, 2, 3, 5, 8, 13, 21, 34, 55, 89] {
        let hz = 110.0 * harmonic as f64;
        let measured = db(magnitude(&filtered, hz) / magnitude(&reference, hz));
        let drawn = expected(&values, hz as f32) as f64;
        println!("{name}: {hz:6.0} Hz measured {measured:7.2} dB, set {drawn:7.2} dB");
        if drawn < -50.0 {
            assert!(measured < -40.0, "{name} at {hz} Hz: {measured} vs {drawn}");
            continue;
        }
        assert!(
            (measured - drawn).abs() < 1.0,
            "{name} at {hz} Hz: measured {measured:.2} dB, set {drawn:.2} dB"
        );
        checked += 1;
    }
    checked
}

#[test]
fn serial_filters_multiply_their_responses() {
    // A resonant low-pass into a high-pass: a band around 300..2000 Hz.
    let changes = filters((LP24, 2000.0, 0.4), (HP12, 300.0, 0.2), false);
    let checked = compare_response("serial", &changes, |values, hz| {
        filter_curve_db(0, values, hz, RATE) + filter_curve_db(1, values, hz, RATE)
    });
    assert!(checked >= 6);
}

#[test]
fn in_parallel_each_filter_hears_only_its_own_sources() {
    // In parallel, osc A (into filter 1 by default) misses filter 2.
    let changes = filters((LP24, 2000.0, 0.4), (HP12, 300.0, 0.2), true);
    let checked = compare_response("parallel, A into 1", &changes, |values, hz| {
        filter_curve_db(0, values, hz, RATE)
    });
    assert!(checked >= 5);
    // Sent to filter 2 instead, it hears only that one, in either routing.
    for parallel in [true, false] {
        let mut changes = filters((LP24, 2000.0, 0.4), (HP12, 300.0, 0.2), parallel);
        changes.push((FILTER_TARGET_OSC_A, 1.0));
        let name = if parallel { "parallel" } else { "serial" };
        let checked = compare_response(&format!("{name}, A into 2"), &changes, |values, hz| {
            filter_curve_db(1, values, hz, RATE)
        });
        assert!(checked >= 5);
    }
}

#[test]
fn a_switched_off_second_filter_changes_nothing() {
    let reference = saw_through(&[(FILTER_ON, 1.0), (FILTER_DRIVE, 0.0)]);
    for parallel in [0.0, 1.0] {
        let other = saw_through(&[
            (FILTER_ON, 1.0),
            (FILTER_DRIVE, 0.0),
            (FILTER2_ON, 0.0),
            (FILTER2_TYPE, BP12 as f32),
            (FILTER_ROUTING, parallel),
        ]);
        let difference = reference
            .iter()
            .zip(&other)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(difference < 1e-6, "{difference}");
    }
}

/// Which way each source can go.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Route {
    Direct,
    Filter1,
    Filter2,
}

/// Low (110 Hz) and high (3960 Hz) levels of one source alone through a
/// low-pass at 400 Hz (filter 1) and a high-pass at 2 kHz (filter 2),
/// against the same source with both filters off, in dB.
fn routed(source: usize, route: Route, parallel: bool) -> (f64, f64) {
    let make = |filtered: bool| {
        let mut synth = plain_sine();
        let a = |offset| osc_param(0, offset);
        synth.set_param(a(OSC_ON), 0.0);
        match source {
            0 => {
                synth.set_param(a(OSC_ON), 1.0);
                synth.set_param(a(OSC_POSITION), 2.0 / 3.0);
            }
            1 => {
                let b = |offset| osc_param(1, offset);
                synth.set_param(b(OSC_ON), 1.0);
                synth.set_param(b(OSC_POSITION), 2.0 / 3.0);
                synth.set_param(b(OSC_UNISON), 0.0);
                synth.set_param(b(OSC_RANDOM), 0.0);
                synth.set_param(b(OSC_OCTAVE), 4.0);
                synth.set_param(b(OSC_FINE), 0.0);
            }
            _ => {
                synth.set_param(SUB_ON, 1.0);
                synth.set_param(SUB_SHAPE, 2.0); // Saw
                synth.set_param(SUB_OCTAVE, 4.0);
            }
        }
        if filtered {
            let changes = filters((LP24, 400.0, 0.0), (HP24, 2000.0, 0.0), parallel);
            for (index, value) in changes {
                synth.set_param(index, value);
            }
            let (switch, target) = SOURCE_ROUTES[source];
            synth.set_param(switch, if route == Route::Direct { 0.0 } else { 1.0 });
            synth.set_param(target, if route == Route::Filter2 { 1.0 } else { 0.0 });
        }
        synth.reset();
        let (l, _) = render(&mut synth, &[on(45)], 1.0);
        l[(0.2 * RATE) as usize..].to_vec()
    };
    let (dry, wet) = (make(false), make(true));
    let level = |hz: f64| db(magnitude(&wet, hz) / magnitude(&dry, hz));
    (level(110.0), level(3960.0))
}

#[test]
fn each_source_goes_where_it_is_routed() {
    for source in 0..3 {
        for parallel in [false, true] {
            for route in [Route::Direct, Route::Filter1, Route::Filter2] {
                let (low, high) = routed(source, route, parallel);
                println!(
                    "source {source}, {route:?}, {}: 110 Hz {low:6.1} dB, 3960 Hz {high:6.1} dB",
                    if parallel { "parallel" } else { "serial" }
                );
                let (low_passes, high_passes) = match route {
                    Route::Direct => (true, true),
                    // In serial, filter 1 feeds the high-pass as well.
                    Route::Filter1 => (true, false),
                    Route::Filter2 => (false, true),
                };
                let serial_through_both = route == Route::Filter1 && !parallel;
                let low_ok = if low_passes && !serial_through_both {
                    low.abs() < 1.0
                } else {
                    low < -30.0
                };
                let high_ok = if high_passes {
                    high.abs() < 1.0
                } else {
                    high < -30.0
                };
                assert!(low_ok && high_ok, "source {source} {route:?}: {low} {high}");
            }
        }
    }
}

#[test]
fn noise_goes_where_it_is_routed() {
    // White noise's level above 4 kHz, through a low-pass at 500 Hz.
    let level = |route: Option<usize>| {
        let mut synth = plain_sine();
        synth.set_param(osc_param(0, OSC_ON), 0.0);
        synth.set_param(NOISE_ON, 1.0);
        let changes = filters((LP24, 500.0, 0.0), (LP24, 500.0, 0.0), true);
        for (index, value) in changes {
            synth.set_param(index, value);
        }
        synth.set_param(FILTER_NOISE, if route.is_some() { 1.0 } else { 0.0 });
        synth.set_param(FILTER_TARGET_NOISE, route.unwrap_or(0) as f32);
        synth.reset();
        let (l, _) = render(&mut synth, &[on(60)], 0.5);
        let bright: Vec<f32> = l.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&bright[4800..])
    };
    let direct = level(None);
    for filter in [0, 1] {
        let filtered = level(Some(filter));
        println!(
            "noise into filter {}: {filtered:.5} vs {direct:.5}",
            filter + 1
        );
        assert!(filtered < direct * 0.05);
    }
}

#[test]
fn switching_filter_2_and_its_routing_does_not_click() {
    let mut synth = plain_sine();
    let b = |offset| osc_param(1, offset);
    synth.set_param(b(OSC_ON), 1.0);
    synth.set_param(b(OSC_POSITION), 0.0);
    synth.set_param(b(OSC_UNISON), 0.0);
    synth.set_param(b(OSC_OCTAVE), 5.0);
    for (index, value) in filters((LP12, 400.0, 0.5), (LP12, 600.0, 0.3), false) {
        synth.set_param(index, value);
    }
    synth.set_param(FILTER2_ON, 0.0);
    synth.reset();
    let (steady, _) = render(&mut synth, &[on(45)], 0.5);
    let largest = |s: &[f32]| s.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    let normal = largest(&steady[12000..]);
    for (name, index, value) in [
        ("filter 2 on", FILTER2_ON, 1.0),
        ("cutoff 2 jump", FILTER2_CUTOFF, 3000.0),
        ("filter 2 off", FILTER2_ON, 0.0),
        ("filter 2 back on", FILTER2_ON, 1.0),
    ] {
        synth.set_param(index, value);
        let (l, r) = render(&mut synth, &[], 0.1);
        let step = largest(&l).max(largest(&r));
        println!("{name}: largest step {step:.4} (steady {normal:.4})");
        assert!(step < normal * 2.5, "{name}: {step} vs {normal}");
    }
}

// ---------------------------------------------------------------------------
// Two warps

/// One second of osc A as a 440 Hz sine with warps `warps`, from the 1000th
/// sample on, and the phase of every sample from the start (see [`heard`]).
fn warped_sine(warps: [(usize, f32); 2]) -> (Vec<f32>, Vec<f32>) {
    warped_sine_with(warps, &[])
}

/// [`warped_sine`] with parameters `changes` set too.
fn warped_sine_with(warps: [(usize, f32); 2], changes: &[(usize, f32)]) -> (Vec<f32>, Vec<f32>) {
    let mut synth = plain_sine();
    for &(index, value) in changes {
        synth.set_param(index, value);
    }
    let a = |offset| osc_param(0, offset);
    synth.set_param(a(OSC_WARP_MODE), warps[0].0 as f32);
    synth.set_param(a(OSC_WARP), warps[0].1);
    synth.set_param(a(OSC_WARP2_MODE), warps[1].0 as f32);
    synth.set_param(a(OSC_WARP2), warps[1].1);
    synth.reset();
    let (l, _) = render(&mut synth, &[on(69)], 1.0);
    // The oscillator starts at phase 0 and steps by 440 / 48000 a sample,
    // in f32, as this replays.
    let inc = 440.0f32 / RATE;
    let mut phase = 0.0f32;
    let mut phases = Vec::with_capacity(l.len());
    for _ in 0..l.len() {
        phases.push(phase);
        phase += inc;
        if phase >= 1.0 {
            phase -= 1.0;
        }
    }
    (l[1000..].to_vec(), phases)
}

/// A wave worked out by hand for every sample of [`warped_sine`], as the
/// synth plays it: without its DC (see [`DC_BLOCK_HZ`]), from the 1000th
/// sample on.
fn heard(expected: Vec<f32>) -> Vec<f32> {
    dc_blocked(&expected, RATE)[1000..].to_vec()
}

/// How far `signal` is from `gain × expected`, with the best gain, as a
/// share of its RMS.
fn misfit(signal: &[f32], expected: &[f32]) -> f32 {
    let dot: f32 = signal.iter().zip(expected).map(|(a, b)| a * b).sum();
    let energy: f32 = expected.iter().map(|b| b * b).sum();
    let gain = dot / energy.max(1e-12);
    let residual: Vec<f32> = signal
        .iter()
        .zip(expected)
        .map(|(a, b)| a - gain * b)
        .collect();
    rms(&residual) / rms(signal).max(1e-12)
}

#[test]
fn either_warp_slot_alone_sounds_the_same() {
    for (mode, name) in WARPS_A.iter().enumerate() {
        let (first, _) = warped_sine([(mode, 0.6), (WARP_OFF, 0.0)]);
        let (second, _) = warped_sine([(WARP_OFF, 0.0), (mode, 0.6)]);
        let difference = first
            .iter()
            .zip(&second)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(difference < 1e-4, "{name}: {difference}");
    }
}

#[test]
fn the_second_warp_works_on_what_the_first_made() {
    let tau = std::f32::consts::TAU;
    let bend = |q: f32, a: f32| q - 0.95 * a * (tau * q).sin() / tau;
    // Bend+ at 0.7, then PWM at 0.5: the bent sine squeezed into the first
    // 52.5% of each cycle, silence after.
    let width = 1.0 - 0.95 * 0.5;
    let bend_then_pwm = |p: f32| {
        if p < width {
            (tau * bend(p / width, 0.7)).sin()
        } else {
            0.0
        }
    };
    // The other way round: the pulse is bent as a whole.
    let pwm_then_bend = |p: f32| {
        let q = bend(p, 0.7);
        if q < width {
            (tau * q / width).sin()
        } else {
            0.0
        }
    };
    let (played, phases) = warped_sine([(WARP_BEND_POS, 0.7), (WARP_PWM, 0.5)]);
    let right = heard(phases.iter().map(|p| bend_then_pwm(*p)).collect());
    let wrong = heard(phases.iter().map(|p| pwm_then_bend(*p)).collect());
    let (fit, other) = (misfit(&played, &right), misfit(&played, &wrong));
    println!(
        "bend then PWM: {:.3}% off; against the reverse order {:.1}%",
        fit * 100.0,
        other * 100.0
    );
    assert!(fit < 0.002, "{fit}");
    assert!(other > 0.1, "{other}");

    // A quantize after a mirror: the mirrored sine rounded to few levels.
    let (played, phases) = warped_sine([(WARP_MIRROR, 1.0), (WARP_QUANTIZE, 0.9)]);
    let levels = (1.0f32 + 7.0 * 0.1).exp2();
    let expected = heard(
        phases
            .iter()
            .map(|p| {
                let mirrored = if *p < 0.5 { 2.0 * p } else { 2.0 - 2.0 * p };
                ((tau * mirrored).sin() * levels).round() / levels
            })
            .collect(),
    );
    let fit = misfit(&played, &expected);
    println!("mirror then quantize: {:.3}% off", fit * 100.0);
    assert!(fit < 0.01, "{fit}");
}

#[test]
fn the_drawn_frame_matches_what_plays() {
    let warps = [(WARP_ASYM_POS, 0.8), (WARP_FLIP, 0.3)];
    let (played, phases) = warped_sine(warps);
    let mut cycle = [0.0f32; 4096];
    frame_warped(0, 0.0, warps, &mut cycle);
    let drawn = heard(
        phases
            .iter()
            .map(|p| cycle[((p * 4096.0) as usize).min(4095)])
            .collect(),
    );
    let fit = misfit(&played, &drawn);
    println!("drawn vs played: {:.3}% off", fit * 100.0);
    assert!(fit < 0.01, "{fit}");
    // And the single-warp helper is the two-warp one with the second off.
    let mut one = [0.0f32; 256];
    let mut two = [0.0f32; 256];
    frame(3, 0.4, WARP_SYNC, 0.6, &mut one);
    frame_warped(3, 0.4, [(WARP_SYNC, 0.6), (WARP_OFF, 0.9)], &mut two);
    assert_eq!(one, two);
}

/// Osc A's remap curve set to `nodes` (`(x, y, curve)` each).
fn remap_curve(nodes: &[(f32, f32, f32)]) -> Vec<(usize, f32)> {
    let a = |offset| osc_param(0, offset);
    let mut changes = vec![(a(OSC_REMAP_NODES), (nodes.len() - 2) as f32)];
    for (i, &(x, y, curve)) in nodes.iter().enumerate() {
        changes.push((a(OSC_REMAP_X + i), x));
        changes.push((a(OSC_REMAP_Y + i), y));
        changes.push((a(OSC_REMAP_CURVE + i), curve));
    }
    changes
}

#[test]
fn remap_warps_play_the_drawn_curve() {
    // Lingers on the cycle's start, then rushes through the rest.
    let curve = remap_curve(&[(0.0, 0.0, 0.5), (0.7, 0.3, -0.4), (1.0, 1.0, 0.0)]);
    let remap = remap_table(&values_of(&curve), 0);
    for mode in [WARP_REMAP, WARP_REMAP_MIRROR, WARP_REMAP_X4] {
        let warps = [(mode, 0.8), (WARP_OFF, 0.0)];
        let (played, phases) = warped_sine_with(warps, &curve);
        let mut cycle = [0.0f32; 4096];
        TableSource::BuiltIn(0).draw_remapped(
            0.0,
            (MORPH_OFF, 0.0, 0.0),
            warps,
            &remap,
            &mut cycle,
        );
        let drawn = heard(
            phases
                .iter()
                .map(|p| cycle[((p * 4096.0) as usize).min(4095)])
                .collect(),
        );
        let fit = misfit(&played, &drawn);
        let (plain, _) = warped_sine([(WARP_OFF, 0.0); 2]);
        let change = misfit(&played, &plain);
        println!(
            "{}: drawn vs played {:.3}% off, {:.1}% from plain",
            WARPS_A[mode],
            fit * 100.0,
            change * 100.0
        );
        assert!(fit < 0.01, "{}: {fit}", WARPS_A[mode]);
        assert!(change > 0.1, "{}: {change}", WARPS_A[mode]);
    }
    // A straight curve changes nothing, at any amount.
    let straight = remap_curve(&[(0.0, 0.0, 0.0), (1.0, 1.0, 0.0)]);
    let (played, _) = warped_sine_with([(WARP_REMAP, 1.0), (WARP_OFF, 0.0)], &straight);
    let (plain, _) = warped_sine([(WARP_OFF, 0.0); 2]);
    let difference = played
        .iter()
        .zip(&plain)
        .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(difference < 1e-3, "{difference}");
}

#[test]
fn a_remap_curve_drawn_while_playing_is_heard() {
    let mut synth = plain_sine();
    let a = |offset| osc_param(0, offset);
    synth.set_param(a(OSC_WARP_MODE), WARP_REMAP as f32);
    synth.set_param(a(OSC_WARP), 1.0);
    for (index, value) in remap_curve(&[(0.0, 0.0, 0.0), (1.0, 1.0, 0.0)]) {
        synth.set_param(index, value);
    }
    synth.reset();
    let (before, _) = render(&mut synth, &[on(69)], 0.2);
    // Reading the cycle twice as fast: an octave up.
    for (index, value) in remap_curve(&[
        (0.0, 0.0, 0.0),
        (0.5, 1.0, 0.0),
        (0.5, 0.0, 0.0),
        (1.0, 1.0, 0.0),
    ]) {
        synth.set_param(index, value);
    }
    let (after, _) = render(&mut synth, &[], 0.2);
    let (was, now) = (pitch(&before[2000..]), pitch(&after[2000..]));
    println!("pitch before {was:.1} Hz, after {now:.1} Hz");
    assert!((was - 440.0).abs() < 5.0, "{was}");
    assert!((now - 880.0).abs() < 10.0, "{now}");
}

#[test]
fn the_second_warp_is_a_mod_destination() {
    // Mod wheel → A Warp 2 (PWM): the wheel narrows the pulse.
    let mut synth = plain_sine();
    let a = |offset| osc_param(0, offset);
    synth.set_param(a(OSC_WARP2_MODE), WARP_PWM as f32);
    synth.set_param(slot_param(0, SLOT_SOURCE), SRC_MOD_WHEEL as f32);
    synth.set_param(slot_param(0, SLOT_DEST), DEST_WARP2_A as f32);
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.8);
    synth.reset();
    let (still, _) = render(&mut synth, &[on(57)], 0.5);
    let wheel = NoteEvent {
        frame: 0,
        kind: NoteEventKind::Control {
            controller: midi::MOD_WHEEL,
            value: 127,
        },
    };
    let (narrow, _) = render(&mut synth, &[wheel], 0.5);
    // A pulse-squeezed sine gains harmonics; a plain one has none.
    let third = |s: &[f32]| db(magnitude(&s[4800..], 660.0) / magnitude(&s[4800..], 220.0));
    let (before, after) = (third(&still), third(&narrow));
    println!("3rd harmonic: {before:.1} dB -> {after:.1} dB");
    assert!(before < -60.0 && after > -20.0, "{before} {after}");
}

// ---------------------------------------------------------------------------
// Drawn LFOs

/// A stepped pattern of the custom shape's points.
fn pattern(i: usize) -> f32 {
    ((i * 7) % LFO_POINTS) as f32 / (LFO_POINTS - 1) as f32
}

#[test]
fn a_custom_lfo_plays_its_drawn_points() {
    // LFO 1, custom, retriggered, a quarter cycle per second, steps (no
    // glide), moving osc A's pitch by up to ±6 semitones.
    let mut synth = plain_sine();
    let l = |offset| lfo_param(0, offset);
    let mut changes = vec![
        (l(LFO_SHAPE), SHAPE_CUSTOM as f32),
        (l(LFO_MODE), MODE_RETRIG as f32),
        (l(LFO_SYNC), 0.0),
        (l(LFO_RATE), 0.25),
        (l(LFO_GLIDE), 0.0),
        (slot_param(0, SLOT_SOURCE), SRC_LFO1 as f32),
        (slot_param(0, SLOT_DEST), (DEST_OSC_A + DEST_PITCH) as f32),
        (slot_param(0, SLOT_AMOUNT), 0.25),
    ];
    for i in 0..LFO_POINTS {
        changes.push((l(LFO_POINT + i), pattern(i)));
    }
    for (index, value) in &changes {
        synth.set_param(*index, *value);
    }
    synth.reset();
    let (out, _) = render(&mut synth, &[on(69)], 4.0);
    // Each step lasts 4 s / 32 = 125 ms; measure the middle of each.
    let step = (4.0 * RATE) as usize / LFO_POINTS;
    let mut worst = 0.0f32;
    for i in 0..LFO_POINTS {
        let middle = i * step + step / 2;
        let hz = pitch(&out[middle - step / 4..middle + step / 4]);
        let semitones = 6.0 * (2.0 * pattern(i) - 1.0);
        let expected = 440.0 * (semitones / 12.0).exp2();
        let cents = 1200.0 * (hz / expected).log2();
        worst = worst.max(cents.abs());
        assert!(cents.abs() < 2.0, "step {i}: {hz} Hz vs {expected}");
    }
    println!("custom LFO steps: worst pitch error {worst:.2} cents over 32 steps");

    // The meters follow the drawn shape as the UI draws it, glide and all.
    let values = values_of(&changes);
    for glide in [0.0, 0.4, 1.0] {
        let mut synth = plain_sine();
        let mut values = values.clone();
        values[l(LFO_GLIDE)] = glide;
        values[l(LFO_RATE)] = 1.5;
        for (index, value) in values.iter().enumerate() {
            synth.set_param(index, *value);
        }
        synth.reset();
        let (mut left, mut right) = ([0.0; 64], [0.0; 64]);
        synth.process(&[on(60)], &mut left, &mut right);
        let mut meters = [0.0; MAX_METERS];
        let mut largest = 0.0f32;
        for _ in 0..1500 {
            synth.process(&[], &mut left, &mut right);
            synth.meters(&mut meters);
            let (value, phase) = (meters[METER_LFO_VALUE], meters[METER_LFO_PHASE]);
            let drawn = lfo_shape_at(&values, 0, phase);
            largest = largest.max((value - drawn).abs());
        }
        println!("glide {glide}: meters within {largest:.2e} of the drawn shape");
        assert!(largest < 1e-4, "glide {glide}: {largest}");
    }
}

#[test]
fn a_one_shot_custom_lfo_stops_on_its_last_point() {
    let mut synth = plain_sine();
    let l = |offset| lfo_param(2, offset);
    synth.set_param(l(LFO_SHAPE), SHAPE_CUSTOM as f32);
    synth.set_param(l(LFO_MODE), MODE_ENV as f32);
    synth.set_param(l(LFO_SYNC), 0.0);
    synth.set_param(l(LFO_RATE), 4.0);
    synth.set_param(l(LFO_POINT + LFO_POINTS - 1), 0.9);
    synth.reset();
    render(&mut synth, &[on(60)], 1.0);
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    assert!((meters[METER_LFO_VALUE + 2] - 0.8).abs() < 1e-4);
}

// ---------------------------------------------------------------------------
// Matrix curves and polarity

/// The slot's output with no note playing, for macro 1 at `macro_value`.
fn slot_output(synth: &mut Wavetable, macro_value: f32) -> f32 {
    synth.set_param(MACROS.start, macro_value);
    let (mut left, mut right) = ([0.0; 64], [0.0; 64]);
    synth.process(&[], &mut left, &mut right);
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    live_modulation(synth.param_values(), &meters).slots[0]
}

#[test]
fn matrix_curves_bend_as_set() {
    let mut synth = plain_sine();
    synth.set_param(slot_param(0, SLOT_SOURCE), SRC_MACRO1 as f32);
    synth.set_param(slot_param(0, SLOT_DEST), DEST_CUTOFF as f32);
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.8);
    for curve in [-1.0, -0.5, 0.0, 0.3, 1.0] {
        synth.set_param(slot_param(0, SLOT_CURVE), curve);
        for x in [0.0, 0.1, 0.25, 0.5, 0.75, 1.0] {
            let out = slot_output(&mut synth, x);
            let expected = 0.8 * dsp::stage_shape(x, curve);
            assert!((out - expected).abs() < 1e-5, "curve {curve} at {x}: {out}");
        }
    }
    // Negative bends up (fast start), positive bends down (slow start).
    synth.set_param(slot_param(0, SLOT_CURVE), -0.6);
    assert!(slot_output(&mut synth, 0.3) > 0.8 * 0.6);
    synth.set_param(slot_param(0, SLOT_CURVE), 0.6);
    assert!(slot_output(&mut synth, 0.3) < 0.8 * 0.1);

    // Polarity: the macro remapped to -1..1, then curved symmetrically.
    synth.set_param(slot_param(0, SLOT_POLARITY), POLARITY_BIPOLAR as f32);
    synth.set_param(slot_param(0, SLOT_CURVE), -0.5);
    for x in [0.0, 0.2, 0.5, 0.8, 1.0] {
        let out = slot_output(&mut synth, x);
        let expected = 0.8 * matrix_curve(2.0 * x - 1.0, -0.5);
        assert!(
            (out - expected).abs() < 1e-5,
            "bipolar at {x}: {out} vs {expected}"
        );
    }
    assert!((slot_output(&mut synth, 0.2) + slot_output(&mut synth, 0.8)).abs() < 1e-5);
    // Asking a unipolar source for unipolar changes nothing.
    synth.set_param(slot_param(0, SLOT_POLARITY), POLARITY_UNIPOLAR as f32);
    synth.set_param(slot_param(0, SLOT_CURVE), 0.0);
    assert!((slot_output(&mut synth, 0.3) - 0.24).abs() < 1e-6);
}

#[test]
fn a_bipolar_source_made_unipolar_only_pushes_one_way() {
    // LFO 1 (sine, -1..1) made unipolar: 0..1, centred on one half.
    let mut synth = plain_sine();
    synth.set_param(lfo_param(0, LFO_RATE), 3.0);
    synth.set_param(slot_param(0, SLOT_SOURCE), SRC_LFO1 as f32);
    synth.set_param(slot_param(0, SLOT_DEST), DEST_CUTOFF as f32);
    synth.set_param(slot_param(0, SLOT_AMOUNT), 1.0);
    synth.set_param(slot_param(0, SLOT_POLARITY), POLARITY_UNIPOLAR as f32);
    synth.reset();
    let (mut left, mut right) = ([0.0; 64], [0.0; 64]);
    let mut meters = [0.0; MAX_METERS];
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    // A note held, whose LFO the meters show.
    synth.process(&[on(60)], &mut left, &mut right);
    for _ in 0..800 {
        synth.process(&[], &mut left, &mut right);
        synth.meters(&mut meters);
        let lfo = meters[METER_LFO_VALUE];
        let slot = live_modulation(synth.param_values(), &meters).slots[0];
        assert!((slot - (lfo + 1.0) / 2.0).abs() < 1e-5, "{lfo} -> {slot}");
        low = low.min(slot);
        high = high.max(slot);
    }
    assert!(low < 0.01 && high > 0.99, "{low}..{high}");
}

#[test]
fn a_curved_slot_bends_what_is_heard() {
    // Macro 1 → A Pitch, ±12 semitones, curve 0.6: at half the macro, the
    // pitch moves by 12 × shape(0.5) semitones rather than 6.
    let mut synth = plain_sine();
    synth.set_param(slot_param(0, SLOT_SOURCE), SRC_MACRO1 as f32);
    synth.set_param(slot_param(0, SLOT_DEST), (DEST_OSC_A + DEST_PITCH) as f32);
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.5);
    synth.set_param(slot_param(0, SLOT_CURVE), 0.6);
    synth.set_param(MACROS.start, 0.5);
    synth.reset();
    let (l, _) = render(&mut synth, &[on(69)], 0.5);
    let semitones = 12.0 * dsp::stage_shape(0.5, 0.6);
    let expected = 440.0 * (semitones / 12.0).exp2();
    let hz = pitch(&l[4800..]);
    println!("curved macro: {hz:.2} Hz, expected {expected:.2} ({semitones:.2} semitones)");
    assert!((hz - expected).abs() < 0.05, "{hz} vs {expected}");
}

// ---------------------------------------------------------------------------
// Unison stacking and warp spread

/// The strongest peak within 2 Hz of `hz`, scanned in 0.05 Hz steps, and
/// its magnitude.
fn peak_near(signal: &[f32], hz: f64) -> (f64, f64) {
    (0..=80)
        .map(|i| {
            let f = hz - 2.0 + i as f64 * 0.05;
            (f, magnitude(signal, f))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
}

fn stacked(voices: usize, detune: f32, stack: usize) -> Vec<f32> {
    let mut synth = plain_sine();
    let a = |offset| osc_param(0, offset);
    synth.set_param(a(OSC_UNISON), (voices - 1) as f32);
    synth.set_param(a(OSC_DETUNE), detune);
    synth.set_param(a(OSC_WIDTH), 0.0);
    synth.set_param(a(OSC_BLEND), 1.0);
    synth.set_param(a(OSC_STACK), stack as f32);
    synth.reset();
    let (l, _) = render(&mut synth, &[on(69)], 2.2);
    l[(0.2 * RATE) as usize..].to_vec()
}

#[test]
fn unison_stacking_puts_voices_at_the_stacked_intervals() {
    // Three voices, no detune, stacked a fifth and an octave up: one voice
    // on each interval, equally loud, and nothing in between.
    let stack = STACKS.iter().position(|s| *s == "+7 +12").unwrap();
    assert_eq!(STACK_INTERVALS[stack], [0.0, 7.0, 12.0]);
    let out = stacked(3, 0.0, stack);
    let levels: Vec<f64> = [0.0, 7.0, 12.0]
        .iter()
        .map(|st| magnitude(&out, 440.0 * 2f64.powf(st / 12.0)))
        .collect();
    println!("+7 +12 stack: levels {levels:.4?}");
    for level in &levels {
        assert!((level / levels[0] - 1.0).abs() < 0.02, "{levels:?}");
    }
    for hz in [550.0, 600.0, 750.0, 1000.0] {
        assert!(magnitude(&out, hz) < levels[0] * 1e-3, "{hz} Hz");
    }

    // Every stack with five voices detuned ±20 cents: each voice where its
    // place in the detune and its layer put it, as loud as the others, and
    // nothing on the intervals the stack doesn't have.
    let all_intervals = [-12.0, 0.0, 7.0, 12.0, 19.0, 24.0];
    for (stack, intervals) in STACK_INTERVALS.iter().enumerate() {
        let voices = 5;
        let out = stacked(voices, 20.0, stack);
        let mut found = Vec::new();
        for u in 0..voices {
            let place = 2.0 * u as f64 / (voices - 1) as f64 - 1.0;
            let interval = stack_interval(stack, u, voices) as f64;
            assert!(intervals.contains(&(interval as f32)));
            let hz = 440.0 * 2f64.powf(20.0 * place / 1200.0 + interval / 12.0);
            let (got, level) = peak_near(&out, hz);
            assert!(
                (got - hz).abs() < 0.3,
                "{}: voice {u} at {got}, not {hz}",
                STACKS[stack]
            );
            found.push((got, level));
        }
        let loudest = found.iter().fold(0.0f64, |m, p| m.max(p.1));
        for (_, level) in &found {
            assert!(*level > loudest * 0.7, "{}: {found:.3?}", STACKS[stack]);
        }
        for interval in all_intervals {
            if !intervals.contains(&(interval as f32)) {
                let hz = 440.0 * 2f64.powf(interval / 12.0);
                assert!(
                    magnitude(&out, hz) < loudest * 1e-3,
                    "{}: {hz} Hz",
                    STACKS[stack]
                );
            }
        }
        // The note itself always sounds: the center voice stays on it.
        assert!(found.iter().any(|(hz, _)| (hz - 440.0).abs() < 0.3));
        println!(
            "{:>8}: {:.2?} Hz",
            STACKS[stack],
            found.iter().map(|p| p.0).collect::<Vec<_>>()
        );
    }
}

#[test]
fn warp_spread_fans_the_warp_out_across_the_unison() {
    // Two voices an octave apart (stack +12), PWM at 0.5. With full spread
    // the lower voice sits at warp 0, a plain sine with no 3rd harmonic;
    // the upper one's harmonics are all multiples of 880 Hz.
    let level_at_1320 = |spread: f32| {
        let mut synth = plain_sine();
        let a = |offset| osc_param(0, offset);
        synth.set_param(a(OSC_UNISON), 1.0);
        synth.set_param(a(OSC_DETUNE), 0.0);
        synth.set_param(a(OSC_STACK), 1.0);
        synth.set_param(a(OSC_WARP_MODE), WARP_PWM as f32);
        synth.set_param(a(OSC_WARP), 0.5);
        synth.set_param(a(OSC_WARP_SPREAD), spread);
        synth.reset();
        let (l, _) = render(&mut synth, &[on(69)], 1.0);
        let tail = &l[4800..];
        db(magnitude(tail, 1320.0) / magnitude(tail, 880.0))
    };
    let (none, full) = (level_at_1320(0.0), level_at_1320(1.0));
    println!("1320 Hz against 880 Hz: no spread {none:.1} dB, full spread {full:.1} dB");
    assert!(none > -25.0 && full < -60.0, "{none} {full}");
}

// ---------------------------------------------------------------------------
// Envelope delay and hold

#[test]
fn envelope_delay_and_hold_shape_the_note() {
    let mut synth = plain_sine();
    synth.set_param(ENV1.attack, 1.0);
    synth.set_param(ENV1.decay, 200.0);
    synth.set_param(ENV1.sustain, 0.0);
    synth.set_param(ENV1.decay + 4, 0.0); // linear decay
    synth.set_param(env_param(0, ENV_DELAY), 100.0);
    synth.set_param(env_param(0, ENV_HOLD), 150.0);
    synth.reset();
    let (l, _) = render(&mut synth, &[on(69)], 0.6);
    // The level over 5 ms: two cycles of the sine, so its peak.
    let window = |ms: f32| {
        let at = (ms / 1000.0 * RATE) as usize;
        peak(&l[at..at + 240])
    };
    let full = window(120.0);
    println!(
        "delay/hold: {:.3} at 50 ms, {full:.3} at 120 ms, {:.3} at 240 ms, {:.3} at 350 ms, {:.3} at 500 ms",
        window(50.0),
        window(240.0),
        window(350.0),
        window(500.0)
    );
    assert_eq!(
        peak(&l[..(0.099 * RATE) as usize]),
        0.0,
        "silent through the delay"
    );
    assert!(full > 0.1);
    assert!((window(240.0) / full - 1.0).abs() < 0.01, "held at the top");
    // 100 ms delay + 1 ms attack + 150 ms hold, then 200 ms of linear decay.
    let halfway = window(351.0 - 2.5) / full;
    assert!((halfway - 0.5).abs() < 0.05, "{halfway}");
    assert!(window(500.0) < 1e-4, "decayed to silence");
}

#[test]
fn a_mod_envelope_delay_delays_its_modulation() {
    // Env 2 (attack 1 ms, sustain 1) delayed by 200 ms, into A Pitch: the
    // pitch only jumps once the delay is over.
    let mut synth = plain_sine();
    synth.set_param(ENV2.attack, 1.0);
    synth.set_param(ENV2.sustain, 1.0);
    synth.set_param(env_param(1, ENV_DELAY), 200.0);
    synth.set_param(slot_param(0, SLOT_SOURCE), (SRC_ENV1 + 1) as f32);
    synth.set_param(slot_param(0, SLOT_DEST), (DEST_OSC_A + DEST_PITCH) as f32);
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.5);
    synth.reset();
    let (l, _) = render(&mut synth, &[on(57)], 0.5);
    let before = pitch(&l[2400..(0.19 * RATE) as usize]);
    let after = pitch(&l[(0.25 * RATE) as usize..]);
    println!("delayed env 2: {before:.2} Hz, then {after:.2} Hz");
    assert!((before - 220.0).abs() < 0.1 && (after - 440.0).abs() < 0.1);
}

// ---------------------------------------------------------------------------
// Everything at once

#[test]
fn the_new_features_together_stay_finite_and_bounded() {
    let chord: Vec<NoteEvent> = (0..12).map(|i| on(30 + 6 * i)).collect();
    let mut synth = default_synth();
    for osc in 0..2 {
        let p = |offset| osc_param(osc, offset);
        synth.set_param(p(OSC_UNISON), 15.0);
        synth.set_param(p(OSC_STACK), 7.0);
        synth.set_param(p(OSC_WARP_SPREAD), 1.0);
        synth.set_param(p(OSC_WARP_MODE), WARP_FM as f32);
        synth.set_param(p(OSC_WARP), 1.0);
        synth.set_param(p(OSC_WARP2_MODE), WARP_SYNC as f32);
        synth.set_param(p(OSC_WARP2), 1.0);
    }
    synth.set_param(SUB_ON, 1.0);
    synth.set_param(NOISE_ON, 1.0);
    synth.set_param(FILTER2_ON, 1.0);
    synth.set_param(FILTER_RESONANCE, 1.0);
    synth.set_param(FILTER2_RESONANCE, 1.0);
    synth.set_param(FILTER2_DRIVE, 1.0);
    synth.set_param(FILTER_TARGET_OSC_B, 1.0);
    synth.set_param(FILTER_TARGET_NOISE, 1.0);
    for lfo in 0..LFO_COUNT {
        synth.set_param(lfo_param(lfo, LFO_SHAPE), SHAPE_CUSTOM as f32);
        synth.set_param(lfo_param(lfo, LFO_RATE), 50.0);
        synth.set_param(lfo_param(lfo, LFO_GLIDE), (lfo % 2) as f32);
    }
    // Every slot at full amount and bend, aimed at every destination in
    // turn, with the polarities mixed.
    let mut loudest = 0.0f32;
    for pass in 0..3 {
        for slot in 0..MOD_SLOTS {
            let dest = (pass * MOD_SLOTS + slot) % (DESTINATIONS.len() - 1) + 1;
            let source = slot % (SOURCES.len() - 1) + 1;
            let p = |offset| slot_param(slot, offset);
            synth.set_param(p(SLOT_SOURCE), source as f32);
            synth.set_param(p(SLOT_DEST), dest as f32);
            synth.set_param(p(SLOT_AMOUNT), if slot % 2 == 0 { 1.0 } else { -1.0 });
            synth.set_param(p(SLOT_CURVE), if slot % 3 == 0 { 1.0 } else { -1.0 });
            synth.set_param(p(SLOT_POLARITY), (slot % 3) as f32);
        }
        for routing in [0.0, 1.0] {
            synth.set_param(FILTER_ROUTING, routing);
            let (l, r) = render(&mut synth, &chord, 0.5);
            loudest = loudest.max(peak(&l)).max(peak(&r));
        }
    }
    // And every new parameter at its extremes, one at a time.
    let range = |kind: ParameterKind| match kind {
        ParameterKind::Float { min, max, .. } => [min, max],
        ParameterKind::Enum { labels, .. } => [0.0, (labels.len() - 1) as f32],
    };
    let new_ones = (OSC_WARP2_MODE..OSC_PARAMS)
        .flat_map(|offset| [osc_param(0, offset), osc_param(1, offset)])
        .chain(FILTER2)
        .chain(env_param(0, 0)..ENVELOPES.end)
        .chain(
            (0..MOD_SLOTS).flat_map(|s| [slot_param(s, SLOT_CURVE), slot_param(s, SLOT_POLARITY)]),
        );
    for index in new_ones {
        let before = PARAMS[index].default_value();
        for value in range(PARAMS[index].kind) {
            synth.set_param(index, value);
            let (l, r) = render(&mut synth, &chord, 0.03);
            loudest = loudest.max(peak(&l)).max(peak(&r));
        }
        synth.set_param(index, before);
    }
    println!("loudest with everything new at once: {loudest:.2}");
    // A slot on the Volume destination turns the volume up as far as the
    // knob goes (+6 dB, from the default's -3 dB), as turning it would: a
    // plain gain on top of everything else, so the bound grows with it.
    let headroom_db = VOLUME_MAX_DB - PARAMS[MASTER_VOLUME].default_value();
    let bound = 20.0 * 10f32.powf(headroom_db / 20.0);
    assert!(loudest < bound, "peaked at {loudest} (bound {bound})");
}

#[test]
fn the_ui_helpers_cover_the_new_parts() {
    // Both filters draw, and a filter that is off is flat.
    let values = values_of(&[
        (FILTER2_ON, 1.0),
        (FILTER2_TYPE, HP24 as f32),
        (FILTER2_CUTOFF, 1000.0),
    ]);
    assert!(filter_curve_db(1, &values, 100.0, RATE) < -30.0);
    assert!(filter_curve_db(1, &values, 10_000.0, RATE).abs() < 0.5);
    let off = values_of(&[]);
    assert_eq!(filter_curve_db(1, &off, 100.0, RATE), 0.0);
    assert_eq!(
        filter_curve_db(0, &off, 500.0, RATE),
        filter_response_db(&off, 500.0, RATE)
    );
    // The LFO helper draws fixed shapes as before and custom ones from their points.
    assert_eq!(lfo_shape_at(&off, 0, 0.25), lfo_value(SHAPE_SINE, 0.25));
    let mut custom = off.clone();
    custom[lfo_param(1, LFO_SHAPE)] = SHAPE_CUSTOM as f32;
    custom[lfo_param(1, LFO_GLIDE)] = 0.0;
    custom[lfo_param(1, LFO_POINT + 3)] = 1.0;
    let at = |i: f32| lfo_shape_at(&custom, 1, (i + 0.5) / LFO_POINTS as f32);
    assert_eq!(at(3.0), 1.0);
    assert!(
        (at(4.0) - (2.0 * PARAMS[lfo_param(1, LFO_POINT + 4)].default_value() - 1.0)).abs() < 1e-6
    );
    // Ids of the new parameters are the ones projects will store.
    assert_eq!(PARAMS[FILTER2_CUTOFF].id, "filter2_cutoff");
    assert_eq!(PARAMS[FILTER_TARGET_NOISE].id, "filter_noise_target");
    assert_eq!(PARAMS[osc_param(1, OSC_WARP2)].id, "osc_b_warp2");
    assert_eq!(PARAMS[osc_param(0, OSC_STACK)].id, "osc_a_stack");
    assert_eq!(PARAMS[env_param(2, ENV_HOLD)].id, "env3_hold");
    assert_eq!(PARAMS[lfo_param(3, LFO_POINT + 31)].id, "lfo4_p31");
    assert_eq!(PARAMS[slot_param(15, SLOT_CURVE)].id, "mod16_curve");
    assert_eq!(DESTINATIONS[DEST_CUTOFF2], "Cutoff 2");
    assert_eq!(DESTINATIONS[DEST_WARP2_A + 1], "B Warp 2");
}

// ---------------------------------------------------------------------------
// Instrument-wide modulators and outside modulation

#[test]
fn mod_outputs_follow_the_meters_and_the_instrument_wide_values() {
    let mut synth = plain_sine();
    // LFO 1 runs per voice; LFO 3 is mono; env 2 is mono; velocity moves
    // macro 2 through the matrix.
    synth.set_param(lfo_param(0, LFO_RATE), 3.0);
    synth.set_param(lfo_param(2, LFO_MONO), 1.0);
    synth.set_param(lfo_param(2, LFO_SHAPE), SHAPE_SINE as f32);
    synth.set_param(lfo_param(2, LFO_SYNC), 0.0);
    synth.set_param(lfo_param(2, LFO_RATE), 1.7);
    synth.set_param(env_mono_param(1), 1.0);
    synth.set_param(macro_param(0), 0.4);
    synth.set_param(slot_param(0, SLOT_SOURCE), SRC_MOD_WHEEL as f32);
    synth.set_param(slot_param(0, SLOT_DEST), (DEST_MACRO1 + 1) as f32);
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.5);
    synth.reset();
    let wheel = NoteEvent {
        frame: 0,
        kind: NoteEventKind::Control {
            controller: midi::MOD_WHEEL,
            value: 127,
        },
    };
    let (mut left, mut right) = ([0.0; 64], [0.0; 64]);
    synth.process(&[wheel, on(60)], &mut left, &mut right);
    let mut outputs = vec![0.0; MOD_OUTPUTS.len()];
    let mut meters = [0.0; MAX_METERS];
    let index = |id: &str| MOD_OUTPUTS.iter().position(|o| o.id == id).unwrap();
    for _ in 0..300 {
        synth.process(&[], &mut left, &mut right);
        synth.meters(&mut meters);
        synth.mod_outputs(&mut outputs);
        for lfo in 0..LFO_COUNT {
            let id = format!("lfo{}", lfo + 1);
            assert_eq!(outputs[index(&id)], meters[METER_LFO_VALUE + lfo], "{id}");
        }
        for env in 0..ENV_COUNT {
            let id = format!("env{}", env + 1);
            assert_eq!(outputs[index(&id)], meters[METER_ENV + env], "{id}");
        }
        assert_eq!(outputs[index("velocity")], meters[METER_VELOCITY]);
        assert_eq!(outputs[index("note")], meters[METER_NOTE]);
        assert_eq!(outputs[index("mod_wheel")], meters[METER_MOD_WHEEL]);
        for (output, value) in MOD_OUTPUTS.iter().zip(&outputs) {
            let range = if output.bipolar {
                -1.0..=1.0
            } else {
                0.0..=1.0
            };
            assert!(range.contains(value), "{}: {value}", output.id);
        }
    }
    // The instrument-wide ones are one value for everyone.
    assert_eq!(
        synth.global_source(lfo_source(2)),
        Some(outputs[index("lfo3")])
    );
    assert_eq!(
        synth.global_source(env_source(1)),
        Some(outputs[index("env2")])
    );
    assert_eq!(
        synth.global_source(lfo_source(0)),
        None,
        "LFO 1 is per voice"
    );
    // Macro 2 is moved by the wheel, for every voice: half its range.
    assert!((outputs[index("macro2")] - 0.5).abs() < 1e-4);
    assert_eq!(outputs[index("macro1")], 0.4);
    // With the note let go and silent, the mono LFO still runs.
    synth.process(&[off(60)], &mut left, &mut right);
    for _ in 0..1500 {
        synth.process(&[], &mut left, &mut right);
    }
    let mut seen = Vec::new();
    for _ in 0..200 {
        synth.process(&[], &mut left, &mut right);
        synth.mod_outputs(&mut outputs);
        seen.push(outputs[index("lfo3")]);
    }
    assert!(
        seen.iter().any(|v| (v - seen[0]).abs() > 0.1),
        "the mono LFO keeps moving"
    );
}

#[test]
fn modulate_lands_within_a_block_without_clicks() {
    let mut synth = plain_sine();
    let level = osc_param(0, OSC_LEVEL);
    synth.set_param(level, 1.0);
    synth.reset();
    let (mut left, mut right) = ([0.0; 64], [0.0; 64]);
    synth.process(&[on(69)], &mut left, &mut right);
    for _ in 0..40 {
        synth.process(&[], &mut left, &mut right);
    }
    let loud = peak(&left);
    // A modulator drops the level: one engine block (64 frames) later it
    // is there, not 20 ms later as a knob would be.
    synth.modulate(level, 0.25);
    synth.process(&[], &mut left, &mut right);
    synth.process(&[], &mut left, &mut right);
    let quiet = peak(&left);
    println!("level {loud:.3} -> {quiet:.3}");
    assert!((quiet / loud - 0.25).abs() < 0.02, "{loud} -> {quiet}");
    // A knob move afterwards ramps as knobs do again.
    synth.set_param(level, 1.0);
    synth.process(&[], &mut left, &mut right);
    synth.process(&[], &mut left, &mut right);
    assert!(peak(&left) < 0.6 * loud, "{}", peak(&left));
    for _ in 0..40 {
        synth.process(&[], &mut left, &mut right);
    }

    // Moved every block by a 20 Hz sine, the output has no steps bigger
    // than the note's own: no clicks.
    let mut out = Vec::new();
    for block in 0..400 {
        let t = block as f32 * 64.0 / RATE;
        synth.modulate(level, 0.5 + 0.5 * (std::f32::consts::TAU * 20.0 * t).sin());
        // Between the sine and the triangle, which have no jumps of their own.
        let position = 0.15 + 0.15 * (std::f32::consts::TAU * 7.0 * t).sin();
        synth.modulate(osc_param(0, OSC_POSITION), position);
        synth.process(&[], &mut left, &mut right);
        out.extend_from_slice(&left);
    }
    let largest = out
        .windows(2)
        .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    // A full-scale 440 Hz sine moves at most 2π·440/48000·peak per sample.
    let natural = std::f32::consts::TAU * 440.0 / RATE * loud;
    println!("largest step {largest:.4}, the sine's own up to {natural:.4}");
    assert!(largest < natural * 2.5, "{largest} vs {natural}");
}

// ---------------------------------------------------------------------------
// Envelope tempo sync and legato retrigger

/// Seconds the amp envelope takes to reach 90% of the note's peak.
fn rise_time(synth: &mut Wavetable) -> f32 {
    let (l, _) = render(synth, &[on(69)], 1.5);
    let full = peak(&l[(1.2 * RATE) as usize..]);
    let window = 48;
    let first = l
        .chunks(window)
        .position(|w| peak(w) >= 0.9 * full)
        .expect("reaches its peak");
    (first * window) as f32 / RATE
}

#[test]
fn synced_envelopes_follow_the_tempo() {
    let mut synth = plain_sine();
    synth.set_param(ENV1.attack, 500.0);
    synth.set_param(ENV1.curves.unwrap()[0], 0.0);
    synth.set_param(env_sync_param(0), 1.0);
    synth.set_tempo(120.0);
    synth.reset();
    let at_120 = rise_time(&mut synth);
    synth.set_tempo(240.0);
    synth.reset();
    let at_240 = rise_time(&mut synth);
    println!("90% after {at_120:.3} s at 120 BPM, {at_240:.3} s at 240");
    assert!((at_120 - 0.45).abs() < 0.02, "{at_120}");
    assert!((at_240 / at_120 - 0.5).abs() < 0.05, "{at_240}");
    // Unsynced, the tempo changes nothing.
    synth.set_param(env_sync_param(0), 0.0);
    synth.reset();
    assert!((rise_time(&mut synth) - at_120).abs() < 0.002);
}

#[test]
fn a_retriggering_envelope_restarts_under_legato() {
    let env2_after_slide = |retrigger: bool| {
        let mut synth = plain_sine();
        synth.set_param(VOICE_MODE, 2.0); // Legato
        synth.set_param(env_retrigger_param(1), if retrigger { 1.0 } else { 0.0 });
        synth.reset();
        render(&mut synth, &[on(60)], 1.0);
        let mut meters = [0.0; MAX_METERS];
        synth.meters(&mut meters);
        let settled = meters[METER_ENV + 1];
        // A second key while the first is held: legato.
        render(&mut synth, &[on(64)], 0.01);
        synth.meters(&mut meters);
        (settled, meters[METER_ENV + 1], meters[METER_ENV])
    };
    let (settled, carried, amp) = env2_after_slide(false);
    assert!((carried - settled).abs() < 0.01, "{settled} -> {carried}");
    let (settled, restarted, amp_retriggered) = env2_after_slide(true);
    println!("env 2 at {settled:.2}: {carried:.2} carried on, {restarted:.2} restarted");
    assert!(restarted > settled + 0.3, "{settled} -> {restarted}");
    assert!(
        (amp - amp_retriggered).abs() < 0.01,
        "the amp envelope carries on"
    );
}

// ---------------------------------------------------------------------------
// Modulating (nearly) every knob

/// A knob turned: its parameter, from, to.
type Turn = (usize, f32, f32);
/// A slot: its source, destination and amount.
type SlotRoute = (usize, usize, f32);
/// A named setup, the knob turned on top of it and the slot that should
/// sound the same.
type Case<'a> = (&'a str, &'a [(usize, f32)], Turn, SlotRoute);

/// Renders `seconds` of a note on `synth` with `changes` set first, both
/// sides summed; the key is let go after `held` seconds, if given.
fn note_with(changes: &[(usize, f32)], seconds: f32, held: Option<f32>) -> Vec<f32> {
    let mut synth = default_synth();
    for slot in 0..MOD_SLOTS {
        synth.set_param(slot_param(slot, SLOT_SOURCE), 0.0);
    }
    for (index, value) in changes {
        synth.set_param(*index, *value);
    }
    synth.reset();
    let (mut l, mut r) = match held {
        Some(held) => {
            let (mut l, mut r) = render(&mut synth, &[on(57)], held);
            let (rest_l, rest_r) = render(&mut synth, &[off(57)], seconds - held);
            l.extend(rest_l);
            r.extend(rest_r);
            (l, r)
        }
        None => render(&mut synth, &[on(57)], seconds),
    };
    for (l, r) in l.iter_mut().zip(r.drain(..)) {
        *l += r;
    }
    l
}

/// How far a slot from `source` at `amount` on `dest` sounds from turning
/// knob `index` from `base` to `turned`, on top of `setup`, for a note
/// let go after `held` seconds if given: the RMS of the difference against
/// the RMS of the sound.
fn slot_against_knob(
    setup: &[(usize, f32)],
    (index, base, turned): Turn,
    (source, dest, amount): SlotRoute,
    held: Option<f32>,
) -> f32 {
    let mut knob = setup.to_vec();
    knob.push((index, turned));
    let mut slot = setup.to_vec();
    slot.extend([
        (index, base),
        (slot_param(0, SLOT_SOURCE), source as f32),
        (slot_param(0, SLOT_DEST), dest as f32),
        (slot_param(0, SLOT_AMOUNT), amount),
    ]);
    let (turned, moved) = (note_with(&knob, 1.2, held), note_with(&slot, 1.2, held));
    let difference: Vec<f32> = turned.iter().zip(&moved).map(|(a, b)| a - b).collect();
    rms(&difference) / rms(&turned).max(1e-9)
}

#[test]
fn a_slot_on_a_new_destination_sounds_like_turning_its_knob() {
    let a = |offset| osc_param(0, offset);
    let unison = [
        (a(OSC_UNISON), 6.0),
        (a(OSC_DETUNE), 20.0),
        (a(OSC_WARP_MODE), WARP_BEND_POS as f32),
        (a(OSC_WARP), 0.4),
        (a(OSC_POSITION), 0.3),
    ];
    let lfo_on_cutoff = [
        (lfo_param(0, LFO_SHAPE), SHAPE_SQUARE as f32),
        (lfo_param(0, LFO_RATE), 4.0),
        (slot_param(1, SLOT_SOURCE), lfo_source(0) as f32),
        (slot_param(1, SLOT_DEST), DEST_CUTOFF as f32),
        (slot_param(1, SLOT_AMOUNT), 0.3),
    ];
    let enveloped = [(ENV1.decay, 300.0), (ENV1.sustain, 0.3), (ENV1.attack, 0.5)];
    let keyed = [(FILTER_TYPE, LP24 as f32), (FILTER_CUTOFF, 150.0)];
    let fixed = SRC_FIXED;
    let cases: [Case; 11] = [
        (
            "warp spread",
            &unison,
            (a(OSC_WARP_SPREAD), 0.0, 0.5),
            (fixed, DEST_WARP_SPREAD_A, 0.5),
        ),
        (
            "position spread",
            &unison,
            (a(OSC_POSITION_SPREAD), 0.1, 0.7),
            (fixed, DEST_POSITION_SPREAD_A, 0.6),
        ),
        (
            "key track",
            &keyed,
            (FILTER_KEYTRACK, 0.0, 0.75),
            (fixed, DEST_KEYTRACK, 0.75),
        ),
        (
            "sustain",
            &enveloped,
            (ENV1.sustain, 0.3, 0.8),
            (fixed, env_dest(0, ENV_DEST_SUSTAIN), 0.5),
        ),
        (
            // Velocity is known before the note sounds: the whole attack
            // is at its modulated time.
            "attack",
            &enveloped,
            (ENV1.attack, 100.0, 200.0),
            (SRC_VELOCITY, env_dest(0, ENV_DEST_ATTACK), 1.0 / 14.287_712),
        ),
        (
            "decay",
            &enveloped,
            (ENV1.decay, 300.0, 75.0),
            (fixed, env_dest(0, ENV_DEST_DECAY), -2.0 / 13.287_712),
        ),
        (
            "LFO smooth",
            &lfo_on_cutoff,
            (lfo_param(0, LFO_SMOOTH), 0.0, 0.6),
            (fixed, lfo_dest(0, LFO_DEST_SMOOTH), 0.6),
        ),
        (
            "LFO phase",
            &lfo_on_cutoff,
            (lfo_param(0, LFO_PHASE), 0.0, 0.25),
            (fixed, lfo_dest(0, LFO_DEST_PHASE), 0.25),
        ),
        (
            // Rise and delay move in seconds, ten at amount 1.
            "LFO rise",
            &lfo_on_cutoff,
            (lfo_param(0, LFO_RISE), 0.0, 0.6),
            (fixed, lfo_dest(0, LFO_DEST_RISE), 0.06),
        ),
        (
            "LFO delay",
            &lfo_on_cutoff,
            (lfo_param(0, LFO_DELAY), 0.0, 0.4),
            (fixed, lfo_dest(0, LFO_DEST_DELAY), 0.04),
        ),
        (
            "volume",
            &[],
            (MASTER_VOLUME, -3.0, -15.0),
            (fixed, DEST_VOLUME, -12.0 / 66.0),
        ),
    ];
    for (name, setup, knob, slot) in cases {
        let off = slot_against_knob(setup, knob, slot, None);
        // Not moving it at all would be far off.
        let unmoved = slot_against_knob(setup, knob, (fixed, slot.1, 0.0), None);
        println!(
            "{name:16} slot vs knob: {:5.2}% off (unmoved {:5.1}%)",
            off * 100.0,
            unmoved * 100.0
        );
        assert!(off < 0.03, "{name}: {off}");
        assert!(
            unmoved > 0.04,
            "{name}: the knob changes the sound ({unmoved})"
        );
    }
}

#[test]
fn a_slot_on_the_release_sounds_like_turning_its_knob() {
    // Two octaves longer: 100 ms of release becomes 400.
    let setup = [(ENV1.sustain, 0.6), (ENV1.release, 100.0)];
    let knob = (ENV1.release, 100.0, 400.0);
    let route = (SRC_FIXED, env_dest(0, ENV_DEST_RELEASE), 2.0 / 14.287_712);
    let off = slot_against_knob(&setup, knob, route, Some(0.4));
    let unmoved = slot_against_knob(&setup, knob, (SRC_FIXED, route.1, 0.0), Some(0.4));
    println!(
        "release slot vs knob: {:.2}% off (unmoved {:.1}%)",
        off * 100.0,
        unmoved * 100.0
    );
    assert!(off < 0.03, "{off}");
    assert!(unmoved > 0.1, "{unmoved}");
}

#[test]
fn a_modulated_sustain_moves_the_level_without_clicks() {
    let mut synth = plain_sine();
    synth.set_param(ENV1.decay, 50.0);
    synth.set_param(ENV1.sustain, 0.5);
    synth.set_param(lfo_param(0, LFO_SHAPE), SHAPE_SQUARE as f32);
    synth.set_param(lfo_param(0, LFO_RATE), 3.0);
    synth.set_param(slot_param(0, SLOT_SOURCE), lfo_source(0) as f32);
    synth.set_param(
        slot_param(0, SLOT_DEST),
        env_dest(0, ENV_DEST_SUSTAIN) as f32,
    );
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.4);
    synth.reset();
    let (l, _) = render(&mut synth, &[on(69)], 1.0);
    let held = &l[(0.2 * RATE) as usize..];
    let levels: Vec<f32> = held.chunks(480).map(peak).collect();
    let (low, high) = levels
        .iter()
        .fold((f32::MAX, 0.0f32), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    println!("sustain swings {low:.3}..{high:.3}");
    assert!(
        high > 2.5 * low,
        "the square moves the level: {low}..{high}"
    );
    // A square on the sustain glides: no step bigger than the sine's own.
    let largest = held
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    let sine_step = high * (2.0 * std::f32::consts::PI * 440.0 / RATE);
    assert!(largest < 1.2 * sine_step, "{largest} vs {sine_step}");
}

#[test]
fn lfo_phase_modulation_moves_the_playhead_too() {
    let mut synth = plain_sine();
    let p = |offset| lfo_param(2, offset);
    synth.set_param(p(LFO_SHAPE), SHAPE_SINE as f32);
    synth.set_param(p(LFO_MODE), MODE_RETRIG as f32);
    synth.set_param(p(LFO_SYNC), 0.0);
    synth.set_param(p(LFO_RATE), 0.01);
    synth.set_param(macro_param(0), 1.0);
    synth.set_param(slot_param(0, SLOT_SOURCE), macro_source(0) as f32);
    synth.set_param(slot_param(0, SLOT_DEST), lfo_dest(2, LFO_DEST_PHASE) as f32);
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.25);
    synth.set_param(slot_param(1, SLOT_SOURCE), lfo_source(2) as f32);
    synth.set_param(slot_param(1, SLOT_DEST), (osc_dest(0) + DEST_LEVEL) as f32);
    synth.reset();
    render(&mut synth, &[on(69)], 0.1);
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    println!(
        "LFO 3 at {:.3}, phase {:.3}",
        meters[METER_LFO_VALUE + 2],
        meters[METER_LFO_PHASE + 2]
    );
    assert!((meters[METER_LFO_VALUE + 2] - 1.0).abs() < 0.01);
    assert!((meters[METER_LFO_PHASE + 2] - 0.25).abs() < 0.01);
}
