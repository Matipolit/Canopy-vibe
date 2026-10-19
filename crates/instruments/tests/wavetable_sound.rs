//! Measures what the wavetable synth plays: tuning, unison spread,
//! aliasing, the filter, modulation, glide and mono, tempo sync, and that
//! nothing blows up at extreme settings.

mod common;

use common::wavetable::{default_synth, plain_sine, render};
use common::{alias_db, magnitude, max_step, off, on, peak, pitch, rms};
use instruments::builtin::wavetable::{self, *};
use instruments::{Instrument, MAX_METERS, ParameterKind};
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind};

const RATE: f32 = 48_000.0;

fn event(frame: u32, kind: NoteEventKind) -> NoteEvent {
    NoteEvent { frame, kind }
}

#[test]
fn silent_without_notes_and_after_release() {
    let mut synth = default_synth();
    let (l, r) = render(&mut synth, &[], 0.2);
    assert_eq!(peak(&l).max(peak(&r)), 0.0);
    render(&mut synth, &[on(60)], 0.3);
    render(&mut synth, &[off(60)], 1.0);
    let (l, r) = render(&mut synth, &[], 0.1);
    assert_eq!(peak(&l).max(peak(&r)), 0.0, "silent once released");
}

#[test]
fn a4_is_440_hz() {
    let mut synth = plain_sine();
    let (l, _) = render(&mut synth, &[on(69)], 1.0);
    let hz = pitch(&l[4800..]);
    assert!((hz - 440.0).abs() < 0.05, "{hz}");
    println!("A4 measured at {hz:.4} Hz");
}

#[test]
fn octave_semitone_and_fine_transpose() {
    let mut synth = plain_sine();
    synth.set_param(osc_param(0, OSC_OCTAVE), 5.0); // +1
    synth.set_param(osc_param(0, OSC_SEMI), 14.0); // +2
    synth.set_param(osc_param(0, OSC_FINE), 50.0);
    let (l, _) = render(&mut synth, &[on(57)], 1.0);
    let expected = 220.0 * 2f32.powf((12.0 + 2.0 + 0.5) / 12.0);
    let hz = pitch(&l[4800..]);
    assert!((hz - expected).abs() < 0.05, "{hz} vs {expected}");
}

#[test]
fn unison_detune_spreads_voices_as_set() {
    for (voices, detune) in [(2usize, 50.0f32), (3, 30.0)] {
        let mut synth = plain_sine();
        synth.set_param(osc_param(0, OSC_UNISON), (voices - 1) as f32);
        synth.set_param(osc_param(0, OSC_DETUNE), detune);
        synth.set_param(osc_param(0, OSC_WIDTH), 0.0);
        synth.set_param(osc_param(0, OSC_BLEND), 1.0);
        let (l, _) = render(&mut synth, &[on(69)], 2.2);
        let tail = &l[(0.2 * RATE) as usize..(2.2 * RATE) as usize];
        // Scan 400..480 Hz in 0.1 Hz steps for the strongest peaks.
        let scan: Vec<(f64, f64)> = (0..800)
            .map(|i| {
                let hz = 400.0 + i as f64 * 0.1;
                (hz, magnitude(tail, hz))
            })
            .collect();
        let mut peaks: Vec<(f64, f64)> = scan
            .windows(3)
            .filter(|w| w[1].1 > w[0].1 && w[1].1 >= w[2].1)
            .map(|w| w[1])
            .collect();
        peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut found: Vec<f64> = peaks.iter().take(voices).map(|p| p.0).collect();
        found.sort_by(f64::total_cmp);
        for (u, hz) in found.iter().enumerate() {
            let place = if voices == 1 {
                0.0
            } else {
                2.0 * u as f64 / (voices - 1) as f64 - 1.0
            };
            let expected = 440.0 * 2f64.powf(detune as f64 * place / 1200.0);
            assert!(
                (hz - expected).abs() < 0.3,
                "{voices} voices at ±{detune} cents: voice {u} at {hz}, expected {expected}"
            );
        }
        println!("{voices} voices ±{detune} cents: {found:.2?} Hz");
    }
}

