//! Every float parameter of every built-in effect, moved over its whole
//! range by a sine LFO as a synth's LFO or a macro will move it: through
//! [`Effect::modulate`] once per engine block of [`BLOCK`] frames, at 20 Hz
//! and at a slower 2.7 Hz, and through [`Effect::set_param`] (automation's
//! way in) at the slower rate. The rule in `lib.rs` ("Modulation") says
//! what effects promise; this checks it:
//!
//! - nothing but finite samples comes out;
//! - no clicks or stair steps (see [`steepness`]): moment by moment, the
//!   output steps from one sample to the next no further, for its level,
//!   than the same effect makes it held still anywhere in the sweep's
//!   range, give or take a share of the input's own steps;
//! - modulating costs little more CPU than holding still.
//!
//! The input is two low tones whose level swells slowly, so a step stands
//! out against their gentle curves; effects with a sidechain hear soft
//! kick-like bursts in it. Each effect is swept at its defaults and at the
//! settings [`CONFIGS`] lists, which switch on what the defaults leave out
//! (EQ bands, modes, stages), so every parameter is heard doing something.

mod common;

use std::time::Instant;

use effects::{Context, Effect, EffectDescriptor, builtins};
use instruments::{ParamDescriptor, ParameterKind};

const RATE: f32 = 48_000.0;
/// Frames in an engine block: the engine moves a modulated parameter once
/// per block, before it.
const BLOCK: usize = 64;
/// The LFOs: a fast one and a slow one; and how long each sweep runs (two
/// slow cycles and a bit).
const FAST_HZ: f64 = 20.0;
const SLOW_HZ: f64 = 2.7;
const SWEEP: usize = 36_000;
/// Samples left out at the start of every run, while a reverb or delay
/// fills and the first settings are taken.
const SETTLE: usize = 2_400;
/// Steepness is measured over windows this long...
const WINDOW: usize = 128;
/// ...against the level within this many windows either side: more than
/// a cycle of the lower tone.
const REACH: usize = 4;
/// Points across the range held still, to measure how steep the effect
/// makes the sound anyway, and how many windows either side of a moment
/// count as "then": an effect whose sound changes from moment to moment
/// (a flanger's sweep, a compressor's pumping) is as steep, held still, a
/// little earlier or later as the sweep is now.
const STILL_POINTS: usize = 9;
const STILL_REACH: usize = 16;
/// A sweep may be this many times as steep as the steepest still setting,
/// at the same moment: room for the pitch bends of moving delays, which
/// the delay and chorus cap well under this...
const STEEPNESS_FACTOR: f32 = 3.0;
/// ...plus the input's own steepness: what gain moving a block at a time
/// adds at most, far under a click's.
const INPUT_SHARE: f32 = 1.0;
/// Levels under this share of the input's peak count as this, so near
/// silence doesn't make every wiggle look huge.
const LEVEL_FLOOR: f32 = 0.02;

/// Settings each effect is also swept at, besides its defaults: modes,
/// bands and stages the defaults leave off.
const CONFIGS: &[(&str, &[(&str, f32)])] = &[
    (
        "canopy.eq",
        &[
            ("b1_on", 1.0),
            ("b1_type", 2.0),
            ("b1_gain", 9.0),
            ("b2_on", 1.0),
            ("b2_type", 0.0),
            ("b2_slope", 3.0),
            ("b3_on", 1.0),
            ("b3_type", 2.0),
            ("b3_dyn", 1.0),
            ("b3_threshold", -40.0),
            ("b3_range", -12.0),
            ("b4_on", 1.0),
            ("b4_type", 5.0),
            ("b4_freq", 3000.0),
        ],
    ),
    ("canopy.compressor", &[("threshold", -36.0), ("model", 1.0)]),
    ("canopy.compressor", &[("threshold", -36.0), ("model", 2.0)]),
    ("canopy.compressor", &[("threshold", -36.0), ("model", 3.0)]),
    ("canopy.ducker", &[("mode", 1.0)]),
    ("canopy.ducker", &[("mode", 2.0)]),
    (
        "canopy.transient",
        &[
            ("attack", 60.0),
            ("sustain", -40.0),
            ("pump", 50.0),
            ("split", 1.0),
            ("clip", 1.0),
        ],
    ),
    ("canopy.waveshaper", &[("shape", 3.0), ("bias", 20.0)]),
    ("canopy.waveshaper", &[("shape", 6.0)]),
    ("canopy.shaper", &[("flt_on", 1.0), ("pan_on", 1.0)]),
    ("canopy.chorus", &[("mode", 1.0), ("feedback", 60.0)]),
    ("canopy.chorus", &[("mode", 2.0), ("feedback", -60.0)]),
    ("canopy.delay", &[("sync", 1.0), ("mode", 0.0)]),
    (
        "canopy.utility",
        &[("bass_mono", 1.0), ("dc", 1.0), ("delay_side", 2.0)],
    ),
    ("canopy.filter", &[("type", 7.0)]),
    ("canopy.filter", &[("type", 8.0)]),
    ("canopy.filter", &[("type", 11.0), ("resonance", 80.0)]),
    ("canopy.filter", &[("type", 15.0), ("resonance", 70.0)]),
    ("canopy.filter", &[("type", 16.0), ("drive", 12.0)]),
    ("canopy.filter", &[("type", 19.0), ("resonance", 70.0)]),
    ("canopy.filter", &[("type", 22.0)]),
    ("canopy.filter", &[("type", 24.0), ("resonance", 60.0)]),
    ("canopy.filter", &[("type", 25.0)]),
    ("canopy.distortion", &[("mode", 6.0), ("filter", 1.0)]),
    ("canopy.distortion", &[("mode", 2.0), ("filter", 2.0)]),
    ("canopy.distortion", &[("mode", 8.0)]),
    (
        "canopy.distortion",
        &[("mode", 16.0), ("oversampling", 0.0)],
    ),
    ("canopy.hyper", &[("voices", 6.0), ("hyper_mix", 100.0)]),
    ("canopy.hyper", &[("voices", 1.0), ("dimension", 100.0)]),
    (
        "canopy.frequency_shifter",
        &[("feedback", 70.0), ("mix", 100.0), ("blur", 50.0)],
    ),
    (
        "canopy.frequency_shifter",
        &[("ring", 100.0), ("range", 3.0)],
    ),
    ("canopy.reverb", &[("type", 1.0), ("mix", 50.0)]),
    ("canopy.reverb", &[("type", 2.0), ("mix", 50.0)]),
    ("canopy.convolve", &[("impulse", 4.0), ("mix", 100.0)]),
];

