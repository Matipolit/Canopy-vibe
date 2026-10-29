//! Aborts the test binary if a built-in instrument allocates or frees memory
//! on the audio thread. Allocation there can block on a lock inside the
//! allocator, which shows up as random dropouts that are hard to trace.
//!
//! Every instrument goes through notes starting (some already bent),
//! bending, retriggering, releasing and being stolen, every parameter's
//! extremes and a reset; then each through what only it has:
//!
//! - the Drum Synth: every voice retriggered while it rings, the hats
//!   choking each other, meters;
//! - the guitar: strums still on their way, slides and hammer-ons, the
//!   wheels and pedal, key-switched palm mutes, harmonics and dead notes,
//!   every model, pickup and amp voicing switched while sounding, blocks
//!   longer than prepared for;
//! - the sampler with an instrument loaded: round robin, release triggers,
//!   chokes, keyswitches, stealing, filters and LFOs;
//! - the slicer with a loop loaded: cut and ringing slices, stealing,
//!   reversed slices, note bends and the wheel;
//! - the wavetable synth: user tables in all three oscillators, every
//!   table, warp, morph, filter routing, LFO shape and mode, voice mode and
//!   matrix destination, mono modulators, envelope delays and holds,
//!   `modulate` and `mod_outputs`, with the controllers and tempo moving.
//!
//! Only meaningful in debug builds: assert_no_alloc has no allocator to
//! install under --release, so there this file is empty.

#![cfg(debug_assertions)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{bends, cascade, event, note_off, note_on, range};
use instruments::builtin::drums::{self, voice_for_note};
use instruments::builtin::guitar::{KEY_SWITCH_FIRST, SOFT_PEDAL};
use instruments::builtin::sampler::{self, Decoded};
use instruments::builtin::wavetable::*;
use instruments::{Instrument, MAX_METERS, ParameterKind, builtins, find_builtin};
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const BLOCK: usize = 256;

const ALL_NOTES_OFF: [NoteEvent; 1] = [NoteEvent {
    frame: 0,
    kind: NoteEventKind::AllNotesOff,
}];

#[test]
fn builtins_never_allocate_while_processing() {
    let note = |i: usize| 24 + (i * 7 % 72) as u8;
    let mut events = cascade(BLOCK, note, |i| (i * 13 % 128) as u8);
    events.extend(bends(BLOCK, note));
    events.push(event(BLOCK - 1, NoteEventKind::AllNotesOff));
    events.sort_unstable();
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);

    for descriptor in builtins() {
        let mut instrument = (descriptor.create)();
        instrument.prepare(48_000.0, BLOCK);
        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..20 {
                instrument.process(&events, &mut left, &mut right);
            }
            for (index, param) in instrument.params().iter().enumerate() {
                let (min, max) = range(param);
                for value in [min, max, param.default_value()] {
                    instrument.set_param(index, value);
                    instrument.process(&events, &mut left, &mut right);
                }
            }
            instrument.set_param(usize::MAX, 0.0);
            instrument.process(&ALL_NOTES_OFF, &mut left, &mut right);
            for _ in 0..10 {
                instrument.process(&[], &mut left, &mut right);
            }
            instrument.process(&[], &mut [], &mut []);
            instrument.reset();
            instrument.process(&events, &mut left, &mut right);
            instrument.reset();
        });
    }
}

/// Every mapped note twice (so each voice is retriggered while it rings),
/// the hats in choking order, and everything a host may send besides.
fn drum_block() -> Vec<NoteEvent> {
    let notes: Vec<u8> = (0..128).filter(|&n| voice_for_note(n).is_some()).collect();
    let mut events = Vec::new();
    let mut push = |frame: usize, kind| events.push(event(frame % BLOCK, kind));
    for (i, &note) in notes.iter().chain(&notes).enumerate() {
        push(i * 5, note_on(note, (i * 29 % 128) as u8));
        let pitch = midi::Pitch::new(note).unwrap();
        let cents = (i as i16 * 41) % 1200;
        push(i * 5, NoteEventKind::NoteBend { pitch, cents });
        push(
            i * 5 + 2,
            NoteEventKind::NoteBend {
                pitch,
                cents: -cents,
            },
        );
    }
    for (frame, note) in [(10, 46), (12, 42), (20, 46), (21, 44), (200, 46), (201, 42)] {
        push(frame, note_on(note, 127));
    }
    push(30, note_off(36));
    push(40, NoteEventKind::PitchBend(4000));
    push(
        41,
        NoteEventKind::Control {
            controller: 1,
            value: 90,
        },
    );
    push(BLOCK - 1, NoteEventKind::AllNotesOff);
    events.sort_unstable();
    events
}