#[test]
fn band_limiting_keeps_a_5_khz_saw_clean() {
    let mut synth = plain_sine();
    synth.set_param(osc_param(0, OSC_POSITION), 2.0 / 3.0); // saw
    // Note 111 is 4978 Hz: harmonics at 5, 10, 15 and 20 kHz.
    let (l, _) = render(&mut synth, &[on(111)], 1.5);
    let f0 = 440.0 * 2f64.powf(42.0 / 12.0);
    let alias = alias_db(&l[9600..], f0);
    println!("5 kHz saw: non-harmonic energy {alias:.1} dB below the harmonics");
    assert!(alias < -70.0, "{alias} dB");

    // For comparison, the same saw with no band limit: it would alias badly.
    let naive: Vec<f32> = (0..65536)
        .map(|i| {
            let p = (i as f64 * f0 / RATE as f64).fract();
            (if p < 0.5 { 2.0 * p } else { 2.0 * p - 2.0 }) as f32
        })
        .collect();
    let naive_alias = alias_db(&naive, f0);
    println!("naive saw: {naive_alias:.1} dB");
    assert!(naive_alias > alias + 40.0);
}

#[test]
fn band_limiting_holds_across_tables_and_warps() {
    // Every table, and the smooth phase warps, at a high note. (Sync, PWM,
    // Flip and Quantize add hard edges on purpose and alias some, as they
    // do in other synths.)
    let f0 = 440.0 * 2f64.powf(30.0 / 12.0); // note 99, 2489 Hz
    for (mode, amount, limit) in [
        (WARP_OFF, 0.0, -65.0),
        (WARP_BEND_POS, 1.0, -40.0),
        (WARP_BEND_NEG, 0.6, -40.0),
        (WARP_ASYM_POS, 1.0, -40.0),
    ] {
        let mut worst = (f64::MIN, "");
        for table in 0..tables().len() {
            let mut synth = plain_sine();
            let a = |offset| osc_param(0, offset);
            synth.set_param(a(OSC_TABLE), table as f32);
            synth.set_param(a(OSC_POSITION), 0.6);
            synth.set_param(a(OSC_WARP_MODE), mode as f32);
            synth.set_param(a(OSC_WARP), amount);
            let (l, _) = render(&mut synth, &[on(99)], 1.0);
            let alias = alias_db(&l[9600..], f0);
            if std::env::var("WAVETABLE_ALIAS_TABLE").is_ok() {
                println!(
                    "{:>12} warp {mode}: {alias:6.1} dB, rms {:.4}",
                    tables()[table],
                    rms(&l[9600..])
                );
            }
            if alias > worst.0 {
                worst = (alias, tables()[table]);
            }
        }
        println!(
            "2.5 kHz, {} {amount}: worst aliasing {:.1} dB ({})",
            WARPS_A[mode], worst.0, worst.1
        );
        assert!(worst.0 < limit, "{} {amount}: {worst:?}", WARPS_A[mode]);
    }
}

/// A 110 Hz saw through the filter, with the matrix and key tracking off.
fn filtered_saw(kind: usize, cutoff: f32, resonance: f32) -> Wavetable {
    let mut synth = plain_sine();
    synth.set_param(osc_param(0, OSC_POSITION), 2.0 / 3.0);
    synth.set_param(FILTER_ON, 1.0);
    synth.set_param(FILTER_TYPE, kind as f32);
    synth.set_param(FILTER_CUTOFF, cutoff);
    synth.set_param(FILTER_RESONANCE, resonance);
    synth.set_param(FILTER_DRIVE, 0.0);
    synth.set_param(FILTER_KEYTRACK, 0.0);
    synth.reset();
    synth
}

#[test]
fn the_cutoff_cuts_above_and_passes_below() {
    let mut open = filtered_saw(LP24, 20_000.0, 0.0);
    let (open, _) = render(&mut open, &[on(45)], 1.0);
    let mut closed = filtered_saw(LP24, 500.0, 0.0);
    let (closed, _) = render(&mut closed, &[on(45)], 1.0);
    let tail = |s: &[f32]| s[9600..].to_vec();
    let (open, closed) = (tail(&open), tail(&closed));
    let db = |hz| 20.0 * (magnitude(&closed, hz) / magnitude(&open, hz)).log10();
    let (pass, stop) = (db(220.0), db(2200.0));
    println!("LP24 at 500 Hz: 220 Hz {pass:.1} dB, 2.2 kHz {stop:.1} dB");
    assert!(pass.abs() < 1.5, "{pass}");
    assert!(stop < -45.0, "{stop}");

    // The UI's drawn curve agrees with what plays.
    let mut values: Vec<f32> = PARAMS.iter().map(|p| p.default_value()).collect();
    values[FILTER_TYPE] = LP24 as f32;
    values[FILTER_CUTOFF] = 500.0;
    values[FILTER_RESONANCE] = 0.0;
    let drawn = filter_response_db(&values, 2200.0, RATE);
    assert!((drawn - stop as f32).abs() < 1.5, "{drawn} vs {stop}");
    assert!((filter_response_db(&values, 500.0, RATE) + 3.0).abs() < 0.2);
}

