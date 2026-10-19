//! How much of one CPU core the built-in effects need, on stereo noise at
//! 48 kHz in 64-frame blocks, at the defaults and at heavier settings.
//! Most limits are relative to the reverb measured in the same run:
//! absolute timings swing with the machine's power profile, the ratios
//! hold steadier. Run with `--release -- --nocapture` for meaningful
//! numbers; the limits are only checked in release builds.

mod common;

use common::{RELEASE, load, print_build, timing};

/// What one case measures: the effect, a description, its settings, and
/// whether a sidechain plays and the transport rolls.
struct Case<'a> {
    id: &'a str,
    what: &'a str,
    settings: &'a [(&'a str, f32)],
    sidechained: bool,
    playing: bool,
}

const fn case<'a>(id: &'a str, what: &'a str, settings: &'a [(&'a str, f32)]) -> Case<'a> {
    Case {
        id,
        what,
        settings,
        sidechained: false,
        playing: false,
    }
}

impl Case<'_> {
    fn sidechained(self) -> Self {
        Case {
            sidechained: true,
            ..self
        }
    }

    fn playing(self) -> Self {
        Case {
            playing: true,
            ..self
        }
    }

    fn load(&self) -> f64 {
        load(self.id, self.settings, self.sidechained, self.playing)
    }
}

/// Measures the reverb at its defaults, the reference the others are held
/// to, and prints it.
fn reverb() -> f64 {
    print_build();
    let reverb = load("canopy.reverb", &[], false, false);
    println!(
        "  {:<24} {:<34} {reverb:6.3}% of real time (reference)",
        "canopy.reverb", "defaults"
    );
    reverb
}

/// Measures every case and fails with every one that costs more than its
/// limit, in times the reverb.
fn check(cases: &[(Case, f64)], reverb: f64) {
    let mut over = Vec::new();
    for (case, limit) in cases {
        let percent = case.load();
        let ratio = percent / reverb;
        let (id, what) = (case.id, case.what);
        println!("  {id:<24} {what:<34} {percent:6.3}% of real time, {ratio:5.2}x the reverb");
        if RELEASE && ratio >= *limit {
            over.push(format!("{id} {what}: {ratio:.2}x the reverb"));
        }
    }
    assert!(over.is_empty(), "{over:?}");
}

#[test]
fn effects_use_little_cpu() {
    let _timing = timing();
    print_build();
    let bells = bells(6, false);
    let bells = named(&bells);
    let cases = [
        case("canopy.eq", "defaults (all bands flat)", &[]),
        case("canopy.eq", "six bells at +6 dB", &bells),
        case("canopy.compressor", "defaults, compressing", &[]),
        case("canopy.reverb", "defaults", &[]),
        case(
            "canopy.reverb",
            "size 100%, decay 20 s",
            &[("size", 100.0), ("decay", 20.0)],
        ),
        case("canopy.delay", "defaults (ping-pong)", &[]),
        case(
            "canopy.delay",
            "stereo, free 2 s, 95%",
            &[
                ("mode", 0.0),
                ("sync", 1.0),
                ("time", 2000.0),
                ("feedback", 95.0),
            ],
        ),
        case("canopy.utility", "defaults (untouched)", &[]),
        case(
            "canopy.utility",
            "every stage on",
            &[
                ("gain", -6.0),
                ("balance", 30.0),
                ("width", 150.0),
                ("bass_mono", 1.0),
                ("invert_left", 1.0),
                ("dc", 1.0),
                ("delay_side", 2.0),
            ],
        ),
        case("canopy.amp", "defaults (clean, 2x12)", &[]),
        case(
            "canopy.amp",
            "lead, gain 8, 4x12",
            &[("voicing", 2.0), ("gain", 8.0), ("cabinet", 3.0)],
        ),
        case("canopy.transient", "defaults (untouched)", &[]),
        case("canopy.transient", "every stage on", &TRANSIENT_ALL),
    ];
    for case in cases {
        let percent = case.load();
        let (id, what) = (case.id, case.what);
        println!("  {id:<24} {what:<34} {percent:6.3}% of real time");
        if RELEASE {
            let limit = if id == "canopy.reverb" { 5.0 } else { 2.0 };
            assert!(percent < limit, "{id} {what}: {percent}%");
        }
    }
}

#[test]
fn creative_effects_cost_a_few_reverbs_at_most() {
    let _timing = timing();
    let reverb = reverb();
    let cases = [
        (
            case("canopy.waveshaper", "defaults (soft clip, 4x)", &[]),
            3.0,
        ),
        (
            case(
                "canopy.waveshaper",
                "oversampling off",
                &[("oversampling", 0.0)],
            ),
            1.0,
        ),
        (
            case(
                "canopy.waveshaper",
                "tube, 8x",
                &[("shape", 2.0), ("oversampling", 3.0)],
            ),
            5.0,
        ),
        (
            case("canopy.shaper", "defaults (volume pump)", &[]).playing(),
            1.0,
        ),
        (
            case(
                "canopy.shaper",
                "volume, filter and pan",
                &[("flt_on", 1.0), ("pan_on", 1.0), ("vol_smooth", 0.0)],
            )
            .playing(),
            2.0,
        ),
        (
            case("canopy.chorus", "defaults (2 voices a side)", &[]),
            0.7,
        ),
        (
            case(
                "canopy.chorus",
                "4 voices, triangle, synced",
                &[("voices", 3.0), ("shape", 1.0), ("sync", 0.0)],
            )
            .playing(),
            1.2,
        ),
        (
            case(
                "canopy.chorus",
                "flanger, 70%",
                &[("mode", 1.0), ("feedback", 70.0), ("delay", 3.0)],
            ),
            0.7,
        ),
        (
            case(
                "canopy.chorus",
                "phaser, 12 stages",
                &[("mode", 2.0), ("stages", 5.0), ("feedback", 50.0)],
            ),
            1.2,
        ),
        (case("canopy.vocoder", "no sidechain (bypassed)", &[]), 0.2),
        (
            case("canopy.vocoder", "defaults (16 bands)", &[]).sidechained(),
            3.0,
        ),
        (
            case("canopy.vocoder", "32 bands", &[("bands", 12.0)]).sidechained(),
            5.0,
        ),
        (
            case("canopy.vocoder", "8 bands", &[("bands", 0.0)]).sidechained(),
            2.0,
        ),
        (
            case("canopy.analyzer", "everything measured", &[]).playing(),
            2.0,
        ),
        (case("canopy.filter", "defaults (low 24)", &[]), 0.5),
        (
            case(
                "canopy.filter",
                "ladder, driven",
                &[("type", 11.0), ("resonance", 80.0), ("drive", 12.0)],
            ),
            0.7,
        ),
        (
            case(
                "canopy.filter",
                "phaser",
                &[("type", 23.0), ("resonance", 60.0)],
            ),
            1.2,
        ),
        (case("canopy.filter", "formant", &[("type", 25.0)]), 0.7),
        (case("canopy.distortion", "defaults (tube, 4x)", &[]), 3.0),
        (
            case(
                "canopy.distortion",
                "sine fold, filter before, 8x",
                &[("mode", 6.0), ("filter", 1.0), ("oversampling", 3.0)],
            ),
            6.0,
        ),
        (
            case(
                "canopy.distortion",
                "downsample, no oversampling",
                &[("mode", 8.0), ("oversampling", 0.0)],
            ),
            1.0,
        ),
        (case("canopy.hyper", "defaults (5 voices)", &[]), 2.0),
        (
            case(
                "canopy.hyper",
                "7 voices, wide",
                &[("voices", 6.0), ("dimension", 100.0)],
            ),
            3.0,
        ),
        (case("canopy.frequency_shifter", "defaults", &[]), 0.7),
        (
            case(
                "canopy.frequency_shifter",
                "feedback, blur, ring",
                &[("feedback", 80.0), ("blur", 60.0), ("ring", 50.0)],
            ),
            1.0,
        ),
        (case("canopy.convolve", "defaults (hall, 3 s)", &[]), 4.0),
        (
            case("canopy.convolve", "longest (hall, 4 s)", &[("size", 100.0)]),
            5.0,
        ),
    ];
    check(&cases, reverb);
}

/// Settings for `bands` EQ bands as +6 dB bells spread over the spectrum,
/// dynamic if asked.
fn bells(bands: usize, dynamic: bool) -> Vec<(String, f32)> {
    (1..=bands)
        .flat_map(|band| {
            let freq = 40.0 * 400f32.powf((band - 1) as f32 / 23.0);
            let mut settings = vec![
                (format!("b{band}_on"), 1.0),
                (format!("b{band}_type"), 2.0),
                (format!("b{band}_gain"), 6.0),
                (format!("b{band}_freq"), freq),
            ];
            if dynamic {
                settings.push((format!("b{band}_dyn"), 1.0));
                settings.push((format!("b{band}_threshold"), -40.0));
                settings.push((format!("b{band}_range"), -6.0));
            }
            settings
        })
        .collect()
}

/// `settings` borrowed, as [`case`] takes them.
fn named(settings: &[(String, f32)]) -> Vec<(&str, f32)> {
    settings.iter().map(|(id, v)| (id.as_str(), *v)).collect()
}

/// The transient shaper with every stage at work.
const TRANSIENT_ALL: [(&str, f32); 7] = [
    ("attack", 60.0),
    ("sustain", -40.0),
    ("pump", 50.0),
    ("split", 1.0),
    ("output", 3.0),
    ("clip", 1.0),
    ("mix", 80.0),
];

#[test]
fn dynamics_cost_relative_to_the_reverb() {
    let _timing = timing();
    let reverb = reverb();
    let [six, many, dynamic] = [bells(6, false), bells(24, false), bells(24, true)];
    let (six, many, dynamic) = (named(&six), named(&many), named(&dynamic));
    let cases = [
        (case("canopy.eq", "defaults (bands flat)", &[]), 0.2),
        (case("canopy.eq", "6 bells", &six), 1.0),
        (case("canopy.eq", "24 bells", &many), 3.0),
        (case("canopy.eq", "24 dynamic bells", &dynamic), 5.0),
        (
            case("canopy.eq", "24 dynamic bells, sidechain", &dynamic).sidechained(),
            5.0,
        ),
        (case("canopy.compressor", "clean", &[("model", 0.0)]), 1.0),
        (case("canopy.compressor", "FET", &[("model", 1.0)]), 1.0),
        (case("canopy.compressor", "opto", &[("model", 2.0)]), 1.0),
        (case("canopy.compressor", "vari-mu", &[("model", 3.0)]), 1.0),
        (case("canopy.limiter", "true peak (default)", &[]), 1.5),
        (
            case("canopy.limiter", "sample peak", &[("true_peak", 0.0)]),
            1.0,
        ),
        (case("canopy.multiband", "defaults (OTT)", &[]), 2.0),
        (case("canopy.transient", "defaults (untouched)", &[]), 0.5),
        (
            case(
                "canopy.transient",
                "attack, sustain, pump",
                &TRANSIENT_ALL[..3],
            ),
            1.0,
        ),
        (
            case(
                "canopy.transient",
                "split, clip and all",
                &TRANSIENT_ALL[..6],
            ),
            1.0,
        ),
    ];
    check(&cases, reverb);
}

/// Most of the clipper's cost is the first oversampling stage's steep
/// filter on the way down (the same at every factor); 8x and 16x add many
/// short filters at the high rates. Driven 6 dB into the ceiling, so it
/// clips all the time.
#[test]
fn the_clipper_costs_about_a_reverb() {
    let _timing = timing();
    let reverb = reverb();
    const DRIVE: (&str, f32) = ("input", 6.0);
    fn clipper<'a>(what: &'a str, settings: &'a [(&'a str, f32)]) -> Case<'a> {
        case("canopy.clipper", what, settings)
    }
    let cases = [
        (case("canopy.limiter", "defaults", &[]), 1.5),
        (clipper("defaults (hard, 4x, true peak)", &[DRIVE]), 5.0),
        (
            clipper(
                "off, sample-peak safety",
                &[DRIVE, ("oversampling", 0.0), ("safety", 1.0)],
            ),
            1.5,
        ),
        (
            clipper("off, true-peak safety", &[DRIVE, ("oversampling", 0.0)]),
            2.0,
        ),
        (
            clipper("2x, true peak", &[DRIVE, ("oversampling", 1.0)]),
            4.0,
        ),
        (
            clipper("4x, sample-peak safety", &[DRIVE, ("safety", 1.0)]),
            4.0,
        ),
        (
            clipper("8x, true peak", &[DRIVE, ("oversampling", 3.0)]),
            7.0,
        ),
        (
            clipper("16x, true peak", &[DRIVE, ("oversampling", 4.0)]),
            10.0,
        ),
        (
            clipper(
                "tanh, 16x, true peak",
                &[DRIVE, ("oversampling", 4.0), ("shape", 2.0)],
            ),
            12.0,
        ),
        (clipper("4x, delta", &[DRIVE, ("delta", 1.0)]), 5.0),
    ];
    check(&cases, reverb);
}
