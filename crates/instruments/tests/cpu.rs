//! CPU cost of the instruments, each measured against the Poly Synth in the
//! same run, so the machine's load and power profile affect both alike.
//! Timing means nothing in debug builds, so the comparisons are ignored by
//! default:
//!
//! ```text
//! cargo test --release -p instruments --test cpu -- --ignored --nocapture
//! ```
//!
//! The guitar's budget test runs in every build and checks the busiest case
//! stays well inside real time even unoptimized, which is what a debug
//! session of the app runs.

mod common;

use std::hint::black_box;
use std::time::{Duration, Instant};

use common::{RATE, event, instrument, note_off, note_on, on, timing};
use instruments::Instrument;
use instruments::builtin::wavetable::*;
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind};

const BLOCK: usize = 256;
/// Seconds of audio each comparison plays.
const SECONDS: usize = 20;
/// Seconds the wavetable synth's chord is held for.
const HELD_SECONDS: usize = 10;
/// Each case runs this many times, interleaved with the others; the fastest
/// counts, as the one least disturbed by everything else on the machine.
const ROUNDS: usize = 7;

/// A name, an instrument, and the blocks of events it plays.
type Contender<'a> = (&'static str, Box<dyn Instrument>, &'a [Vec<NoteEvent>]);

fn time(instrument: &mut dyn Instrument, blocks: &[Vec<NoteEvent>]) -> Duration {
    instrument.reset();
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    let start = Instant::now();
    for events in blocks {
        instrument.process(events, &mut left, &mut right);
        black_box((&left, &right));
    }
    start.elapsed()
}

/// Times every contender in interleaved rounds, keeping each one's fastest,
/// and prints them against the first, `reference`.
fn report(contenders: &mut [Contender], reference: &str) {
    let mut best = vec![Duration::MAX; contenders.len()];
    for _ in 0..ROUNDS {
        for (slot, (_, instrument, blocks)) in contenders.iter_mut().enumerate() {
            best[slot] = best[slot].min(time(&mut **instrument, blocks));
        }
    }
    let first = best[0].as_secs_f64();
    println!("{SECONDS} s of audio at {RATE} Hz in {BLOCK}-sample blocks, best of {ROUNDS}:");
    for ((name, _, _), took) in contenders.iter().zip(&best) {
        let seconds = took.as_secs_f64();
        println!(
            "  {name:<38} {:>6.3}% of a core  {:>5.2}x {reference}",
            100.0 * seconds / SECONDS as f64,
            seconds / first,
        );
    }
}

/// Samples per sixteenth note at 120 bpm.
const SIXTEENTH: usize = (RATE as usize) * 60 / 120 / 4;

/// Notes with their sample offsets within a loop, and the loop's length.
struct Score {
    events: Vec<(usize, NoteEventKind)>,
    length: usize,
}

/// Two bars with every voice busy: kick on the beat, snare and clap on two
/// and four, closed hats on sixteenths with the open hat on the off-beats
/// (choked each time), pedal hat, shaker, ride on eighths, crash on the one,
/// cowbell and rimshot, and a tom fill.
fn groove() -> Score {
    let mut events = Vec::new();
    for step in 0..32 {
        let at = step * SIXTEENTH;
        let mut play = |note: u8, velocity: u8| events.push((at, note_on(note, velocity)));
        if step % 4 == 0 {
            play(36, 120);
        }
        if step % 8 == 4 {
            play(38, 110);
            play(39, 90);
        }
        if step % 4 == 2 {
            play(46, 90);
        } else {
            play(42, 60 + (step as u8 * 13) % 40);
        }
        if step % 4 == 3 {
            play(44, 70);
        }
        play(70, 50 + (step as u8 * 29) % 50);
        if step % 2 == 0 {
            play(51, 80);
        }
        if step == 0 {
            play(49, 120);
        }
        if step % 8 == 6 {
            play(56, 90);
            play(37, 100);
        }
        if step >= 26 {
            play([50, 48, 47, 45, 43, 41][step - 26], 110);
        }
    }
    Score {
        events,
        length: 32 * SIXTEENTH,
    }
}

/// Every voice struck on every beat: the worst case.
fn everything() -> Score {
    let notes = [
        36, 37, 38, 39, 42, 44, 46, 41, 45, 48, 49, 51, 56, 70, 63, 75,
    ];
    Score {
        events: notes.iter().map(|&note| (0, note_on(note, 127))).collect(),
        length: 4 * SIXTEENTH,
    }
}