#[test]
fn high_pass_and_band_pass_shape_the_spectrum() {
    let mut reference = filtered_saw(LP12, 20_000.0, 0.0);
    reference.set_param(FILTER_ON, 0.0);
    let (reference, _) = render(&mut reference, &[on(45)], 1.0);
    let reference = &reference[9600..];
    for (kind, low_db, high_db) in [(HP24, -40.0f64, -1.0f64), (BP12, -15.0, -15.0)] {
        let mut synth = filtered_saw(kind, 1000.0, 0.0);
        let (out, _) = render(&mut synth, &[on(45)], 1.0);
        let out = &out[9600..];
        let db = |hz| 20.0 * (magnitude(out, hz) / magnitude(reference, hz)).log10();
        let (low, mid, high) = (db(110.0), db(990.0), db(8800.0));
        println!("type {kind}: 110 Hz {low:.1} dB, 990 Hz {mid:.1} dB, 8.8 kHz {high:.1} dB");
        assert!(low < low_db, "type {kind} low {low}");
        let shaped = if kind == HP24 {
            high > high_db
        } else {
            high < high_db
        };
        assert!(shaped, "type {kind} high {high}");
        assert!(mid > -4.0, "type {kind} mid {mid}");
    }
}

#[test]
fn resonance_and_fast_modulation_stay_stable_in_every_filter() {
    for (kind, name) in FILTER_TYPES.iter().enumerate() {
        let mut synth = filtered_saw(kind, 300.0, 1.0);
        synth.set_param(FILTER_DRIVE, 1.0);
        synth.set_param(osc_param(0, OSC_UNISON), 7.0);
        // LFO 1 at 40 Hz sweeps the cutoff over ±8 octaves.
        synth.set_param(lfo_param(0, LFO_RATE), 40.0);
        synth.set_param(lfo_param(0, LFO_SHAPE), SHAPE_SQUARE as f32);
        synth.set_param(slot_param(0, SLOT_SOURCE), 4.0);
        synth.set_param(slot_param(0, SLOT_DEST), 20.0);
        synth.set_param(slot_param(0, SLOT_AMOUNT), 0.8);
        let (l, r) = render(&mut synth, &[on(36), on(60), on(84)], 2.0);
        let loudest = peak(&l).max(peak(&r));
        assert!(loudest < 8.0, "{name}: peak {loudest}");
        assert!(rms(&l) > 1e-4, "{name} went silent");
    }
}

/// Brightness (energy of the first difference) over windows of `window` samples.
fn brightness(signal: &[f32], window: usize) -> Vec<f32> {
    signal
        .chunks(window)
        .map(|c| rms(&c.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>()) / rms(c).max(1e-9))
        .collect()
}