fn is_float(param: &ParamDescriptor) -> bool {
    matches!(param.kind, ParameterKind::Float { .. })
}

/// Two tones, a fifth-ish apart, swelling slowly.
fn input(len: usize) -> (Vec<f32>, Vec<f32>) {
    let tone = |freq: f64, phase: f64, swell: f64| -> Vec<f32> {
        (0..len)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                let level = 0.8 + 0.2 * (std::f64::consts::TAU * swell * t).sin();
                (0.5 * level * (std::f64::consts::TAU * freq * t + phase).sin()) as f32
            })
            .collect()
    };
    (tone(110.0, 0.0, 1.1), tone(137.0, 0.5, 0.8))
}

/// A soft kick every 300 ms: a 55 Hz burst with a 3 ms rise and an 80 ms
/// decay.
fn kicks(len: usize) -> Vec<f32> {
    let every = (0.3 * RATE) as usize;
    (0..len)
        .map(|n| {
            let t = (n % every) as f32 / RATE;
            let rise = (t / 0.003).min(1.0);
            let rise = 0.5 - 0.5 * (std::f32::consts::PI * rise).cos();
            0.8 * rise * (-t / 0.08).exp() * (std::f32::consts::TAU * 55.0 * t).sin()
        })
        .collect()
}

/// How steep a stereo signal is, window by window: the largest step from
/// one sample to the next in each [`WINDOW`], as a share of the loudest
/// sample within [`REACH`] windows of it (at least `floor`).
///
/// A low tone moves a small share of its level from one sample to the
/// next; a click, or the stair of a parameter that jumps instead of
/// gliding, is a step of a size of its own. Dividing by the level around it
/// makes a quiet click count as much as a loud one, and lets each moment be
/// judged against the same moment of another run. A parameter gliding a
/// block at a time bends the waveform's slope at the block's edges, which
/// barely changes its steps, so fast modulation done right passes.
fn steepness(left: &[f32], right: &[f32], floor: f32) -> Vec<f32> {
    let windows = left.len() / WINDOW;
    let mut step = vec![0.0f32; windows];
    let mut level = vec![0.0f32; windows];
    for channel in [left, right] {
        for (w, (step, level)) in step.iter_mut().zip(&mut level).enumerate() {
            let span = &channel[w * WINDOW..(w + 1) * WINDOW];
            *level = span.iter().fold(*level, |m, s| m.max(s.abs()));
            let from = (w * WINDOW).max(1);
            for n in from..(w + 1) * WINDOW {
                *step = step.max((channel[n] - channel[n - 1]).abs());
            }
        }
    }
    (0..windows)
        .map(|w| {
            let near = level[w.saturating_sub(REACH)..(w + REACH + 1).min(windows)]
                .iter()
                .fold(floor, |m, &l| m.max(l));
            step[w] / near
        })
        .collect()
}

/// What one run made, and how long it took.
struct Run {
    finite: bool,
    left: Vec<f32>,
    right: Vec<f32>,
    seconds: f64,
}

