//! Playing SFZ instruments built from synthetic samples: region choice,
//! pitch, loops, envelopes, triggers, round robin, chokes and voices.

mod common;

use std::f32::consts::TAU;
use std::path::Path;
use std::sync::Arc;

use common::{event, max_step, note_on, off, peak, rms};
use instruments::builtin::sampler::{
    self, DESCRIPTOR, Decoded, METER_KEYS, METER_KEYSWITCH, METER_LAST_KEY, METER_LAST_REGION,
    METER_LAST_VELOCITY, METER_PEAK, METER_REGION_COUNT, METER_REGIONS, METER_RELEASING,
    METER_VOICES, SfzBank,
};
use instruments::{Instrument, MAX_METERS, ParameterKind};
use midi::{NoteEvent, NoteEventKind};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 256;
/// The rate of the synthetic samples, different from the output's so
/// every test also covers rate conversion.
const SAMPLE_RATE: u32 = 44_100;

/// Samples by name: `sine440` is a second of a 440 Hz sine, `dc25` half a
/// second at 0.25, `short` a quarter second of 440 Hz.
fn decode(path: &Path) -> Result<Decoded, String> {
    let name = path.file_stem().unwrap().to_str().unwrap();
    let rate = f64::from(SAMPLE_RATE);
    let sine = |hz: f64, seconds: f64| {
        (0..(seconds * rate) as usize)
            .map(|i| (std::f64::consts::TAU * hz * i as f64 / rate).sin() as f32)
            .collect()
    };
    let samples = if let Some(hz) = name.strip_prefix("sine") {
        sine(hz.parse().unwrap(), 1.0)
    } else if let Some(level) = name.strip_prefix("dc") {
        vec![level.parse::<f32>().unwrap() / 100.0; (0.5 * rate) as usize]
    } else if name == "short" {
        sine(440.0, 0.25)
    } else {
        return Err(format!("no sample {name}"));
    };
    Ok(Decoded::mono(samples, SAMPLE_RATE))
}

fn bank(text: &str) -> Arc<SfzBank> {
    let bank = sampler::load_str(text, Path::new("/test/test.sfz"), &decode).unwrap();
    assert!(bank.warnings().is_empty(), "{:?}", bank.warnings());
    Arc::new(bank)
}

fn loaded(text: &str) -> Box<dyn Instrument> {
    let mut sampler = (DESCRIPTOR.create)();
    sampler.set_data(bank(text));
    sampler.prepare(RATE, BLOCK);
    sampler
}

/// The index of the sampler's parameter `id`.
fn param(id: &str) -> usize {
    DESCRIPTOR.params.iter().position(|p| p.id == id).unwrap()
}

fn on(key: u8, velocity: u8) -> NoteEvent {
    event(0, note_on(key, velocity))
}

fn at(frame: u32, event: NoteEvent) -> NoteEvent {
    NoteEvent { frame, ..event }
}

fn frames(seconds: f32) -> usize {
    (seconds * RATE) as usize
}

/// Renders `frames` frames with `events` at the start; returns the left
/// channel and checks both are finite.
fn run(sampler: &mut dyn Instrument, events: &[NoteEvent], frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    let mut first = true;
    while out.len() < frames {
        let n = BLOCK.min(frames - out.len());
        let (mut left, mut right) = (vec![0.0; n], vec![0.0; n]);
        sampler.process(if first { events } else { &[] }, &mut left, &mut right);
        let finite = left.iter().chain(&right).all(|s| s.is_finite());
        assert!(finite, "NaN or infinity");
        out.extend_from_slice(&left);
        first = false;
    }
    out
}

fn meters(sampler: &dyn Instrument) -> [f32; MAX_METERS] {
    let mut out = [0.0; MAX_METERS];
    sampler.meters(&mut out);
    out
}

fn voices(sampler: &dyn Instrument) -> f32 {
    meters(sampler)[METER_VOICES]
}