#[test]
fn an_lfo_routed_to_the_cutoff_moves_it() {
    let mut synth = filtered_saw(LP24, 600.0, 0.3);
    synth.set_param(lfo_param(0, LFO_SHAPE), SHAPE_SINE as f32);
    synth.set_param(lfo_param(0, LFO_MODE), MODE_RETRIG as f32);
    synth.set_param(lfo_param(0, LFO_RATE), 2.0);
    synth.set_param(slot_param(0, SLOT_SOURCE), 4.0); // LFO 1
    synth.set_param(slot_param(0, SLOT_DEST), 20.0); // Cutoff
    synth.set_param(slot_param(0, SLOT_AMOUNT), 0.25); // ±2.5 octaves
    let (l, _) = render(&mut synth, &[on(45)], 2.0);
    let window = 480; // 10 ms
    let bright = brightness(&l, window);
    // Correlate with the LFO's own shape: retriggered, it starts at phase 0.
    let lfo: Vec<f32> = (0..bright.len())
        .map(|i| lfo_value(SHAPE_SINE, 2.0 * (i as f32 + 0.5) * window as f32 / RATE))
        .collect();
    let skip = 5;
    let (b, s) = (&bright[skip..], &lfo[skip..]);
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    let (mb, ms) = (mean(b), mean(s));
    let cov: f32 = b.iter().zip(s).map(|(x, y)| (x - mb) * (y - ms)).sum();
    let var_b: f32 = b.iter().map(|x| (x - mb).powi(2)).sum();
    let var_s: f32 = s.iter().map(|y| (y - ms).powi(2)).sum();
    let correlation = cov / (var_b * var_s).sqrt();
    let lo = b.iter().fold(f32::MAX, |m, x| m.min(*x));
    let hi = b.iter().fold(0.0f32, |m, x| m.max(*x));
    println!("LFO -> cutoff: brightness {lo:.3}..{hi:.3}, correlation {correlation:.3}");
    assert!(correlation > 0.9, "{correlation}");
    assert!(hi > lo * 2.0);

    // The meters show the slot's live contribution following the LFO.
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    let lfo_now = meters[METER_LFO_VALUE];
    let live = live_modulation(synth.param_values(), &meters);
    assert!((live.slots[0] - 0.25 * lfo_now).abs() < 1e-6);
    assert!((live.destinations[DEST_CUTOFF] - 2.5 * lfo_now).abs() < 1e-5);
    assert_eq!(meters[METER_VOICES], 1.0);
}

#[test]
fn the_mod_wheel_bends_through_the_matrix() {
    let mut synth = plain_sine();
    synth.set_param(slot_param(0, SLOT_SOURCE), 10.0); // Mod Wheel
    synth.set_param(slot_param(0, SLOT_DEST), 5.0); // A Pitch
    synth.set_param(slot_param(0, SLOT_AMOUNT), 1.0 / 24.0); // one semitone
    let (l, _) = render(&mut synth, &[on(69)], 0.5);
    assert!((pitch(&l[4800..]) - 440.0).abs() < 0.05);
    let wheel = event(
        0,
        NoteEventKind::Control {
            controller: MOD_WHEEL,
            value: 127,
        },
    );
    let (l, _) = render(&mut synth, &[wheel], 0.5);
    let up = 440.0 * 2f32.powf(1.0 / 12.0);
    let hz = pitch(&l[4800..]);
    assert!((hz - up).abs() < 0.05, "{hz} vs {up}");

    // The default patch opens the filter with it.
    let mut synth = default_synth();
    let (dark, _) = render(&mut synth, &[on(48)], 0.6);
    let (bright, _) = render(&mut synth, &[wheel], 0.6);
    // Compare a band well above the closed cutoff (about 4 kHz) with the fundamental.
    let air = |s: &[f32]| {
        let s = &s[14400..28800];
        (8000..8800)
            .step_by(130)
            .map(|hz| magnitude(s, hz as f64))
            .sum::<f64>()
            / magnitude(s, 130.81)
    };
    let (dark, bright) = (air(&dark), air(&bright));
    println!(
        "mod wheel: 8 kHz band {:.1} dB -> {:.1} dB",
        20.0 * dark.log10(),
        20.0 * bright.log10()
    );
    assert!(bright > dark * 4.0, "{dark} -> {bright}");
}