/// Four-note chords struck on every beat and held to the next: four voices
/// sounding, and four more in their release.
fn four_note_chords() -> Score {
    let progression: [[u8; 4]; 4] = [
        [48, 60, 64, 67],
        [45, 57, 60, 64],
        [41, 57, 60, 65],
        [43, 55, 59, 62],
    ];
    let mut events = Vec::new();
    for (beat, chord) in progression.iter().enumerate() {
        let at = beat * 4 * SIXTEENTH;
        let previous = progression[(beat + 3) % 4];
        events.extend(previous.iter().map(|&note| (at, note_off(note))));
        events.extend(chord.iter().map(|&note| (at, note_on(note, 100))));
    }
    Score {
        events,
        length: 16 * SIXTEENTH,
    }
}

/// Sixteen notes held throughout: the Poly Synth's full polyphony.
fn sixteen_held() -> Score {
    Score {
        events: (0..16).map(|i| (0, note_on(40 + 3 * i, 100))).collect(),
        length: usize::MAX,
    }
}

/// Nothing at all.
fn silence() -> Score {
    Score {
        events: Vec::new(),
        length: usize::MAX,
    }
}

/// Each block's events, worked out before timing starts.
fn blocks(score: &Score) -> Vec<Vec<NoteEvent>> {
    let count = SECONDS * RATE as usize / BLOCK;
    (0..count)
        .map(|block| {
            let start = block * BLOCK;
            let mut events: Vec<NoteEvent> = score
                .events
                .iter()
                .filter_map(|&(at, kind)| {
                    let offset = (at + score.length - start % score.length) % score.length;
                    (offset < BLOCK).then(|| event(offset, kind))
                })
                .collect();
            if score.length == usize::MAX && block > 0 {
                events.clear();
            }
            events.sort_unstable();
            events
        })
        .collect()
}

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn drums_cpu_relative_to_the_poly_synth() {
    let _timing = timing();
    let (poly, drums) = ("canopy.poly_synth", "canopy.drums");
    let cases = [
        ("Poly Synth: 4-note chords", poly, four_note_chords()),
        ("Poly Synth: 16 notes held", poly, sixteen_held()),
        ("Drum Synth: busy groove", drums, groove()),
        ("Drum Synth: every voice, every beat", drums, everything()),
        ("Drum Synth: silence", drums, silence()),
    ];
    let prepared: Vec<_> = cases.iter().map(|(_, _, score)| blocks(score)).collect();
    let mut contenders: Vec<Contender> = cases
        .iter()
        .zip(&prepared)
        .map(|((name, id, _), blocks)| (*name, instrument(id, BLOCK, &[]), &blocks[..]))
        .collect();
    report(&mut contenders, "the chords");
}

/// Six-note chords every half second, each held for most of it, cycling
/// through four shapes: what a strummed rhythm part does. With `vibrato`,
/// the mod wheel is half up throughout.
fn strums(seconds: usize, vibrato: bool) -> Vec<Vec<NoteEvent>> {
    let shapes: [[u8; 6]; 4] = [
        [40, 47, 52, 55, 59, 64],
        [45, 52, 57, 60, 64, 69],
        [43, 47, 50, 55, 59, 67],
        [41, 48, 53, 57, 60, 65],
    ];
    let blocks = seconds * RATE as usize / BLOCK;
    let every = (RATE as usize / 2) / BLOCK;
    let mut out = vec![Vec::new(); blocks];
    for (i, block) in (0..blocks).step_by(every).enumerate() {
        let shape = shapes[i % shapes.len()];
        for &note in &shape {
            out[block].push(event(0, note_on(note, 100)));
            if block + every - 4 < blocks {
                out[block + every - 4].push(event(0, note_off(note)));
            }
        }
        if i == 0 && vibrato {
            let wheel = NoteEventKind::Control {
                controller: MOD_WHEEL,
                value: 64,
            };
            out[block].push(event(0, wheel));
        }
    }
    for block in &mut out {
        block.sort_unstable();
    }
    out
}

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn guitar_cpu_relative_to_the_poly_synth() {
    let _timing = timing();
    let (plain, wheel) = (strums(SECONDS, false), strums(SECONDS, true));
    let mut contenders: Vec<Contender> = vec![
        (
            "Poly Synth",
            instrument("canopy.poly_synth", BLOCK, &[]),
            &plain,
        ),
        (
            "Steel (defaults)",
            instrument("canopy.guitar", BLOCK, &[]),
            &plain,
        ),
        (
            "Nylon",
            instrument("canopy.guitar", BLOCK, &[("model", 1.0)]),
            &plain,
        ),
        (
            "Electric, no amp",
            instrument("canopy.guitar", BLOCK, &[("model", 2.0)]),
            &plain,
        ),
        (
            "Electric, lead amp",
            instrument("canopy.guitar", BLOCK, &[("model", 2.0), ("amp", 3.0)]),
            &plain,
        ),
        (
            "Steel, vibrato",
            instrument("canopy.guitar", BLOCK, &[]),
            &wheel,
        ),
        (
            "Steel, no resonance",
            instrument("canopy.guitar", BLOCK, &[("resonance", 0.0)]),
            &plain,
        ),
    ];
    println!("Six-note chords every 0.5 s:");
    report(&mut contenders, "the Poly Synth");
}