fn last_region(sampler: &dyn Instrument) -> f32 {
    meters(sampler)[METER_LAST_REGION]
}

#[track_caller]
fn assert_near(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual}, expected {expected} ± {tolerance}"
    );
}

/// Pitch from rising zero crossings, interpolated between frames.
fn frequency(samples: &[f32]) -> f32 {
    let crossings: Vec<f32> = samples
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f32 + w[0] / (w[0] - w[1]))
        .collect();
    assert!(crossings.len() > 10, "no steady pitch");
    let span = crossings.last().unwrap() - crossings[0];
    (crossings.len() - 1) as f32 / span * RATE
}

/// Starts `key` from silence and measures its pitch over 0.35 s.
fn pitch_of(sampler: &mut dyn Instrument, key: u8) -> f32 {
    sampler.reset();
    let out = run(sampler, &[on(key, 127)], frames(0.4));
    frequency(&out[frames(0.05)..])
}

#[test]
fn silent_and_inert_without_a_bank() {
    let mut sampler = (DESCRIPTOR.create)();
    sampler.prepare(RATE, BLOCK);
    let out = run(&mut *sampler, &[on(60, 100)], frames(0.1));
    assert_eq!(peak(&out), 0.0);
    let meters = meters(&*sampler);
    assert_eq!(meters[METER_REGION_COUNT], 0.0);
    assert_eq!(meters[METER_VOICES], 0.0);
    assert_eq!(meters[METER_LAST_REGION], -1.0);

    // Data of the wrong type is ignored too.
    sampler.set_data(Arc::new(5u32));
    assert_eq!(peak(&run(&mut *sampler, &[on(60, 100)], BLOCK)), 0.0);
}

#[test]
fn key_and_velocity_pick_the_region() {
    let mut sampler = loaded(
        "<group> amp_veltrack=0 loop_mode=one_shot
         <region> sample=dc10.wav lokey=60 hikey=64 hivel=63
         <region> sample=dc20.wav lokey=60 hikey=64 lovel=64
         <region> sample=dc30.wav lokey=65 hikey=72",
    );
    let cases = [
        (62, 40, 0, 0.1),
        (62, 100, 1, 0.2),
        (70, 1, 2, 0.3),
        (64, 127, 1, 0.2),
    ];
    for (key, velocity, region, level) in cases {
        sampler.reset();
        let out = run(&mut *sampler, &[on(key, velocity)], 500);
        assert_near(out[400], level, 1e-4);
        let meters = meters(&*sampler);
        assert_eq!(meters[METER_LAST_REGION], region as f32);
        assert_eq!(meters[METER_LAST_KEY], key as f32);
        assert_eq!(meters[METER_LAST_VELOCITY], velocity as f32);
        assert_eq!(meters[METER_VOICES], 1.0);
        assert_eq!(meters[METER_REGION_COUNT], 3.0);
    }
    // No region on key 50.
    sampler.reset();
    assert_eq!(peak(&run(&mut *sampler, &[on(50, 100)], 500)), 0.0);
    assert_eq!(voices(&*sampler), 0.0);
}

#[test]
fn pitch_follows_key_root_and_sample_rate() {
    let mut sampler = loaded("<region> sample=sine440.wav pitch_keycenter=a4");
    for (key, hz) in [(69, 440.0), (81, 880.0), (57, 220.0), (76, 659.255)] {
        assert_near(pitch_of(&mut *sampler, key), hz, hz * 0.001);
    }

    let text = "<region> sample=sine440.wav pitch_keycenter=a4 transpose=12 tune=-100";
    let mut sampler = loaded(text);
    let wanted = 880.0 * 2f32.powf(-1.0 / 12.0);
    assert_near(pitch_of(&mut *sampler, 69), wanted, 1.0);

    let text = "<region> sample=sine440.wav pitch_keycenter=a4 pitch_keytrack=0";
    let mut sampler = loaded(text);
    assert_near(pitch_of(&mut *sampler, 81), 440.0, 0.5);
}

