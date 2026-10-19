//! The built-in effects pushed to the edges: extreme and garbage parameter
//! values, odd sample rates and block sizes, loud and garbage input,
//! strange tempos and song positions, sidechains coming and going. Nothing
//! may panic or put out NaN or infinity, every meter stays finite, and
//! everything falls silent again when the input does.
//!
//! Every effect goes through the shared cases; the creative effects, the
//! dynamics and the clipper then through what only they have.

mod common;

use common::{CREATIVE, DYNAMICS, Random, effect, loudest, noise, range, run_checked};
use effects::{Context, MAX_METERS, builtins};

const ANY: f32 = f32::INFINITY;

#[test]
fn extreme_settings_never_break_an_effect() {
    let garbage = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1e30,
        -1e30,
        f32::MIN_POSITIVE,
    ];
    let loud = noise(4096, 3, 4.0);
    let plain = Context::DEFAULT;
    for descriptor in builtins() {
        for (sample_rate, block) in [
            (8_000.0, 7),
            (44_100.0, 64),
            (48_000.0, 1),
            (192_000.0, 512),
        ] {
            let what = format!("{} at {sample_rate} Hz, blocks of {block}", descriptor.id);
            let mut effect = effect(descriptor.id, sample_rate, 512, &[]);
            let effect = effect.as_mut();
            let params = effect.params();

            // Each parameter through its extremes and some garbage while loud
            // noise plays.
            for (index, param) in params.iter().enumerate() {
                let (min, max) = range(param);
                for &value in [min, max]
                    .iter()
                    .chain(&garbage)
                    .chain([&param.default_value()])
                {
                    effect.set_param(index, value);
                    run_checked(effect, &loud[..512], None, block, &plain, ANY, &what);
                }
            }

            // Everything at its minimum, then everything at its maximum.
            for pick in [0, 1] {
                for (index, param) in params.iter().enumerate() {
                    let (min, max) = range(param);
                    effect.set_param(index, if pick == 0 { min } else { max });
                }
                run_checked(effect, &loud, None, block, &plain, ANY, &what);
            }

            // Random settings, changed every block, at strange tempos.
            let mut random = Random::new(0x1234_5677);
            for round in 0..64 {
                for (index, param) in params.iter().enumerate() {
                    effect.set_param(index, random.value(param));
                }
                let bpm = [120.0, 0.0, -5.0, f64::NAN, 1e9, 1.0, 20.0][round % 7];
                let context = Context { bpm, ..plain };
                run_checked(effect, &loud[..256], None, block, &context, ANY, &what);
            }

            // Back to the defaults, a sample-rate change, then silence.
            for (index, param) in params.iter().enumerate() {
                effect.set_param(index, param.default_value());
            }
            effect.prepare(sample_rate, 512);
            run_checked(effect, &loud, None, block, &plain, ANY, &what);
            for (index, param) in params.iter().enumerate() {
                // Long tails: every parameter at its top except the mix, so
                // the tail is heard, and the delay's feedback, whose 95% on
                // a 2 s delay rightly rings for minutes.
                if param.id != "mix" && param.id != "feedback" {
                    effect.set_param(index, range(param).1);
                }
            }
            run_checked(effect, &loud, None, block, &plain, ANY, &what);
            // Long enough for a 20 s reverb to fall 180 dB.
            let mut tail = 0.0;
            for _ in 0..(sample_rate as usize * 60 / 1024) {
                let (left, right) =
                    run_checked(effect, &[0.0; 1024], None, 1024, &plain, ANY, &what);
                tail = loudest(&left, &right);
            }
            assert!(
                tail < 1e-5,
                "{what}: still {tail} after a minute of silence"
            );
        }
    }
}

#[test]
fn reset_silences_everything() {
    let loud = noise(48_000, 9, 1.0);
    for descriptor in builtins() {
        let mut effect = effect(descriptor.id, 48_000.0, 64, &[]);
        if effect.params().iter().any(|p| p.id == "mix") {
            common::set(effect.as_mut(), "mix", 100.0);
        }
        run_checked(
            effect.as_mut(),
            &loud,
            None,
            64,
            &Context::DEFAULT,
            ANY,
            descriptor.id,
        );
        effect.reset();
        assert_eq!(effect.meter(), 0.0, "{}", descriptor.id);
        let (mut left, mut right) = (vec![0.0; 4096], vec![0.0; 4096]);
        effect.process(&mut left, &mut right, &Context::DEFAULT);
        assert!(
            left.iter().chain(&right).all(|&s| s == 0.0),
            "{} rings on after reset",
            descriptor.id
        );
    }
}