#[test]
fn drums_never_allocate_on_the_audio_thread() {
    let events = drum_block();
    let early: Vec<NoteEvent> = events.iter().copied().filter(|e| e.frame < 100).collect();
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut meters = [0.0; MAX_METERS];
    let mut kit = (drums::DESCRIPTOR.create)();
    kit.prepare(48_000.0, BLOCK);
    let mut loudest = 0.0f32;

    assert_no_alloc::assert_no_alloc(|| {
        kit.set_tempo(128.0);
        for _ in 0..40 {
            kit.process(&events, &mut left, &mut right);
            kit.meters(&mut meters);
            loudest = loudest.max(meters.iter().copied().fold(0.0, f32::max));
        }
        for (index, param) in kit.params().iter().enumerate() {
            let (min, max) = range(param);
            for value in [min, max, f32::NAN, f32::INFINITY, param.default_value()] {
                kit.set_param(index, value);
                kit.process(&events, &mut left, &mut right);
                kit.meters(&mut meters);
            }
        }
        kit.set_param(usize::MAX, 0.0);
        // A short block, an empty one, and long enough for every hit to end.
        kit.process(&early, &mut left[..100], &mut right[..100]);
        kit.process(&[], &mut [], &mut []);
        for _ in 0..1000 {
            kit.process(&[], &mut left, &mut right);
        }
        kit.meters(&mut meters);
        kit.reset();
        kit.process(&events, &mut left, &mut right);
        kit.reset();
    });

    assert!(loudest > 0.0, "the voices played");
    assert!(meters.iter().all(|&m| m == 0.0), "and stopped");
}

fn on(frame: usize, note: u8, velocity: u8) -> NoteEvent {
    event(frame, note_on(note, velocity))
}

fn off(frame: usize, note: u8) -> NoteEvent {
    event(frame, note_off(note))
}

/// A strummed chord (down, then up), a held note with a slide and a
/// hammer-on over it, the wheels moving, and a note released before its
/// turn in the strum.
fn bend(frame: usize, note: u8, cents: i16) -> NoteEvent {
    let pitch = midi::Pitch::new(note).unwrap();
    event(frame, NoteEventKind::NoteBend { pitch, cents })
}

fn phrase() -> Vec<NoteEvent> {
    let mut events = vec![
        on(0, 40, 110),
        on(0, 47, 110),
        on(0, 52, 110),
        on(0, 55, 110),
        on(0, 59, 110),
        on(0, 64, 110),
        off(10, 64),
        event(
            20,
            NoteEventKind::Control {
                controller: MOD_WHEEL,
                value: 127,
            },
        ),
        event(30, NoteEventKind::PitchBend(4000)),
        on(64, 57, 90),
        bend(64, 57, 150),
        bend(70, 57, 200),
        // Bends of notes still on their way in the strum.
        bend(1, 59, -100),
        bend(5, 40, 1200),
        off(80, 40),
        off(80, 47),
        off(80, 52),
        off(80, 55),
        off(80, 59),
        on(120, 60, 90),
        off(130, 57),
        on(160, 62, 90),
        off(170, 60),
        on(200, 48, 40),
        on(200, 52, 40),
        on(200, 55, 40),
        event(240, NoteEventKind::PitchBend(-8192)),
        on(250, 127, 127),
        on(251, 0, 1),
        // Palm mute by key-switch and pedal, harmonics, dead notes, back
        // to open.
        on(2, KEY_SWITCH_FIRST + 1, 100),
        event(
            3,
            NoteEventKind::Control {
                controller: SOFT_PEDAL,
                value: 90,
            },
        ),
        on(140, KEY_SWITCH_FIRST + 2, 100),
        on(141, 52, 90),
        on(142, 64, 90),
        on(180, KEY_SWITCH_FIRST + 3, 100),
        on(181, 45, 70),
        on(182, 50, 70),
        on(230, KEY_SWITCH_FIRST, 100),
        event(
            231,
            NoteEventKind::Control {
                controller: SOFT_PEDAL,
                value: 0,
            },
        ),
    ];
    events.sort_unstable();
    events
}