/// Rate conversion and transposition by cubic interpolation: about 111 dB
/// signal-to-error on a 440 Hz sine, where linear interpolation gets 69.
#[test]
fn interpolation_is_clean() {
    let fifth_up = 440.0 * 2f64.powf(5.0 / 12.0);
    let fifth_down = 440.0 * 2f64.powf(-7.0 / 12.0);
    for (key, hz) in [(69, 440.0), (74, fifth_up), (62, fifth_down)] {
        let mut sampler = loaded("<region> sample=sine440.wav pitch_keycenter=a4");
        let out = run(&mut *sampler, &[on(key, 127)], frames(0.3));
        let (mut signal, mut error) = (0.0f64, 0.0f64);
        for (n, &sample) in out.iter().enumerate().skip(4) {
            let ideal = (std::f64::consts::TAU * hz * n as f64 / f64::from(RATE)).sin();
            signal += ideal * ideal;
            error += (f64::from(sample) - ideal).powi(2);
        }
        let snr = 10.0 * (signal / error).log10();
        assert!(snr > 90.0, "key {key}: {snr:.1} dB");
    }
}

#[test]
fn pitch_bend_and_tune_parameter() {
    let mut sampler = loaded("<region> sample=sine440.wav pitch_keycenter=a4");
    sampler.set_param(param("tune"), 12.0);
    assert_near(pitch_of(&mut *sampler, 69), 880.0, 1.0);
    sampler.set_param(param("tune"), 0.0);

    sampler.reset();
    run(&mut *sampler, &[on(69, 127)], BLOCK);
    let bend = NoteEvent {
        frame: 0,
        kind: NoteEventKind::PitchBend(8191),
    };
    run(&mut *sampler, &[bend], BLOCK);
    let bent = frequency(&run(&mut *sampler, &[], frames(0.2)));
    assert_near(bent, 440.0 * 2f32.powf(2.0 / 12.0), 1.0);
}

#[test]
fn loops_sustain_without_clicks() {
    // 4000 frames is 39.9 cycles: the loop points don't line up, so jumping
    // back would click without the crossfade the loader adds.
    let mut sampler = loaded(
        "<region> sample=sine440.wav pitch_keycenter=a4
         loop_mode=loop_continuous loop_start=10000 loop_end=13999",
    );
    let out = run(&mut *sampler, &[on(69, 127)], frames(3.0));
    assert_near(rms(&out[frames(2.5)..]), 0.707, 0.05);
    let step = max_step(&out[frames(0.01)..]);
    let sine_step = TAU * 440.0 / RATE;
    assert!(step < sine_step * 1.1, "{step} vs a sine's {sine_step}");
    assert_eq!(voices(&*sampler), 1.0);
}

#[test]
fn loop_sustain_plays_on_past_the_loop_after_release() {
    let mut sampler = loaded(
        "<region> sample=sine440.wav pitch_keycenter=a4 loop_mode=loop_sustain
         loop_start=4410 loop_end=8819 ampeg_release=5",
    );
    let held = run(&mut *sampler, &[on(69, 127)], frames(2.0));
    assert!(rms(&held[frames(1.5)..]) > 0.6, "loops while held");
    // The rest of the sample is under a second; the 5 s release outlasts it.
    let released = run(&mut *sampler, &[off(69)], frames(1.2));
    assert!(rms(&released[..frames(0.1)]) > 0.3, "release starts");
    assert_eq!(voices(&*sampler), 0.0, "ended with the sample");
    assert_eq!(peak(&released[frames(1.1)..]), 0.0);
}