/// A loud sidechain (the vocoder only works with one), the transport
/// rolling with the song position jumping, garbage tempos and song
/// positions, extreme and random settings.
#[test]
fn creative_effects_survive_sidechains_transport_and_garbage() {
    let garbage = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30];
    let loud = noise(4096, 5, 4.0);
    let side = noise(4096, 6, 4.0);
    let playing = Context {
        bpm: 140.0,
        beat: 3.25,
        playing: true,
        sidechain: None,
    };
    for id in CREATIVE {
        for (sample_rate, block) in [(8_000.0, 7), (44_100.0, 64), (96_000.0, 333)] {
            let what = format!("{id} at {sample_rate} Hz, blocks of {block}");
            let mut effect = effect(id, sample_rate, 512, &[]);
            let effect = effect.as_mut();
            let params = effect.params();

            // Each parameter through its extremes and garbage, sidechained
            // and playing.
            for (index, param) in params.iter().enumerate() {
                let (min, max) = range(param);
                for &value in [min, max].iter().chain(&garbage) {
                    effect.set_param(index, value);
                    run_checked(
                        effect,
                        &loud[..256],
                        Some(&side[..256]),
                        block,
                        &playing,
                        ANY,
                        &what,
                    );
                }
                effect.set_param(index, param.default_value());
            }

            // Random settings every block, with garbage tempos and song
            // positions, the transport starting and stopping, the
            // sidechain coming and going.
            let mut random = Random::new(0x0bad_5eed);
            for round in 0..48 {
                for (index, param) in params.iter().enumerate() {
                    effect.set_param(index, random.value(param));
                }
                let context = Context {
                    bpm: [120.0, 0.0, -5.0, f64::NAN, 1e9, 1.0, 999.0][round % 7],
                    beat: [0.0, 1e12, -3.0, f64::NAN, f64::INFINITY, 17.5][round % 6],
                    playing: round % 3 != 0,
                    sidechain: None,
                };
                let side = (round % 4 != 0).then_some(&side[..256]);
                run_checked(effect, &loud[..256], side, block, &context, ANY, &what);
            }

            // Everything at its top, then silence everywhere: it must die
            // away. Feedback goes back to its default: at its top on the
            // frequency shifter's 2 s delay it rightly rings for minutes.
            for (index, param) in params.iter().enumerate() {
                match param.id {
                    "mix" => {}
                    "feedback" => effect.set_param(index, param.default_value()),
                    _ => effect.set_param(index, range(param).1),
                }
            }
            run_checked(effect, &loud, Some(&side), block, &playing, ANY, &what);
            let silence = vec![0.0; sample_rate as usize * 10];
            let (left, right) = run_checked(
                effect,
                &silence,
                Some(&silence),
                block,
                &playing,
                ANY,
                &what,
            );
            let tail = loudest(&left[left.len() - 1024..], &right[right.len() - 1024..]);
            assert!(tail < 1e-5, "{what}: still {tail} after 10 s of silence");
        }
    }
}

#[test]
fn the_analyzer_never_changes_a_sample() {
    let mut analyzer = effect("canopy.analyzer", 44_100.0, 512, &[]);
    let input = noise(44_100, 9, 1.5);
    for playing in [false, true, false, true] {
        let context = Context {
            playing,
            ..Context::DEFAULT
        };
        let mut left = input.clone();
        let mut right: Vec<f32> = input.iter().map(|s| -0.3 * s).collect();
        let expected_right = right.clone();
        for (l, r) in left.chunks_mut(100).zip(right.chunks_mut(100)) {
            analyzer.process(l, r, &context);
        }
        assert_eq!(left, input);
        assert_eq!(right, expected_right);
    }
}

/// Whether the dynamics effect's output and meters are all finite.
fn all_finite(effect: &dyn effects::Effect, left: &[f32], right: &[f32]) -> bool {
    let mut meters = [0.0; MAX_METERS];
    effect.meters(&mut meters);
    left.iter()
        .chain(right)
        .chain(&meters)
        .all(|s| s.is_finite())
}