#[test]
fn the_aux_source_scales_a_slot_and_rise_fades_an_lfo_in() {
    // LFO 2 (5 Hz triangle, retriggered) adds ±1 semitone of vibrato,
    // scaled by the mod wheel, fading in over 0.4 s.
    let mut synth = plain_sine();
    synth.set_param(lfo_param(1, LFO_SHAPE), SHAPE_TRIANGLE as f32);
    synth.set_param(lfo_param(1, LFO_MODE), MODE_RETRIG as f32);
    synth.set_param(lfo_param(1, LFO_RATE), 5.0);
    synth.set_param(lfo_param(1, LFO_RISE), 0.4);
    synth.set_param(slot_param(0, SLOT_SOURCE), 5.0); // LFO 2
    synth.set_param(slot_param(0, SLOT_DEST), 19.0); // Pitch
    synth.set_param(slot_param(0, SLOT_AMOUNT), 1.0 / 24.0);
    synth.set_param(slot_param(0, SLOT_AUX), 10.0); // Mod Wheel
    let semitones = |hz: f32| 12.0 * (hz / 440.0).log2();
    let deviation = |track: &[f32]| {
        track
            .iter()
            .fold(0.0f32, |m, hz| m.max(semitones(*hz).abs()))
    };

    let (l, _) = render(&mut synth, &[on(69)], 1.0);
    let still = deviation(&pitch_track(&l[4800..48000], 20.0));
    assert!(still < 0.01, "no wheel, no vibrato: {still}");

    let wheel = |value| {
        event(
            0,
            NoteEventKind::Control {
                controller: MOD_WHEEL,
                value,
            },
        )
    };
    synth.reset();
    let (l, _) = render(&mut synth, &[wheel(127), on(69)], 1.0);
    let early = deviation(&pitch_track(&l[..(0.08 * RATE) as usize], 20.0));
    let settled = &l[(0.5 * RATE) as usize..(0.98 * RATE) as usize];
    let late = deviation(&pitch_track(settled, 10.0));
    println!("vibrato: {early:.2} semitones in the first 80 ms, {late:.2} after the rise");
    assert!(early < 0.3, "{early}");
    assert!((0.8..1.1).contains(&late), "{late}");

    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    let lfo = meters[METER_LFO_VALUE + 1];
    let slot = live_modulation(synth.param_values(), &meters).slots[0];
    assert!(
        (slot - lfo / 24.0).abs() < 1e-5,
        "slot = amount × LFO × wheel"
    );
}

#[test]
fn pitch_bend_follows_the_range() {
    let mut synth = plain_sine();
    synth.set_param(BEND_RANGE, 7.0);
    render(&mut synth, &[on(69)], 0.1);
    let bend = event(0, NoteEventKind::PitchBend(-8192));
    let (l, _) = render(&mut synth, &[bend], 0.5);
    let down = 440.0 * 2f32.powf(-7.0 / 12.0);
    let hz = pitch(&l[4800..]);
    assert!((hz - down).abs() < 0.05, "{hz} vs {down}");
}

/// Pitch over consecutive whole windows of `ms`.
fn pitch_track(signal: &[f32], ms: f32) -> Vec<f32> {
    signal
        .chunks_exact((ms / 1000.0 * RATE) as usize)
        .map(pitch)
        .collect()
}

#[test]
fn mono_glides_and_falls_back_to_held_notes() {
    let mut synth = plain_sine();
    synth.set_param(VOICE_MODE, 1.0); // Mono
    synth.set_param(GLIDE, 200.0);
    render(&mut synth, &[on(57)], 0.3);
    let (l, _) = render(&mut synth, &[on(69)], 0.5);
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    assert_eq!(meters[METER_VOICES], 1.0, "one voice in mono");
    let track = pitch_track(&l, 25.0);
    println!("glide 220 -> 440 Hz: {:.0?}", &track[..12]);
    assert!(track[0] < 260.0, "starts near 220: {}", track[0]);
    assert!(track[4] > 280.0 && track[4] < 400.0, "midway: {}", track[4]);
    let arrived = track[10];
    assert!(
        (arrived - 440.0).abs() < 1.0,
        "arrives after 200 ms: {arrived}"
    );
    // Glide is constant-time and linear in pitch: halfway (100 ms) is a
    // tritone up from 220 Hz, about 311 Hz.
    let halfway = pitch(&l[(0.0875 * RATE) as usize..(0.1125 * RATE) as usize]);
    assert!((halfway - 311.1).abs() < 8.0, "{halfway}");

    // Letting go of the top note glides back to the one still held.
    let (l, _) = render(&mut synth, &[off(69)], 0.5);
    let settled = pitch(&l[l.len() - 9600..]);
    assert!((settled - 220.0).abs() < 0.5, "{settled}");
    assert!(rms(&l[l.len() - 4800..]) > 0.05, "still sounding");
    let (l, _) = render(&mut synth, &[off(57)], 2.0);
    assert_eq!(peak(&l[l.len() - 4800..]), 0.0, "released");
}

#[test]
fn poly_glide_starts_from_the_last_note_and_the_first_note_lands_in_tune() {
    let mut synth = plain_sine();
    synth.set_param(GLIDE, 150.0);
    synth.set_param(ENV1.release, 1.0);
    // The first note after a reset has nothing to glide from.
    let (l, _) = render(&mut synth, &[on(57)], 0.3);
    let first = pitch_track(&l, 20.0);
    assert!((first[0] - 220.0).abs() < 0.5, "{first:?}");
    render(&mut synth, &[off(57)], 0.05);
    // The next one slides up from it.
    let (l, _) = render(&mut synth, &[on(69)], 0.4);
    let track = pitch_track(&l, 20.0);
    assert!(track[0] < 260.0, "{track:?}");
    assert!((track[track.len() - 1] - 440.0).abs() < 0.5, "{track:?}");
}