#[test]
fn the_guitar_never_allocates_while_playing() {
    let descriptor = find_builtin("canopy.guitar").unwrap();
    let mut guitar = (descriptor.create)();
    guitar.prepare(48_000.0, BLOCK);
    let events = phrase();
    let (mut left, mut right) = (vec![0.0; 2 * BLOCK], vec![0.0; 2 * BLOCK]);
    let mut meters = [0.0; MAX_METERS];
    let strum = descriptor
        .params
        .iter()
        .position(|p| p.id == "strum")
        .unwrap();

    assert_no_alloc::assert_no_alloc(|| {
        // A long strum, so plucks are still queued across blocks.
        guitar.set_param(strum, 250.0);
        for round in 0..30 {
            let block = if round % 2 == 0 { &events[..] } else { &[] };
            guitar.process(block, &mut left[..BLOCK], &mut right[..BLOCK]);
            guitar.meters(&mut meters);
        }
        for (index, param) in guitar.params().iter().enumerate() {
            let (min, max) = range(param);
            for value in [max, min, (min + max) / 2.0, param.default_value()] {
                guitar.set_param(index, value);
                guitar.process(&events, &mut left[..BLOCK], &mut right[..BLOCK]);
                guitar.process(&[], &mut left[..BLOCK], &mut right[..BLOCK]);
            }
        }
        // Every amp voicing, switched while notes ring.
        let index = |id: &str| guitar.params().iter().position(|p| p.id == id).unwrap();
        let (amp, model, pickup, pickups) = (
            index("amp"),
            index("model"),
            index("pickup"),
            index("pickup_type"),
        );
        for mode in [1.0, 2.0, 3.0, 0.0] {
            guitar.set_param(amp, mode);
            for _ in 0..4 {
                guitar.process(&events, &mut left[..BLOCK], &mut right[..BLOCK]);
            }
        }
        // Every model, switched while sounding, and every pickup.
        for m in [2.0, 1.0, 0.0, 2.0] {
            guitar.set_param(model, m);
            for p in 0..5 {
                guitar.set_param(pickup, p as f32);
                guitar.set_param(pickups, (p % 2) as f32);
                for _ in 0..3 {
                    guitar.process(&events, &mut left[..BLOCK], &mut right[..BLOCK]);
                }
            }
        }
        // Twice the prepared block.
        guitar.process(&events, &mut left, &mut right);
        guitar.process(&ALL_NOTES_OFF, &mut left[..BLOCK], &mut right[..BLOCK]);
        for _ in 0..400 {
            guitar.process(&[], &mut left[..BLOCK], &mut right[..BLOCK]);
        }
        guitar.process(&[], &mut [], &mut []);
        guitar.meters(&mut meters);
        guitar.reset();
        guitar.process(&events, &mut left[..BLOCK], &mut right[..BLOCK]);
        guitar.reset();
    });
}

const INSTRUMENT: &str = "
<control> default_path=samples/
<global> ampeg_release=0.05
<group> lokey=0 hikey=60 seq_length=2 loop_mode=loop_continuous cutoff=2000 resonance=12 fil_veltrack=1200
         pitchlfo_freq=5 pitchlfo_depth=20 amplfo_freq=3 amplfo_depth=3 fillfo_freq=2 fillfo_depth=600
         fileg_depth=1200 fileg_decay=0.2 pitcheg_depth=-100 pitcheg_decay=0.05
<region> sample=a.wav seq_position=1
<region> sample=b.wav seq_position=2 loop_mode=loop_sustain fil_type=hpf_1p
<group> lokey=61 hikey=127 group=1 off_by=1 note_polyphony=2
<region> sample=a.wav hirand=0.5 loop_mode=one_shot
<region> sample=b.wav lorand=0.5 fil_type=bpf_2p cutoff=800 delay=0.002
<region> sample=a.wav trigger=release rt_decay=3
<group> lokey=20 hikey=30 sw_last=10 sw_lokey=10 sw_hikey=11 trigger=legato polyphony=3
<region> sample=b.wav
";

fn decode(path: &Path) -> Result<Decoded, String> {
    let frames = if path.ends_with("a.wav") { 3000 } else { 9000 };
    let wave = (0..frames)
        .map(|i| ((i * 7 % 101) as f32 / 50.0) - 1.0)
        .collect();
    Ok(Decoded {
        loop_points: Some((500, frames - 500)),
        ..Decoded::mono(wave, 44_100)
    })
}