#[test]
fn dynamics_put_out_silence_for_silence_at_any_setting_with_a_sidechain() {
    for id in DYNAMICS {
        for seed in 1..20 {
            let mut effect = effect(id, 48_000.0, 64, &[]);
            let mut random = Random::new(seed);
            for (index, param) in effect.params().iter().enumerate() {
                effect.set_param(index, random.value(param));
            }
            // A silent sidechain too: nothing to hear, nothing to do.
            let quiet = vec![0.0f32; 512];
            let context = Context {
                sidechain: Some((&quiet, &quiet)),
                ..Context::DEFAULT
            };
            let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
            for _ in 0..20 {
                effect.process(&mut left, &mut right, &context);
            }
            assert!(
                left.iter().chain(&right).all(|&s| s == 0.0),
                "{id}, seed {seed}: sound from silence"
            );
            assert!(all_finite(effect.as_ref(), &left, &right), "{id}");
        }
    }
}

#[test]
fn dynamics_stay_finite_with_garbage_sidechains_and_extreme_inputs() {
    let garbage = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1e30,
        -1e30,
        1e-40,
    ];
    for id in DYNAMICS {
        let mut effect = effect(id, 48_000.0, 64, &[]);
        let params = effect.params();
        // Everything at its top, then everything at its bottom, then random.
        let mut random = Random::new(7);
        for round in 0..30 {
            for (index, param) in params.iter().enumerate() {
                let value = match round {
                    0 => range(param).1,
                    1 => range(param).0,
                    _ => random.value(param),
                };
                effect.set_param(index, value);
            }
            let loud: Vec<f32> = (0..512).map(|_| 16.0 * random.bipolar()).collect();
            let side: Vec<f32> = (0..512)
                .map(|n| {
                    if n % 7 == 0 {
                        garbage[n % garbage.len()]
                    } else {
                        loud[n]
                    }
                })
                .collect();
            let context = Context {
                sidechain: Some((&side, &loud)),
                ..Context::DEFAULT
            };
            let (mut left, mut right) = (loud.clone(), loud.clone());
            effect.process(&mut left, &mut right, &context);
            assert!(
                all_finite(effect.as_ref(), &left, &right),
                "{id}, round {round}"
            );
        }
    }
}

/// The largest jump between neighbouring output samples of the dynamics
/// effect `id`, relative to the loudest sample around it, while parameters
/// move at random. A 200 Hz sine moves by at most 2.6% of its peak per
/// sample; a click jumps far more.
fn largest_relative_step(id: &str, seed: u32) -> f32 {
    const RATE: f32 = 48_000.0;
    let mut effect = effect(id, RATE, 64, &[]);
    let params = effect.params();
    let mut random = Random::new(seed);
    let len = RATE as usize * 4;
    let step = std::f64::consts::TAU * 200.0 / f64::from(RATE);
    let input: Vec<f32> = (0..len)
        .map(|n| 0.25 * (step * n as f64).sin() as f32)
        .collect();
    let (mut left, mut right) = (input.clone(), input);
    let mut moves = Vec::new();
    // One parameter moves every 25 ms, as fast as a hand on a knob.
    for (block, (l, r)) in left
        .chunks_mut(1200)
        .zip(right.chunks_mut(1200))
        .enumerate()
    {
        if block > 0 {
            let index = random.index(params.len());
            let value = random.value(&params[index]);
            effect.set_param(index, value);
            moves.push((params[index].id, value));
        }
        for (l, r) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
            effect.process(l, r, &Context::DEFAULT);
        }
    }
    let mut worst = (0.0f32, 0);
    // After the first block: the start is a sine starting from silence.
    // Only where the sound is within 40 dB of the input: further down (a
    // band turned down 60 dB, the filters' own whisper lifted by upward
    // compression) nothing is audible enough to click.
    for n in 1200..len - 1 {
        let around = &left[n.saturating_sub(480)..(n + 480).min(len)];
        let local = loudest(around, &[]);
        if local > 0.0025 {
            let relative = (left[n + 1] - left[n]).abs() / local;
            if relative > worst.0 {
                worst = (relative, n);
            }
        }
    }
    let block = worst.1 / 1200;
    if worst.0 > 0.1 {
        println!(
            "{id} seed {seed}: step {} at {} ({:?}), after {:?}",
            worst.0,
            worst.1,
            &left[worst.1 - 3..worst.1 + 4],
            &moves[block.saturating_sub(3)..block]
        );
    }
    worst.0
}

#[test]
fn moving_dynamics_parameters_never_clicks() {
    for id in DYNAMICS {
        let worst = (1..6)
            .map(|seed| largest_relative_step(id, seed))
            .fold(0.0f32, f32::max);
        println!("{id}: largest step {:.1}% of the local peak", 100.0 * worst);
        assert!(worst < 0.1, "{id}: {worst}");
    }
}