#[test]
fn legato_keeps_the_envelope_and_mono_retriggers_it() {
    for (mode, retriggers) in [(2.0, false), (1.0, true)] {
        let mut synth = plain_sine();
        synth.set_param(VOICE_MODE, mode);
        render(&mut synth, &[on(57)], 0.5);
        let mut meters = [0.0; MAX_METERS];
        synth.meters(&mut meters);
        // Env 2 attacks in 2 ms then decays toward 0.25 over 700 ms.
        let before = meters[METER_ENV + 1];
        render(&mut synth, &[on(60)], 0.01);
        synth.meters(&mut meters);
        let after = meters[METER_ENV + 1];
        if retriggers {
            assert!(after > before + 0.3, "{before} -> {after}");
        } else {
            assert!(after <= before + 0.01, "{before} -> {after}");
        }
        assert_eq!(meters[METER_VOICES], 1.0);
    }
}

#[test]
fn poly_takes_a_voice_per_note_up_to_the_polyphony() {
    let mut synth = default_synth();
    synth.set_param(POLYPHONY, 3.0); // 4 voices
    let chord: Vec<NoteEvent> = (0..6).map(|i| on(48 + 4 * i)).collect();
    render(&mut synth, &chord, 0.1);
    let mut meters = [0.0; MAX_METERS];
    synth.meters(&mut meters);
    assert_eq!(meters[METER_VOICES], 4.0);
}

#[test]
fn tempo_synced_lfos_lock_to_the_tempo() {
    for (bpm, division, beats) in [(120.0, 8.0, 1.0f32), (90.0, 11.0, 0.5), (140.0, 3.0, 4.0)] {
        let mut synth = plain_sine();
        synth.set_tempo(bpm);
        synth.set_param(lfo_param(0, LFO_SYNC), 1.0);
        synth.set_param(lfo_param(0, LFO_DIVISION), division);
        synth.set_param(lfo_param(0, LFO_SHAPE), SHAPE_SAW_UP as f32);
        let expected_hz = bpm as f32 / 60.0 / beats;
        // Follow the phase meter over eight cycles, unwrapping it.
        let seconds = 8.0 / expected_hz;
        let blocks = (seconds * RATE / 64.0) as usize;
        let (mut l, mut r) = ([0.0; 64], [0.0; 64]);
        let mut meters = [0.0; MAX_METERS];
        synth.process(&[on(60)], &mut l, &mut r);
        synth.meters(&mut meters);
        let mut last = meters[METER_LFO_PHASE];
        let mut cycles = 0.0f64;
        for _ in 1..blocks {
            synth.process(&[], &mut l, &mut r);
            synth.meters(&mut meters);
            let phase = meters[METER_LFO_PHASE];
            cycles += (phase - last).rem_euclid(1.0) as f64;
            last = phase;
        }
        let measured = cycles / ((blocks - 1) as f64 * 64.0 / RATE as f64);
        println!(
            "{bpm} bpm, {}: {measured:.4} Hz (expected {expected_hz:.4})",
            DIVISIONS[division as usize]
        );
        assert!(
            (measured / expected_hz as f64 - 1.0).abs() < 1e-3,
            "{measured} vs {expected_hz}"
        );
    }
}