/// Notes across the keyboard every 8 frames, each released three notes
/// later, plus pitch bends and keyswitches.
fn sampler_block() -> Vec<NoteEvent> {
    let note = |i: usize| (i * 13 % 128) as u8;
    let mut events = cascade(BLOCK, note, |i| (i * 29 % 128) as u8);
    events.extend(bends(BLOCK, note));
    for i in 0..BLOCK / 8 {
        events.push(event(
            i * 8,
            NoteEventKind::PitchBend((i as i16 * 997) % 8192),
        ));
    }
    // The keyswitch that lets the legato group on keys 20..=30 play.
    events.push(event(4, note_on(10, 127)));
    events.push(event(5, note_off(10)));
    events.sort_unstable();
    events
}

#[test]
fn a_loaded_sampler_never_allocates_while_processing() {
    let bank = sampler::load_str(INSTRUMENT, Path::new("/test/kit.sfz"), &decode).unwrap();
    assert!(bank.warnings().is_empty(), "{:?}", bank.warnings());
    let mut instrument = (sampler::DESCRIPTOR.create)();
    instrument.set_data(Arc::new(bank));
    instrument.prepare(48_000.0, BLOCK);

    let events = sampler_block();
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut meters = [0.0; MAX_METERS];
    let mut voices_seen = 0.0f32;
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..40 {
            instrument.process(&events, &mut left, &mut right);
            instrument.meters(&mut meters);
            voices_seen = voices_seen.max(meters[sampler::METER_VOICES]);
        }
        for (index, param) in instrument.params().iter().enumerate() {
            let (min, max) = range(param);
            for value in [min, max, param.default_value()] {
                instrument.set_param(index, value);
                instrument.process(&events, &mut left, &mut right);
            }
        }
        instrument.process(&ALL_NOTES_OFF, &mut left, &mut right);
        for _ in 0..20 {
            instrument.process(&[], &mut left, &mut right);
        }
        instrument.process(&[], &mut [], &mut []);
        instrument.meters(&mut meters);
        instrument.reset();
        instrument.process(&events, &mut left, &mut right);
        instrument.reset();
    });
    assert!(voices_seen >= 32.0, "it was busy: {voices_seen} voices");
    assert!(left.iter().chain(&right).all(|s| s.is_finite()));
}

#[test]
fn a_loaded_slicer_never_allocates_while_playing() {
    use instruments::builtin::slicer::{self, METER_VOICES, SliceData};
    let audio: Vec<f32> = (0..20_000)
        .map(|i| ((i * 7 % 101) as f32 / 50.0) - 1.0)
        .collect();
    let data = SliceData {
        left: audio.clone().into(),
        right: audio.into(),
        sample_rate: 44_100.0,
        starts: (0..64).map(|k| k * 300).collect(),
        speed: 1.3,
    };
    let mut instrument = (slicer::DESCRIPTOR.create)();
    instrument.set_data(Arc::new(data));
    instrument.prepare(48_000.0, BLOCK);
    let mut events = cascade(BLOCK, |i| 50 + (i * 5 % 80) as u8, |i| (i * 29 % 128) as u8);
    for i in 0..BLOCK / 8 {
        let pitch = midi::Pitch::new(50 + (i * 5 % 80) as u8).unwrap();
        events.push(event(
            i * 8 + 1,
            NoteEventKind::NoteBend { pitch, cents: 150 },
        ));
        events.push(event(
            i * 8,
            NoteEventKind::PitchBend((i as i16 * 997) % 8192),
        ));
    }
    events.sort_unstable();
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut meters = [0.0; MAX_METERS];
    let mut voices_seen = 0.0f32;
    assert_no_alloc::assert_no_alloc(|| {
        for cut in [1.0, 0.0] {
            instrument.set_param(slicer::CUT, cut);
            for _ in 0..20 {
                instrument.process(&events, &mut left, &mut right);
                instrument.meters(&mut meters);
                voices_seen = voices_seen.max(meters[METER_VOICES]);
            }
        }
        for (index, param) in instrument.params().iter().enumerate() {
            let (min, max) = range(param);
            for value in [max, min, param.default_value()] {
                instrument.set_param(index, value);
                instrument.process(&events, &mut left, &mut right);
            }
        }
        instrument.process(&ALL_NOTES_OFF, &mut left, &mut right);
        instrument.process(&[], &mut [], &mut []);
        instrument.reset();
    });
    assert!(voices_seen >= 16.0, "it was busy: {voices_seen} voices");
    assert!(left.iter().chain(&right).all(|s| s.is_finite()));
}