#[test]
fn one_shot_ignores_note_off_and_no_loop_stops() {
    let text = "<region> sample=short.wav loop_mode=one_shot pitch_keycenter=a4";
    let mut sampler = loaded(text);
    let events = [on(69, 127), at(10, off(69))];
    let out = run(&mut *sampler, &events, frames(0.4));
    assert!(rms(&out[frames(0.15)..frames(0.2)]) > 0.6, "plays through");
    // 0.25 s at 44.1 kHz lasts 0.25 s at any output rate.
    assert_eq!(peak(&out[frames(0.26)..]), 0.0, "and stops at its end");
    assert_eq!(voices(&*sampler), 0.0);

    let mut sampler = loaded("<region> sample=sine440.wav ampeg_release=0.02");
    let out = run(&mut *sampler, &events, frames(0.1));
    assert!(peak(&out[frames(0.03)..]) < 1e-3, "released");
}

#[test]
fn release_follows_the_envelope() {
    let mut sampler = loaded(
        "<region> sample=sine440.wav pitch_keycenter=a4 loop_mode=loop_continuous
         ampeg_attack=0.1 ampeg_release=0.5",
    );
    let attack = run(&mut *sampler, &[on(69, 127)], frames(0.2));
    assert!(peak(&attack[..frames(0.02)]) < 0.25, "linear attack");
    assert!(peak(&attack[frames(0.15)..]) > 0.95);
    let release = run(&mut *sampler, &[off(69)], frames(0.6));
    let quarter = peak(&release[frames(0.1)..frames(0.13)]);
    assert!((0.05..0.4).contains(&quarter), "exponential: {quarter}");
    assert_eq!(voices(&*sampler), 0.0);
    assert!(peak(&release[frames(0.5)..]) < 1e-3);
    assert!(max_step(&release) < 0.07, "no click on note-off");

    // The release parameter adds to every region's release.
    sampler.set_param(param("release"), 1000.0);
    run(&mut *sampler, &[on(69, 127)], frames(0.2));
    run(&mut *sampler, &[off(69)], frames(1.0));
    assert_eq!(voices(&*sampler), 1.0, "1.5 s release");
    run(&mut *sampler, &[], frames(0.6));
    assert_eq!(voices(&*sampler), 0.0);
}

#[test]
fn release_triggers_fire_on_note_off() {
    let mut sampler = loaded(
        "<group> amp_veltrack=0 key=60
         <region> sample=dc10.wav loop_mode=loop_continuous
         <region> sample=dc50.wav trigger=release rt_decay=6",
    );
    // A note-off with no note-on before it triggers nothing.
    run(&mut *sampler, &[off(60)], BLOCK);
    assert_eq!(last_region(&*sampler), -1.0);

    let held = run(&mut *sampler, &[on(60, 100)], frames(1.0));
    assert_near(held[frames(0.9)], 0.1, 1e-4);
    assert_eq!(last_region(&*sampler), 0.0);
    let released = run(&mut *sampler, &[off(60)], frames(0.6));
    assert_eq!(last_region(&*sampler), 1.0);
    // Held for a second at 6 dB per second: 0.5 down 6 dB.
    assert_near(released[frames(0.05)], 0.25, 0.01);
    assert_eq!(peak(&released[frames(0.55)..]), 0.0, "it plays once");
}

#[test]
fn round_robin_cycles() {
    let mut sampler = loaded(
        "<group> key=60 seq_length=3 amp_veltrack=0
         <region> sample=dc10.wav seq_position=1
         <region> sample=dc20.wav seq_position=2
         <region> sample=dc30.wav seq_position=3",
    );
    let mut order = Vec::new();
    for _ in 0..7 {
        let out = run(&mut *sampler, &[on(60, 100)], 500);
        let region = last_region(&*sampler);
        order.push(region as usize);
        assert_eq!(voices(&*sampler), 1.0, "one region per note");
        assert_near(out[400], 0.1 * (region + 1.0), 1e-4);
        run(&mut *sampler, &[off(60)], frames(0.05));
    }
    assert_eq!(order, [0, 1, 2, 0, 1, 2, 0]);

    // Reset starts the cycle over.
    sampler.reset();
    run(&mut *sampler, &[on(60, 100)], 500);
    assert_eq!(last_region(&*sampler), 0.0);
}