#[test]
fn every_lfo_mode_and_shape_reaches_the_meters() {
    let mut synth = default_synth();
    for shape in 0..LFO_SHAPES.len() {
        for mode in 0..LFO_MODES.len() {
            synth.set_param(lfo_param(1, LFO_SHAPE), shape as f32);
            synth.set_param(lfo_param(1, LFO_MODE), mode as f32);
            synth.set_param(lfo_param(1, LFO_RATE), 7.0);
            // No rise, and a sustain point off the shapes' zero crossings.
            synth.set_param(lfo_param(1, LFO_RISE), 0.0);
            synth.set_param(lfo_param(1, LFO_LOOP), 0.7);
            let mut seen = Vec::new();
            render(&mut synth, &[on(60)], 0.02);
            for _ in 0..40 {
                render(&mut synth, &[], 0.01);
                let mut meters = [0.0; MAX_METERS];
                synth.meters(&mut meters);
                let v = meters[METER_LFO_VALUE + 1];
                assert!((-1.0..=1.0).contains(&v));
                seen.push(v);
            }
            render(&mut synth, &[off(60)], 1.0);
            let moved = seen.iter().any(|v| (v - seen[0]).abs() > 0.1);
            // A one-shot random LFO only gets one draw, which may barely move.
            let one_shot = matches!(mode, MODE_ENV | MODE_SUSTAIN);
            let random = (SHAPE_SAMPLE_HOLD..=SHAPE_SMOOTH_RANDOM).contains(&shape);
            let one_random_draw = one_shot && random;
            assert!(moved || one_random_draw, "shape {shape} mode {mode}");
        }
    }
}

#[test]
fn the_default_patch_plays_at_a_sensible_level() {
    let mut synth = default_synth();
    let (l, r) = render(&mut synth, &[on(60)], 1.0);
    let one = peak(&l).max(peak(&r));
    synth.reset();
    let chord: Vec<NoteEvent> = [48, 55, 60, 64, 67, 72].iter().map(|n| on(*n)).collect();
    let (l, r) = render(&mut synth, &chord, 1.0);
    let six = peak(&l).max(peak(&r));
    println!("default patch: one note peaks at {one:.2}, a six-note chord at {six:.2}");
    assert!((0.15..0.6).contains(&one), "{one}");
    assert!(six < 1.2, "{six}");
    assert_ne!(l, r, "stereo unison");
}

#[test]
fn ui_helpers_draw_sensible_shapes() {
    assert!(tables().len() >= 12);
    assert_eq!(tables().len(), wavetable::TABLE_NAMES.len());
    let mut cycle = [0.0; 256];
    frame(0, 0.0, WARP_OFF, 0.0, &mut cycle);
    let quarter = cycle[64];
    assert!((quarter - 1.0).abs() < 1e-3, "basic shapes start as a sine");
    for table in 0..tables().len() {
        for mode in 0..WARP_COUNT {
            frame(table, 0.37, mode, 0.8, &mut cycle);
            assert!(cycle.iter().all(|s| s.is_finite() && s.abs() <= 1.5));
        }
    }
    frame(99, 0.0, 0, 0.0, &mut cycle);
    assert!(cycle.iter().all(|s| *s == 0.0));
    for (i, (name, range)) in SECTIONS.iter().enumerate() {
        assert!(!name.is_empty());
        if i > 0 {
            let previous = &SECTIONS[i - 1].1;
            assert_eq!(previous.end, range.start, "sections are contiguous");
        }
    }
    assert_eq!(SECTIONS.last().unwrap().1.end, PARAMS.len());
    assert!(PARAMS[ENV1.attack].id.starts_with("env1"));
    assert_eq!(PARAMS[osc_param(1, OSC_PAN)].id, "osc_b_pan");
    assert_eq!(PARAMS[lfo_param(3, LFO_SMOOTH)].id, "lfo4_smooth");
    assert_eq!(PARAMS[slot_param(15, SLOT_AMOUNT)].id, "mod16_amount");
    let mut ids: Vec<&str> = PARAMS.iter().map(|p| p.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), PARAMS.len(), "ids are unique");
    assert_eq!(DESTINATION_SCALES.len(), DESTINATIONS.len());
    assert_eq!(DIVISION_BEATS.len(), DIVISIONS.len());
}