/// The clipper with garbage in its input (NaN, infinities, huge values):
/// a safety keeps every sample under the ceiling whatever comes in, and
/// silence in is exact silence out once its filters have emptied.
#[test]
fn clipper_garbage_in_stays_under_the_ceiling_and_dies_away() {
    let mut input = noise(4096, 3, 4.0);
    for (i, s) in input.iter_mut().enumerate() {
        *s = match i % 97 {
            0 => f32::NAN,
            11 => f32::INFINITY,
            23 => f32::NEG_INFINITY,
            37 => 1e30,
            51 => -1e30,
            _ => *s,
        };
    }
    let limit = dsp::db_to_gain(-3.0);
    for (rate, block) in [
        (8_000.0, 7),
        (44_100.0, 64),
        (96_000.0, 333),
        (192_000.0, 512),
    ] {
        for oversampling in 0..=4 {
            for safety in [1.0, 2.0] {
                for shape in [0.0, 1.0, 2.0] {
                    let settings = [
                        ("input", 24.0),
                        ("ceiling", -3.0),
                        ("oversampling", oversampling as f32),
                        ("safety", safety),
                        ("shape", shape),
                    ];
                    let what = format!("{settings:?} at {rate} Hz, blocks of {block}");
                    let mut effect = effect("canopy.clipper", rate, 512, &settings);
                    let context = Context::DEFAULT;
                    run_checked(effect.as_mut(), &input, None, block, &context, limit, &what);
                    let silence = vec![0.0; 2048];
                    let (left, right) = run_checked(
                        effect.as_mut(),
                        &silence,
                        None,
                        block,
                        &context,
                        limit,
                        &what,
                    );
                    assert!(
                        left[1024..].iter().chain(&right[1024..]).all(|&s| s == 0.0),
                        "{what}: still sounding after silence"
                    );
                }
            }
        }
    }
}

#[test]
fn clipper_silence_is_exact_and_dc_passes_or_clips() {
    let context = Context::DEFAULT;
    for oversampling in 0..=4 {
        let settings = [("oversampling", oversampling as f32)];
        let clipper = || effect("canopy.clipper", 48_000.0, 512, &settings);
        let (left, _) = run_checked(
            clipper().as_mut(),
            &[0.0; 4096],
            None,
            64,
            &context,
            0.0,
            "silence",
        );
        assert!(left.iter().all(|&s| s == 0.0));

        // DC under the ceiling comes through at its level.
        let (left, right) = run_checked(
            clipper().as_mut(),
            &[0.5; 4096],
            None,
            64,
            &context,
            1.0,
            "DC",
        );
        for s in left[2048..].iter().chain(&right[2048..]) {
            assert!((s - 0.5).abs() < 1e-5, "{oversampling}: DC {s}");
        }

        // DC over it settles just under the ceiling, never over.
        let ceiling = dsp::db_to_gain(-0.3);
        let (left, _) = run_checked(
            clipper().as_mut(),
            &[2.0; 8192],
            None,
            64,
            &context,
            ceiling,
            "loud DC",
        );
        let settled = left[8000];
        assert!(
            settled <= ceiling && settled > dsp::db_to_gain(-0.4),
            "{oversampling}: {settled}"
        );
    }
}

#[test]
fn clipper_takes_random_settings_every_block() {
    let context = Context::DEFAULT;
    for (rate, block) in [(22_050.0, 31), (48_000.0, 64), (88_200.0, 200)] {
        let mut effect = effect("canopy.clipper", rate, 256, &[]);
        let params = effect.params();
        let mut random = Random::new(0x0bad_5eed);
        let loud = noise(256, 9, 3.0);
        for _ in 0..200 {
            for (index, param) in params.iter().enumerate() {
                effect.set_param(index, random.value(param));
            }
            // Delta plays what was taken off the driven sound, up to 24 dB
            // over this +9.5 dBFS noise; nothing else gets near that.
            run_checked(
                effect.as_mut(),
                &loud,
                None,
                block,
                &context,
                50.0,
                "random settings",
            );
        }
        // Settings for silence: safety on, no delta.
        common::set(effect.as_mut(), "safety", 2.0);
        common::set(effect.as_mut(), "delta", 0.0);
        let (left, right) = run_checked(
            effect.as_mut(),
            &[0.0; 8192],
            None,
            block,
            &context,
            8.0,
            "after random",
        );
        assert!(
            left[4096..].iter().chain(&right[4096..]).all(|&s| s == 0.0),
            "falls silent"
        );
    }
}