/// Runs `len` samples of the input through `effect` in [`BLOCK`]-frame
/// blocks, the transport rolling, calling `modulate(effect, sample)`
/// before each block.
fn run(
    descriptor: &EffectDescriptor,
    effect: &mut dyn Effect,
    len: usize,
    mut modulate: impl FnMut(&mut dyn Effect, usize),
) -> Run {
    let (mut left, mut right) = input(len);
    let side = kicks(len);
    let per_sample = 120.0 / 60.0 / f64::from(RATE);
    let start = Instant::now();
    let mut at = 0;
    while at < len {
        let end = (at + BLOCK).min(len);
        modulate(effect, at);
        let context = Context {
            bpm: 120.0,
            beat: at as f64 * per_sample,
            playing: true,
            sidechain: descriptor
                .sidechain
                .then(|| (&side[at..end], &side[at..end])),
        };
        effect.process(&mut left[at..end], &mut right[at..end], &context);
        at = end;
    }
    let seconds = start.elapsed().as_secs_f64();
    let finite = left.iter().chain(&right).all(|s| s.is_finite());
    Run {
        finite,
        left,
        right,
        seconds,
    }
}

/// Where a sine LFO at `hz` starting at `phase` (cycles) has a parameter
/// `sample` samples in, from its bottom to its top on the knob's scale.
fn lfo(param: &ParamDescriptor, hz: f64, phase: f64, sample: usize) -> f32 {
    let t = sample as f64 / f64::from(RATE);
    let wave = (std::f64::consts::TAU * (hz * t + phase)).sin();
    param.from_normalized((0.5 + 0.5 * wave) as f32)
}