#[test]
fn extreme_parameters_stay_finite_and_bounded() {
    let range = |kind: ParameterKind| match kind {
        ParameterKind::Float { min, max, .. } => [min, max],
        ParameterKind::Enum { labels, .. } => [0.0, (labels.len() - 1) as f32],
    };
    let chord: Vec<NoteEvent> = (0..20).map(|i| on(24 + 4 * i)).collect();
    let mut synth = default_synth();
    render(&mut synth, &chord, 0.01);
    let mut loudest = 0.0f32;
    for (index, param) in PARAMS.iter().enumerate() {
        for value in range(param.kind) {
            synth.set_param(index, value);
            let (l, r) = render(&mut synth, &chord, 0.04);
            loudest = loudest.max(peak(&l)).max(peak(&r));
        }
        synth.set_param(index, param.default_value());
    }
    for extreme in [0, 1] {
        for (index, param) in PARAMS.iter().enumerate() {
            synth.set_param(index, range(param.kind)[extreme]);
        }
        let (l, r) = render(&mut synth, &chord, 0.5);
        loudest = loudest.max(peak(&l)).max(peak(&r));
        let (l, r) = render(&mut synth, &[event(0, NoteEventKind::AllNotesOff)], 0.1);
        loudest = loudest.max(peak(&l)).max(peak(&r));
    }
    // Every slot driving the same destination at full amount, from every source.
    let mut synth = default_synth();
    for slot in 0..MOD_SLOTS {
        let source = slot % (SOURCES.len() - 1) + 1;
        let dest = slot % (DESTINATIONS.len() - 1) + 1;
        let amount = if slot % 2 == 0 { 1.0 } else { -1.0 };
        synth.set_param(slot_param(slot, SLOT_SOURCE), source as f32);
        synth.set_param(slot_param(slot, SLOT_DEST), dest as f32);
        synth.set_param(slot_param(slot, SLOT_AMOUNT), amount);
    }
    synth.set_param(FILTER_RESONANCE, 1.0);
    let (l, r) = render(&mut synth, &chord, 1.0);
    loudest = loudest.max(peak(&l)).max(peak(&r));
    println!("loudest under extreme settings: {loudest:.2}");
    assert!(loudest < 20.0, "peaked at {loudest}");
}

#[test]
fn switches_and_jumps_fade_instead_of_clicking() {
    // Two low sines (A and B) through a gentle low-pass: smooth enough that
    // any click stands out from the wave's own sample-to-sample movement.
    let make = || {
        let mut synth = plain_sine();
        let b = |offset| osc_param(1, offset);
        synth.set_param(b(OSC_ON), 1.0);
        synth.set_param(b(OSC_POSITION), 0.0);
        synth.set_param(b(OSC_UNISON), 0.0);
        synth.set_param(b(OSC_OCTAVE), 5.0);
        synth.set_param(FILTER_ON, 1.0);
        synth.set_param(FILTER_TYPE, LP12 as f32);
        synth.set_param(FILTER_CUTOFF, 400.0);
        synth.set_param(FILTER_RESONANCE, 0.5);
        synth.set_param(FILTER_KEYTRACK, 0.0);
        synth.set_param(FILTER_DRIVE, 0.0);
        synth.reset();
        synth
    };
    let mut synth = make();
    let (steady, _) = render(&mut synth, &[on(45)], 0.5);
    let normal = max_step(&steady[12000..]);
    let changes: [(&str, usize, f32); 9] = [
        ("filter off", FILTER_ON, 0.0),
        ("filter back on", FILTER_ON, 1.0),
        ("cutoff jump", FILTER_CUTOFF, 4000.0),
        ("pan hard left", osc_param(0, OSC_PAN), -1.0),
        ("sub on", SUB_ON, 1.0),
        ("position jump", osc_param(0, OSC_POSITION), 0.3),
        ("sub off", SUB_ON, 0.0),
        ("osc B off", osc_param(1, OSC_ON), 0.0),
        ("osc A level 0", osc_param(0, OSC_LEVEL), 0.0),
    ];
    for (name, index, value) in changes {
        synth.set_param(index, value);
        let (l, r) = render(&mut synth, &[], 0.1);
        let step = max_step(&l).max(max_step(&r));
        let level = rms(&l);
        println!("{name}: largest step {step:.4} (steady {normal:.4}), rms {level:.3}");
        assert!(step < normal * 2.5, "{name}: {step} vs {normal}");
    }
}

#[test]
fn nan_and_unknown_parameters_are_ignored() {
    let mut synth = default_synth();
    for index in 0..PARAMS.len() + 2 {
        synth.set_param(index, f32::NAN);
    }
    synth.set_tempo(f64::NAN);
    let (l, _) = render(&mut synth, &[on(60)], 0.2);
    assert!(peak(&l) > 0.05);
}

#[test]
fn renders_the_same_after_reset() {
    let mut synth = default_synth();
    let (first, _) = render(&mut synth, &[on(60), on(64)], 0.3);
    synth.reset();
    let (second, _) = render(&mut synth, &[on(60), on(64)], 0.3);
    assert_eq!(first, second);
}