/// The busiest case (six-string chords through the lead amp, every string
/// ringing) stays under this share of real time in any build, debug
/// included: room for a dozen guitars on one core even unoptimized.
const BUDGET: f64 = 0.12;

#[test]
fn the_busiest_guitar_stays_well_inside_real_time() {
    let _timing = timing();
    let blocks = strums(4, true);
    let mut guitar = instrument("canopy.guitar", BLOCK, &[("model", 2.0), ("amp", 3.0)]);
    let mut acoustic = instrument("canopy.guitar", BLOCK, &[]);
    // Best of three: the least disturbed by whatever else the machine does.
    let mut share = f64::MAX;
    for _ in 0..3 {
        let took = time(&mut *guitar, &blocks).max(time(&mut *acoustic, &blocks));
        share = share.min(took.as_secs_f64() / 4.0);
    }
    println!(
        "{} build: the busiest guitar takes {:.2}% of real time",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        share * 100.0
    );
    assert!(share < BUDGET, "{:.1}% of real time", share * 100.0);
}

/// Seconds of CPU per second of audio for a six-note chord held for
/// [`HELD_SECONDS`].
fn cost(instrument: &mut dyn Instrument) -> f64 {
    let blocks = HELD_SECONDS * RATE as usize / BLOCK;
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    let events: Vec<NoteEvent> = [48u8, 55, 60, 64, 67, 72]
        .iter()
        .map(|&note| on(note))
        .collect();
    // Warm up caches and the branch predictor.
    instrument.process(&events, &mut left, &mut right);
    for _ in 0..100 {
        instrument.process(&[], &mut left, &mut right);
    }
    let start = Instant::now();
    let mut sink = 0.0f32;
    for _ in 0..blocks {
        instrument.process(&[], &mut left, &mut right);
        sink += left[0];
    }
    assert!(sink.is_finite());
    start.elapsed().as_secs_f64() / HELD_SECONDS as f64
}

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn wavetable_cpu_relative_to_the_poly_synth() {
    let _timing = timing();
    let mut poly = instrument("canopy.poly_synth", BLOCK, &[]);
    let mut synth = Wavetable::new();
    synth.prepare(RATE, BLOCK);
    for osc in 0..2 {
        synth.set_param(osc_param(osc, OSC_ON), 1.0);
        synth.set_param(osc_param(osc, OSC_UNISON), 7.0); // 8 voices
    }
    let mut heavy = Wavetable::new();
    heavy.prepare(RATE, BLOCK);
    for osc in 0..2 {
        heavy.set_param(osc_param(osc, OSC_UNISON), 15.0); // 16 voices
        heavy.set_param(osc_param(osc, OSC_WARP_MODE), WARP_BEND_POS as f32);
        heavy.set_param(osc_param(osc, OSC_WARP), 0.5);
    }
    heavy.set_param(SUB_ON, 1.0);
    heavy.set_param(NOISE_ON, 1.0);
    heavy.set_param(FILTER_TYPE, FORMANT as f32);

    let mut default_patch = Wavetable::new();
    default_patch.prepare(RATE, BLOCK);

    // The default patch with the newer features on: a second warp on both
    // oscillators, A stacked an octave up with its warp spread, filter 2 in
    // parallel taking B, envelope delay and hold, a drawn LFO on the
    // position and curved matrix slots.
    let mut modern = Wavetable::new();
    modern.prepare(RATE, BLOCK);
    for osc in 0..2 {
        modern.set_param(osc_param(osc, OSC_WARP_MODE), WARP_BEND_POS as f32);
        modern.set_param(osc_param(osc, OSC_WARP), 0.3);
        modern.set_param(osc_param(osc, OSC_WARP2_MODE), WARP_MIRROR as f32);
        modern.set_param(osc_param(osc, OSC_WARP2), 0.4);
    }
    modern.set_param(osc_param(0, OSC_STACK), 1.0);
    modern.set_param(osc_param(0, OSC_WARP_SPREAD), 0.5);
    modern.set_param(FILTER2_ON, 1.0);
    modern.set_param(FILTER_ROUTING, 1.0);
    modern.set_param(FILTER_TARGET_OSC_B, 1.0);
    modern.set_param(env_param(0, ENV_HOLD), 50.0);
    modern.set_param(lfo_param(0, LFO_SHAPE), SHAPE_CUSTOM as f32);
    for slot in 0..4 {
        modern.set_param(slot_param(slot, SLOT_CURVE), -0.4);
    }

    // The 2x8 chord with osc C on as well, playing a user table.
    let mut three = Wavetable::new();
    three.prepare(RATE, BLOCK);
    let saw: Vec<f32> = (0..16 * 2048)
        .map(|i| 2.0 * ((i % 2048) as f32 / 2048.0 + 0.5).fract() - 1.0)
        .collect();
    let table = UserTable::from_samples(&saw, 2048).unwrap();
    let tables = UserTables {
        tables: [None, None, Some(std::sync::Arc::new(table))],
    };
    three.set_data(std::sync::Arc::new(tables));
    for osc in 0..OSC_COUNT {
        three.set_param(osc_param(osc, OSC_ON), 1.0);
        three.set_param(osc_param(osc, OSC_UNISON), 7.0); // 8 voices
    }
    three.set_param(osc_param(2, OSC_TABLE), USER_TABLE as f32);

    // The 2x8 chord rendered at twice the rate (High quality).
    let mut oversampled = Wavetable::new();
    oversampled.prepare(RATE, BLOCK);
    for osc in 0..2 {
        oversampled.set_param(osc_param(osc, OSC_UNISON), 7.0);
    }
    oversampled.set_param(QUALITY, 1.0);

    // The 2x8 chord with a spectral morph on both oscillators, swept by an
    // LFO so every voice makes a new frame as often as it may: the worst
    // case, since a still morph costs nothing after its first frame.
    let mut morphing = Wavetable::new();
    morphing.prepare(RATE, BLOCK);
    for osc in 0..2 {
        morphing.set_param(osc_param(osc, OSC_UNISON), 7.0);
        morphing.set_param(
            osc_param(osc, OSC_MORPH_MODE),
            MORPH_HARMONIC_STRETCH as f32,
        );
        morphing.set_param(osc_param(osc, OSC_MORPH), 0.4);
    }
    morphing.set_param(slot_param(6, SLOT_SOURCE), SRC_LFO1 as f32);
    morphing.set_param(slot_param(6, SLOT_DEST), DEST_MORPH_A as f32);
    morphing.set_param(slot_param(6, SLOT_AMOUNT), 0.3);
    morphing.set_param(slot_param(7, SLOT_SOURCE), SRC_LFO1 as f32);
    morphing.set_param(slot_param(7, SLOT_DEST), (DEST_MORPH_A + 1) as f32);
    morphing.set_param(slot_param(7, SLOT_AMOUNT), 0.3);
    morphing.set_param(lfo_param(0, LFO_RATE), 2.0);

    // The default patch with the whole modulation system busy: every LFO
    // and envelope routed, every slot in use, two LFOs and an envelope
    // mono, a drawn Points LFO and a chaos one. Some slots move envelope
    // times (stretching running stages every tick), LFO phases and
    // smoothing, the volume and a spread.
    let mut busy = Wavetable::new();
    busy.prepare(RATE, BLOCK);
    for slot in 0..MOD_SLOTS {
        let source = SOURCE_GROUPS[slot % 2].1[slot % 6];
        let dest = [
            DEST_CUTOFF,
            DEST_OSC_A,
            DEST_OSC_A + 8 + DEST_WARP,
            DEST_RESONANCE,
            env_dest(0, ENV_DEST_DECAY),
            env_dest(1, ENV_DEST_ATTACK),
            env_dest(0, ENV_DEST_SUSTAIN),
            lfo_dest(0, LFO_DEST_PHASE),
            lfo_dest(1, LFO_DEST_SMOOTH),
            DEST_VOLUME,
            DEST_WARP_SPREAD_A,
        ][slot % 11];
        busy.set_param(slot_param(slot, SLOT_SOURCE), source as f32);
        busy.set_param(slot_param(slot, SLOT_DEST), dest as f32);
        busy.set_param(slot_param(slot, SLOT_AMOUNT), 0.02);
        busy.set_param(slot_param(slot, SLOT_CURVE), 0.3);
    }
    busy.set_param(lfo_param(2, LFO_MONO), 1.0);
    busy.set_param(lfo_param(5, LFO_MONO), 1.0);
    busy.set_param(env_mono_param(2), 1.0);

    let mut results = Vec::new();
    let mut newer = Vec::new();
    for round in 0..3 {
        let poly_cost = cost(&mut *poly);
        let default_cost = cost(&mut default_patch);
        let modern_cost = cost(&mut modern);
        let synth_cost = cost(&mut synth);
        let heavy_cost = cost(&mut heavy);
        let three_cost = cost(&mut three);
        results.push((poly_cost, synth_cost, heavy_cost, three_cost));
        println!(
            "round {round}: poly synth {:.2}% of real time, default patch {:.2}% ({:.1}x poly), \
             default + new features {:.2}% ({:.1}x), wavetable 2x8 unison {:.2}% ({:.1}x poly), \
             2x16 unison + bend + sub + noise + formant {:.2}% ({:.1}x)",
            poly_cost * 100.0,
            default_cost * 100.0,
            default_cost / poly_cost,
            modern_cost * 100.0,
            modern_cost / poly_cost,
            synth_cost * 100.0,
            synth_cost / poly_cost,
            heavy_cost * 100.0,
            heavy_cost / poly_cost
        );
        println!(
            "         3x8 unison, C on a user table {:.2}% ({:.2}x the 2x8 chord, whose C is off)",
            three_cost * 100.0,
            three_cost / synth_cost
        );
        let oversampled_cost = cost(&mut oversampled);
        let morphing_cost = cost(&mut morphing);
        let busy_cost = cost(&mut busy);
        newer.push((oversampled_cost, morphing_cost, busy_cost));
        println!(
            "         2x8 at twice the rate {:.2}% ({:.2}x), 2x8 with swept morphs {:.2}% ({:.2}x), \
             default with every modulator busy {:.2}% ({:.2}x the default)",
            oversampled_cost * 100.0,
            oversampled_cost / synth_cost,
            morphing_cost * 100.0,
            morphing_cost / synth_cost,
            busy_cost * 100.0,
            busy_cost / default_cost
        );
    }
    let best = results.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
    assert!(
        best.1 < 0.25,
        "the 2x8 unison chord takes {:.1}% of real time",
        best.1 * 100.0
    );
    let best_three = results.iter().map(|r| r.3).fold(f64::MAX, f64::min);
    assert!(
        best_three < 0.375,
        "the 3x8 unison chord takes {:.1}% of real time",
        best_three * 100.0
    );
    // Twice the rate may cost about twice; a swept morph on every voice,
    // a frame per voice every other tick; a busy matrix, little.
    let best = |pick: fn(&(f64, f64, f64)) -> f64| newer.iter().map(pick).fold(f64::MAX, f64::min);
    let (oversampled, morphing, busy) = (best(|r| r.0), best(|r| r.1), best(|r| r.2));
    assert!(
        oversampled < 0.5,
        "the 2x8 chord at twice the rate takes {:.1}%",
        oversampled * 100.0
    );
    assert!(
        morphing < 0.4,
        "the 2x8 chord with swept morphs takes {:.1}%",
        morphing * 100.0
    );
    assert!(
        busy < 0.15,
        "the default patch with every modulator busy takes {:.1}%",
        busy * 100.0
    );
}