#[test]
fn random_ranges_pick_one_region_per_note() {
    let mut sampler = loaded(
        "<group> key=60 loop_mode=one_shot
         <region> sample=dc10.wav hirand=0.5
         <region> sample=dc20.wav lorand=0.5",
    );
    let mut counts = [0; 2];
    for _ in 0..200 {
        run(&mut *sampler, &[on(60, 100)], 64);
        assert_eq!(voices(&*sampler), 1.0);
        counts[last_region(&*sampler) as usize] += 1;
        run(&mut *sampler, &[], frames(0.55));
    }
    assert!(counts.iter().all(|c| *c > 60), "{counts:?}");
}

#[test]
fn groups_choke_each_other() {
    let mut sampler = loaded(
        "<group> amp_veltrack=0 loop_mode=loop_continuous
         <region> sample=dc10.wav key=46 group=1 off_by=2
         <region> sample=dc20.wav key=42 group=2
         <region> sample=dc30.wav key=49 group=3 off_by=3 off_mode=normal ampeg_release=0.2",
    );
    // The closed hat chokes the open one, with a fade rather than a cut.
    run(&mut *sampler, &[on(46, 100)], frames(0.1));
    let out = run(&mut *sampler, &[on(42, 100)], frames(0.1));
    assert_eq!(voices(&*sampler), 1.0);
    assert_near(out[frames(0.05)], 0.2, 1e-4);
    assert!(max_step(&out[1..]) < 0.01);

    // A region that chokes itself, in its own release time.
    sampler.reset();
    run(&mut *sampler, &[on(49, 100)], frames(0.1));
    run(&mut *sampler, &[off(49), on(49, 100)], frames(0.1));
    let now = meters(&*sampler);
    assert_eq!(now[METER_VOICES], 2.0);
    assert_eq!(now[METER_RELEASING], 1.0);
    run(&mut *sampler, &[], frames(0.15));
    assert_eq!(voices(&*sampler), 1.0);
}

#[test]
fn keyswitches_pick_articulations() {
    let mut sampler = loaded(
        "<group> sw_lokey=24 sw_hikey=25 sw_default=24 lokey=60 hikey=72
           amp_veltrack=0 loop_mode=one_shot
         <region> sample=dc10.wav sw_last=24
         <region> sample=dc20.wav sw_last=25",
    );
    assert_eq!(meters(&*sampler)[METER_KEYSWITCH], 24.0);
    let out = run(&mut *sampler, &[on(60, 100)], 500);
    assert_near(out[400], 0.1, 1e-4);
    run(&mut *sampler, &[on(25, 100), at(10, off(25))], 64);
    assert_eq!(meters(&*sampler)[METER_KEYSWITCH], 25.0);
    let out = run(&mut *sampler, &[on(61, 100)], 500);
    assert_eq!(last_region(&*sampler), 1.0);
    // Both sound: the first is a one-shot.
    assert_near(out[400], 0.3, 1e-4);
}

#[test]
fn velocity_scales_level_and_sensitivity_parameter() {
    let mut sampler = loaded("<region> sample=dc100.wav loop_mode=one_shot");
    let level = |sampler: &mut dyn Instrument, velocity| {
        sampler.reset();
        run(sampler, &[on(60, velocity)], 500)[400]
    };
    assert_near(level(&mut *sampler, 127), 1.0, 1e-4);
    // Velocity squared.
    assert_near(level(&mut *sampler, 64), 0.25, 1e-3);
    sampler.set_param(param("velocity"), 0.0);
    assert_near(level(&mut *sampler, 64), 1.0, 1e-4);
}

#[test]
fn volume_and_pan_parameters() {
    let text = "<region> sample=dc50.wav loop_mode=one_shot amp_veltrack=0";
    let mut sampler = loaded(text);
    sampler.set_param(param("volume"), -6.0206);
    sampler.set_param(param("pan"), 100.0);
    sampler.reset();
    let (mut left, mut right) = (vec![0.0; 2000], vec![0.0; 2000]);
    sampler.process(&[on(60, 100)], &mut left, &mut right);
    assert_near(left[1500], 0.0, 1e-4);
    assert_near(right[1500], 0.25, 1e-3);
    assert_near(meters(&*sampler)[METER_PEAK], 0.25, 1e-3);
}