/// Every configuration to sweep: each built-in at its defaults, then what
/// [`CONFIGS`] adds.
fn configurations() -> Vec<(&'static EffectDescriptor, &'static [(&'static str, f32)])> {
    let mut all: Vec<_> = builtins().iter().map(|d| (d, &[][..])).collect();
    for &(id, settings) in CONFIGS {
        let descriptor = effects::find_builtin(id).unwrap_or_else(|| panic!("no {id}"));
        all.push((descriptor, settings));
    }
    all
}

/// How a sweep reaches the effect: through `modulate` (a modulator's way)
/// or `set_param` (a knob's, automation's), and how fast it goes.
#[derive(Debug, Clone, Copy)]
enum Sweep {
    Modulate(f64),
    Knob(f64),
}

impl Sweep {
    const ALL: [Sweep; 3] = [
        Sweep::Modulate(FAST_HZ),
        Sweep::Modulate(SLOW_HZ),
        Sweep::Knob(SLOW_HZ),
    ];

    fn hz(self) -> f64 {
        match self {
            Sweep::Modulate(hz) | Sweep::Knob(hz) => hz,
        }
    }

    fn apply(self, effect: &mut dyn Effect, index: usize, value: f32) {
        match self {
            Sweep::Modulate(_) => effect.modulate(index, value),
            Sweep::Knob(_) => effect.set_param(index, value),
        }
    }
}

#[test]
fn sweeping_any_parameter_never_clicks_or_breaks() {
    let (input_l, input_r) = input(SWEEP);
    let peak = input_l
        .iter()
        .chain(&input_r)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let floor = LEVEL_FLOOR * peak;
    let own = steepness(&input_l, &input_r, floor)
        .into_iter()
        .fold(0.0f32, f32::max);
    let first = SETTLE / WINDOW;
    let mut failures = Vec::new();
    let mut checked = 0;
    for (descriptor, settings) in configurations() {
        let mut held = common::effect(descriptor.id, RATE, 512, settings);
        let still = run(descriptor, held.as_mut(), SWEEP, |_, _| {});
        assert!(still.finite, "{} {settings:?}: non-finite", descriptor.id);
        let still_steepness = steepness(&still.left, &still.right, floor);
        for (index, param) in descriptor.params.iter().enumerate() {
            if !is_float(param) || settings.iter().any(|(id, _)| *id == param.id) {
                continue;
            }
            // How steep the effect is anyway, moment by moment, held still
            // anywhere in the range: worked out once it's needed.
            let mut reference: Option<Vec<f32>> = None;
            for sweep in Sweep::ALL {
                let what = format!("{} {settings:?}, {} {sweep:?}", descriptor.id, param.id);
                let mut effect = common::effect(descriptor.id, RATE, 512, settings);
                let swept = run(descriptor, effect.as_mut(), SWEEP, |effect, at| {
                    sweep.apply(effect, index, lfo(param, sweep.hz(), 0.0, at));
                });
                assert!(swept.finite, "{what}: non-finite output");
                if swept.left == still.left && swept.right == still.right {
                    // The parameter does nothing at these settings.
                    break;
                }
                checked += 1;
                let swept_steepness = steepness(&swept.left, &swept.right, floor);
                let worst = |reference: &[f32]| {
                    (first..swept_steepness.len())
                        .map(|w| {
                            let allowed = STEEPNESS_FACTOR * reference[w] + INPUT_SHARE * own;
                            (swept_steepness[w] / allowed, w)
                        })
                        .fold((0.0f32, 0), |a, b| if b.0 > a.0 { b } else { a })
                };
                if reference.is_none() && worst(&still_steepness).0 <= 1.0 {
                    continue;
                }
                // Steeper than at the defaults somewhere: compare with
                // every setting the sweep passes through, held still.
                let reference = reference.get_or_insert_with(|| {
                    let mut steepest = still_steepness.clone();
                    for point in 0..STILL_POINTS {
                        let value = param.from_normalized(point as f32 / (STILL_POINTS - 1) as f32);
                        let mut effect = common::effect(descriptor.id, RATE, 512, settings);
                        effect.set_param(index, value);
                        let held = run(descriptor, effect.as_mut(), SWEEP, |_, _| {});
                        for (r, h) in
                            steepest
                                .iter_mut()
                                .zip(steepness(&held.left, &held.right, floor))
                        {
                            *r = r.max(h);
                        }
                    }
                    (0..steepest.len())
                        .map(|w| {
                            let near = w.saturating_sub(STILL_REACH)
                                ..(w + STILL_REACH + 1).min(steepest.len());
                            steepest[near].iter().fold(0.0f32, |m, &s| m.max(s))
                        })
                        .collect()
                });
                let (ratio, w) = worst(reference);
                if ratio > 1.0 {
                    let at = w * WINDOW;
                    failures.push(format!(
                        "{what}: {ratio:.1}x too steep at {:.3} s ({} = {:.3}): {:.2e} against {:.2e} held still",
                        at as f32 / RATE,
                        param.id,
                        lfo(param, sweep.hz(), 0.0, at / BLOCK * BLOCK),
                        swept_steepness[w],
                        reference[w],
                    ));
                }
            }
        }
    }
    println!("{checked} sweeps; the input's own steepness {own:.2e}");
    assert!(
        failures.is_empty(),
        "clicks or stair steps:\n{}",
        failures.join("\n")
    );
}

/// How many times holding still moving every parameter may cost. The
/// convolver's impulse parameters remake its impulse while they move (spread
/// over blocks, so bounded) and it plays the old and the new impulse while
/// it fades between them: about 5% of a core at 48 kHz in a release build.
fn allowed_ratio(id: &str) -> f64 {
    match id {
        "canopy.convolve" => 6.0,
        _ => 3.0,
    }
}

#[test]
fn modulating_every_parameter_at_once_costs_little_more() {
    // Every float parameter on its own LFO, at rates spread from 0.5 to
    // 19 Hz so they never line up, through `modulate`, against the same
    // settings held still. Best of three, as the other timings.
    let release = !cfg!(debug_assertions);
    println!(
        "{} build: {SWEEP} samples, every float parameter modulated every {BLOCK} frames",
        if release { "release" } else { "debug" }
    );
    let mut failures = Vec::new();
    for (descriptor, settings) in configurations() {
        let floats: Vec<(usize, &ParamDescriptor)> = descriptor
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| is_float(p) && !settings.iter().any(|(id, _)| *id == p.id))
            .collect();
        let best = |modulated: bool| {
            (0..3)
                .map(|_| {
                    let mut effect = common::effect(descriptor.id, RATE, 512, settings);
                    let result = run(descriptor, effect.as_mut(), SWEEP, |effect, at| {
                        if !modulated {
                            return;
                        }
                        for (n, &(index, param)) in floats.iter().enumerate() {
                            let hz = 0.5 + 1.53 * (n % 13) as f64;
                            let phase = 0.618_034 * n as f64;
                            effect.modulate(index, lfo(param, hz, phase, at));
                        }
                    });
                    assert!(
                        result.finite,
                        "{} {settings:?}: non-finite output with everything moving",
                        descriptor.id
                    );
                    result.seconds
                })
                .fold(f64::INFINITY, f64::min)
        };
        let still = best(false);
        let moving = best(true);
        let audio = SWEEP as f64 / f64::from(RATE);
        let ratio = moving / still.max(1e-9);
        println!(
            "  {:<18} {:<40} still {:6.3}%, moving {:6.3}% of real time ({ratio:4.2}x)",
            descriptor.id,
            format!("{settings:?}").chars().take(40).collect::<String>(),
            100.0 * still / audio,
            100.0 * moving / audio,
        );
        // Moving costs at most a few times what holding still does, or a
        // sliver of a core for effects that cost next to nothing still.
        if release && ratio > allowed_ratio(descriptor.id) && moving / audio > 0.005 {
            failures.push(format!(
                "{} {settings:?}: {ratio:.2}x when moving",
                descriptor.id
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