/// Notes starting, releasing and being stolen, with the wheels moving.
fn wavetable_block() -> Vec<NoteEvent> {
    let note = |i: usize| 24 + (i * 7 % 72) as u8;
    let mut events = cascade(BLOCK, note, |i| (i * 13 % 128) as u8);
    events.extend(bends(BLOCK, note));
    for i in 0..BLOCK / 8 {
        let value = (i * 17 % 128) as u8;
        events.push(event(
            i * 8,
            NoteEventKind::Control {
                controller: MOD_WHEEL,
                value,
            },
        ));
        events.push(event(
            i * 8,
            NoteEventKind::PitchBend((i as i16 * 997) % 8192),
        ));
    }
    events.sort_unstable();
    events
}

#[test]
fn the_wavetable_synth_never_allocates_while_processing() {
    let events = wavetable_block();
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut meters = [0.0; MAX_METERS];
    let mut synth = Wavetable::new();
    synth.prepare(48_000.0, BLOCK);
    // Every slot in use, so the whole matrix runs, with curves and
    // polarities; drawn LFOs, both filters, envelope delays and holds,
    // second warps, stacks and warp spread on.
    let use_slots = |synth: &mut Wavetable, pass: usize| {
        for slot in 0..MOD_SLOTS {
            let p = |offset| slot_param(slot, offset);
            let source = slot % (SOURCES.len() - 1) + 1;
            let dest = (pass * MOD_SLOTS + slot) % (DESTINATIONS.len() - 1) + 1;
            synth.set_param(p(SLOT_SOURCE), source as f32);
            synth.set_param(p(SLOT_DEST), dest as f32);
            let aux = SOURCES.len() - 1 - slot % SOURCES.len();
            synth.set_param(p(SLOT_AUX), aux as f32);
            synth.set_param(p(SLOT_AMOUNT), 0.3);
            synth.set_param(p(SLOT_CURVE), (slot % 5) as f32 * 0.5 - 1.0);
            synth.set_param(p(SLOT_POLARITY), (slot % POLARITIES.len()) as f32);
        }
    };
    use_slots(&mut synth, 0);
    for lfo in 0..LFO_COUNT {
        synth.set_param(lfo_param(lfo, LFO_SHAPE), SHAPE_CUSTOM as f32);
        synth.set_param(lfo_param(lfo, LFO_GLIDE), lfo as f32 / 3.0);
    }
    for env in 0..3 {
        synth.set_param(env_param(env, ENV_DELAY), 3.0);
        synth.set_param(env_param(env, ENV_HOLD), 2.0);
    }
    // Each oscillator plays its own table: a short single cycle, three
    // frames, and a full 256 morphing from a sine to a pulse.
    let frames = |count: usize, size: usize| -> Vec<f32> {
        (0..count * size)
            .map(|i| {
                let (frame, p) = (i / size, (i % size) as f32 / size as f32);
                let t = frame as f32 / count.max(2) as f32;
                let pulse = if p < 0.3 { 1.0 } else { -0.4 };
                (std::f32::consts::TAU * p).sin() * (1.0 - t) + pulse * t
            })
            .collect()
    };
    let table =
        |samples: Vec<f32>| Some(Arc::new(UserTable::from_samples(&samples, 2048).unwrap()));
    let tables = UserTables {
        tables: [
            table(frames(1, 600)),
            table(frames(3, 2048)),
            table(frames(256, 2048)),
        ],
    };
    synth.set_data(Arc::new(tables));
    for osc in 0..OSC_COUNT {
        synth.set_param(osc_param(osc, OSC_ON), 1.0);
        synth.set_param(osc_param(osc, OSC_TABLE), USER_TABLE as f32);
        synth.set_param(osc_param(osc, OSC_WARP2_MODE), WARP_MIRROR as f32);
        synth.set_param(osc_param(osc, OSC_WARP2), 0.5);
        synth.set_param(osc_param(osc, OSC_STACK), 4.0);
        synth.set_param(osc_param(osc, OSC_WARP_SPREAD), 0.7);
    }
    // C listens to A, which listens to B.
    synth.set_param(osc_param(0, OSC_WARP_MODE), WARP_AM as f32);
    synth.set_param(osc_param(0, OSC_WARP), 0.6);
    synth.set_param(osc_param(2, OSC_WARP_MODE), WARP_FM as f32);
    synth.set_param(osc_param(2, OSC_WARP), 0.6);
    synth.set_param(FILTER_TARGET_OSC_C, 1.0);
    synth.set_param(FILTER2_ON, 1.0);
    synth.set_param(FILTER_TARGET_OSC_B, 1.0);
    // The newer paths at once: morphs, twice the rate, mono modulators,
    // the sub as B's FM source, drawn nodes and chaos.
    for osc in 0..OSC_COUNT {
        synth.set_param(osc_param(osc, OSC_MORPH_MODE), (1 + osc * 3) as f32);
        synth.set_param(osc_param(osc, OSC_MORPH), 0.4);
        synth.set_param(osc_param(osc, OSC_DETUNE_MODE), DETUNE_RANDOM as f32);
        synth.set_param(osc_param(osc, OSC_POSITION_SPREAD), 0.5);
    }
    synth.set_param(osc_param(1, OSC_WARP_MODE), WARP_FM as f32);
    synth.set_param(osc_param(1, OSC_MOD_SOURCE), MOD_SOURCE_SUB as f32);
    synth.set_param(SUB_ON, 1.0);
    synth.set_param(NOISE_ON, 1.0);
    synth.set_param(QUALITY, 1.0);
    synth.set_param(lfo_param(0, LFO_MONO), 1.0);
    synth.set_param(lfo_param(4, LFO_MONO), 1.0);
    synth.set_param(lfo_param(4, LFO_MODE), MODE_LOOP as f32);
    synth.set_param(lfo_param(5, LFO_SHAPE), SHAPE_LORENZ as f32);
    synth.set_param(lfo_param(6, LFO_SHAPE), SHAPE_ROSSLER as f32);
    synth.set_param(lfo_param(6, LFO_MONO), 1.0);
    synth.set_param(env_mono_param(1), 1.0);
    synth.set_param(env_mono_param(3), 1.0);
    synth.set_param(slot_param(20, SLOT_SOURCE), SRC_VELOCITY as f32);
    synth.set_param(slot_param(20, SLOT_DEST), (DEST_MACRO1 + 2) as f32);
    synth.set_param(slot_param(20, SLOT_AMOUNT), 0.5);
    synth.set_param(slot_param(21, SLOT_BYPASS), 1.0);
    let mut outputs = vec![0.0; MOD_OUTPUTS.len()];
    assert_no_alloc::assert_no_alloc(|| {
        for bpm in [60.0, 174.0] {
            synth.set_tempo(bpm);
            for routing in [0.0, 1.0] {
                synth.set_param(FILTER_ROUTING, routing);
                for _ in 0..4 {
                    synth.process(&events, &mut left, &mut right);
                    synth.meters(&mut meters);
                }
            }
        }
        // Every destination in turn.
        for pass in 1..DESTINATIONS.len().div_ceil(MOD_SLOTS) + 1 {
            use_slots(&mut synth, pass);
            for _ in 0..2 {
                synth.process(&events, &mut left, &mut right);
                synth.meters(&mut meters);
                synth.mod_outputs(&mut outputs);
            }
        }
        // A modulator moving parameters every block.
        for block in 0..8 {
            let t = block as f32 / 8.0;
            synth.modulate(osc_param(0, OSC_POSITION), t);
            synth.modulate(FILTER_CUTOFF, 200.0 + 4000.0 * t);
            synth.modulate(osc_param(2, OSC_MORPH), 1.0 - t);
            synth.process(&events, &mut left, &mut right);
            synth.mod_outputs(&mut outputs);
        }
        for (index, param) in PARAMS.iter().enumerate() {
            let values: [f32; 2];
            let (steps, value): (usize, &dyn Fn(usize) -> f32) = match param.kind {
                // Walk every choice of the enums: each warp, filter, shape,
                // stack, destination and mode.
                ParameterKind::Enum { labels, .. } => (labels.len(), &|step| step as f32),
                ParameterKind::Float { min, max, .. } => {
                    values = [min, max];
                    (2, &|step| values[step])
                }
            };
            for step in 0..steps {
                synth.set_param(index, value(step));
                synth.process(&events, &mut left, &mut right);
                synth.meters(&mut meters);
            }
            synth.set_param(index, param.default_value());
        }
        synth.set_param(usize::MAX, 0.0);
        synth.set_param(0, f32::NAN);
        synth.process(&ALL_NOTES_OFF, &mut left, &mut right);
        for _ in 0..10 {
            synth.process(&[], &mut left, &mut right);
            synth.meters(&mut meters);
        }
        synth.process(&[], &mut [], &mut []);
        synth.reset();
        synth.process(&events, &mut left, &mut right);
        synth.reset();
    });
}