#[test]
fn filters_shape_the_sound() {
    for (text, quiet) in [
        ("sample=sine5000.wav", false),
        ("sample=sine5000.wav cutoff=300", true),
        ("sample=sine5000.wav cutoff=300 fil_type=lpf_1p", true),
        ("sample=sine100.wav cutoff=3000 fil_type=hpf_2p", true),
        // Keytracking takes the cutoff 67 keys down, to about 60 Hz.
        (
            "sample=sine100.wav cutoff=3000 fil_type=hpf_2p fil_keytrack=100 fil_keycenter=127",
            false,
        ),
        ("sample=sine440.wav cutoff=440 fil_type=bpf_2p", false),
        ("sample=sine440.wav cutoff=440 fil_type=brf_2p", true),
    ] {
        let mut sampler = loaded(&format!("<region> {text} pitch_keycenter=60"));
        let out = run(&mut *sampler, &[on(60, 127)], frames(0.3));
        let level = rms(&out[frames(0.2)..]);
        if quiet {
            assert!(level < 0.1, "{text}: {level}");
        } else {
            assert!(level > 0.5, "{text}: {level}");
        }
    }
}

#[test]
fn lfos_modulate() {
    let text = "<region> sample=sine440.wav pitch_keycenter=a4 pitchlfo_freq=5 pitchlfo_depth=100";
    let mut sampler = loaded(text);
    let out = run(&mut *sampler, &[on(69, 127)], frames(0.6));
    let first = frequency(&out[..frames(0.1)]);
    let second = frequency(&out[frames(0.1)..frames(0.2)]);
    assert!(first > 450.0 && second < 430.0, "{first}, {second}");

    let mut sampler = loaded("<region> sample=sine440.wav amplfo_freq=4 amplfo_depth=6");
    let out = run(&mut *sampler, &[on(69, 127)], frames(0.25));
    let loud = peak(&out[frames(0.04)..frames(0.08)]);
    let soft = peak(&out[frames(0.17)..frames(0.21)]);
    assert!(loud > 1.5 && soft < 0.65, "tremolo: {loud}, {soft}");
}

#[test]
fn polyphony_steals_voices() {
    // Steady levels with soft attacks, so any cut would show as a step.
    let mut sampler = loaded(
        "<region> sample=dc10.wav amp_veltrack=0 loop_mode=loop_continuous
         ampeg_attack=0.01 ampeg_release=1",
    );
    let chord: Vec<NoteEvent> = (30..100).map(|key| on(key, 100)).collect();
    let out = run(&mut *sampler, &chord, frames(0.05));
    // The 70 notes leave 64 voices once the stolen ones fade.
    let now = meters(&*sampler);
    assert_eq!(now[METER_VOICES], 64.0);
    assert_near(out[frames(0.04)], 6.4, 1e-3);
    // One region, listed once.
    assert_eq!(now[METER_REGIONS], 0.0);
    assert_eq!(now[METER_REGIONS + 1], -1.0);
    // The oldest six made way; keys 36..=99 sound. Slot n holds keys 24n..24n+23.
    assert_eq!(now[METER_KEYS], 0.0);
    assert_eq!(now[METER_KEYS + 1], ((1u32 << 24) - (1 << 12)) as f32);
    assert_eq!(now[METER_KEYS + 2], ((1u32 << 24) - 1) as f32);
    assert_eq!(now[METER_KEYS + 4], ((1u32 << 4) - 1) as f32);
    assert_eq!(now[METER_KEYS + 5], 0.0);

    // Lowering the polyphony fades out the rest.
    let ParameterKind::Enum { labels, .. } = DESCRIPTOR.params[param("polyphony")].kind else {
        panic!("polyphony is a choice");
    };
    let four = labels.iter().position(|l| *l == "4").unwrap();
    sampler.set_param(param("polyphony"), four as f32);
    run(&mut *sampler, &[], frames(0.05));
    assert_eq!(voices(&*sampler), 4.0);
    let out = run(&mut *sampler, &[on(20, 100), on(21, 100)], frames(0.05));
    assert_eq!(voices(&*sampler), 4.0);
    assert_near(out[frames(0.04)], 0.4, 1e-3);
    assert!(max_step(&out) < 0.01, "stealing fades rather than cuts");
}

#[test]
fn stays_finite_and_bounded_at_extremes() {
    let mut sampler = loaded(
        "<region> sample=sine440.wav lokey=0 hikey=63 transpose=60 tune=9600 volume=24
           cutoff=20000 resonance=40 pitchlfo_freq=100 pitchlfo_depth=9600 fillfo_freq=50
           fillfo_depth=9600 amplfo_freq=100 amplfo_depth=24 loop_mode=loop_continuous
           loop_start=0 loop_end=1 fileg_depth=9600 fileg_attack=0.01 pitcheg_depth=-9600
         <region> sample=short.wav lokey=64 hikey=127 transpose=-127 offset=100000000
           delay=0.01 pan=-100 width=-100
         <region> sample=dc100.wav lokey=64 hikey=127 fil_type=bpf_2p cutoff=1 resonance=40
           offset_random=100000 pitch_random=9600",
    );
    for (index, param) in DESCRIPTOR.params.iter().enumerate() {
        let value = match param.kind {
            ParameterKind::Float { max, .. } => max,
            ParameterKind::Enum { labels, .. } => (labels.len() - 1) as f32,
        };
        sampler.set_param(index, value);
        sampler.set_param(index, f32::NAN);
    }
    // Everything at full gain and resonance is loud; the point is that it
    // stays finite.
    let chord: Vec<NoteEvent> = (0..128).step_by(5).map(|key| on(key, 127)).collect();
    let out = run(&mut *sampler, &chord, frames(1.0));
    assert!(peak(&out) < 1e6, "peak {}", peak(&out));
    let bend = NoteEvent {
        frame: 0,
        kind: NoteEventKind::PitchBend(-8192),
    };
    let all_off = NoteEvent {
        frame: 1,
        kind: NoteEventKind::AllNotesOff,
    };
    run(&mut *sampler, &[bend, all_off], frames(0.5));
}

#[test]
fn all_notes_off_releases_everything_and_reset_silences() {
    let mut sampler = loaded(
        "<region> sample=sine440.wav loop_mode=loop_continuous ampeg_release=0.1
         <region> sample=dc50.wav trigger=release",
    );
    let chord = [on(60, 100), on(64, 100), on(67, 100)];
    run(&mut *sampler, &chord, frames(0.1));
    assert_eq!(voices(&*sampler), 3.0);
    let all_off = NoteEvent {
        frame: 0,
        kind: NoteEventKind::AllNotesOff,
    };
    let out = run(&mut *sampler, &[all_off], frames(0.05));
    assert!(peak(&out) > 0.1, "a release tail, not a cut");
    // Three releasing, and three release triggers.
    assert_eq!(voices(&*sampler), 6.0);
    sampler.reset();
    assert_eq!(voices(&*sampler), 0.0);
    assert_eq!(peak(&run(&mut *sampler, &[], BLOCK)), 0.0);
}

#[test]
fn notes_start_at_their_frame() {
    let mut sampler = loaded("<region> sample=dc50.wav amp_veltrack=0 delay=0.001");
    let (mut left, mut right) = ([1.0; BLOCK], [1.0; BLOCK]);
    sampler.process(&[at(100, on(60, 100))], &mut left, &mut right);
    // Silent until the note and its 1 ms delay.
    assert_eq!(peak(&left[..148]), 0.0);
    assert_near(left[200], 0.5, 1e-4);
}
