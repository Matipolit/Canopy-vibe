//! Measures the Drum Synth: which notes play what, pitch and its envelope,
//! choke groups, velocity, decay times, where each pad's energy sits in the
//! spectrum, what each layer setting does (sources, modes, FM, bursts,
//! filters and their envelopes), and that nothing breaks at extreme
//! settings.

mod common;

use common::{max_step, peak, rms, seconds};
use instruments::builtin::drums::{
    self, FilterKind, LAYERS, Layer, Mode, Source, Voice, render_hit, voices,
};
use instruments::{Instrument, MAX_METERS, ParamDescriptor, ParameterKind};
use midi::{NoteEvent, NoteEventKind, Pitch, Velocity};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 64;

fn kit() -> Box<dyn Instrument> {
    let mut kit = (drums::DESCRIPTOR.create)();
    kit.prepare(RATE, BLOCK);
    kit
}

fn index(id: &str) -> usize {
    drums::DESCRIPTOR
        .params
        .iter()
        .position(|p| p.id == id)
        .unwrap_or_else(|| panic!("no parameter {id}"))
}

fn set(kit: &mut dyn Instrument, id: &str, value: f32) {
    kit.set_param(index(id), value);
}

fn slot(name: &str) -> usize {
    voices()
        .iter()
        .position(|v| v.name == name)
        .unwrap_or_else(|| panic!("no voice {name}"))
}

fn voice(name: &str) -> &'static Voice {
    &voices()[slot(name)]
}

fn default(index: usize) -> f32 {
    drums::DESCRIPTOR.params[index].default_value()
}

/// Settings that leave only `keep` of `voice`'s layers on.
fn only(voice: &Voice, keep: &[usize]) -> Vec<(usize, f32)> {
    (0..LAYERS)
        .filter(|layer| !keep.contains(layer))
        .map(|layer| (voice.layer_param(layer, Layer::MODE), 0.0))
        .collect()
}

fn hit(note: u8, velocity: f32) -> NoteEventKind {
    NoteEventKind::On {
        pitch: Pitch::new(note).unwrap(),
        velocity: Velocity::new((velocity * 65535.0).round() as u16),
    }
}

struct Render {
    left: Vec<f32>,
    right: Vec<f32>,
    /// The meters after each block.
    meters: Vec<[f32; MAX_METERS]>,
}

impl Render {
    fn mono(&self) -> Vec<f32> {
        self.left
            .iter()
            .zip(&self.right)
            .map(|(l, r)| 0.5 * (l + r))
            .collect()
    }

    fn peak(&self) -> f32 {
        peak(&self.left).max(peak(&self.right))
    }

    /// The first block's meters, after the note at frame 0.
    fn first_meters(&self) -> [f32; MAX_METERS] {
        self.meters[0]
    }

    /// When voice `slot`'s meter falls below `fraction` of its highest for
    /// good, in seconds.
    fn meter_fall(&self, slot: usize, fraction: f32) -> f32 {
        let top = self.meters.iter().map(|m| m[slot]).fold(0.0, f32::max);
        let last = self.meters.iter().rposition(|m| m[slot] >= top * fraction);
        (last.map_or(0, |b| b + 1) + 1) as f32 * BLOCK as f32 / RATE
    }
}

/// Renders `seconds` of output, with `events` at the given sample offsets.
fn render(kit: &mut dyn Instrument, events: &[(usize, NoteEventKind)], seconds: f32) -> Render {
    let blocks = (seconds * RATE / BLOCK as f32).ceil() as usize;
    let mut out = Render {
        left: Vec::with_capacity(blocks * BLOCK),
        right: Vec::with_capacity(blocks * BLOCK),
        meters: Vec::with_capacity(blocks),
    };
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    for block in 0..blocks {
        let start = block * BLOCK;
        let block_events: Vec<NoteEvent> = events
            .iter()
            .filter(|(at, _)| (start..start + BLOCK).contains(at))
            .map(|&(at, kind)| NoteEvent {
                frame: (at - start) as u32,
                kind,
            })
            .collect();
        kit.process(&block_events, &mut left, &mut right);
        assert!(
            left.iter().chain(&right).all(|s| s.is_finite()),
            "not finite in block {block}"
        );
        out.left.extend_from_slice(&left);
        out.right.extend_from_slice(&right);
        let mut meters = [0.0; MAX_METERS];
        kit.meters(&mut meters);
        out.meters.push(meters);
    }
    out
}

/// A kit with `settings` (parameter index, value) applied, and `voice`'s
/// drive off so the shape is its layers' own.
fn kit_with(voice: &Voice, settings: &[(usize, f32)]) -> Box<dyn Instrument> {
    let mut kit = kit();
    kit.set_param(voice.param(Voice::DRIVE), 0.0);
    for &(index, value) in settings {
        kit.set_param(index, value);
    }
    kit
}

/// One hit of `voice`'s main note at full velocity, with its drive off,
/// and `settings` as (parameter index, value).
fn solo(voice: &Voice, seconds: f32, settings: &[(usize, f32)]) -> Render {
    let mut kit = kit_with(voice, settings);
    render(&mut *kit, &[(0, hit(voice.notes[0], 1.0))], seconds)
}

/// In-place radix-2 FFT; `re.len()` must be a power of two.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (sin, cos) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (br, bi) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                re[b] = re[a] - br;
                im[b] = im[a] - bi;
                re[a] += br;
                im[a] += bi;
            }
        }
        len <<= 1;
    }
}

/// Energy per bin up to Nyquist, and the bin width in Hz. The signal is
/// zero-padded to a power of two; a hit that has died away by the end of
/// it needs no window.
fn spectrum(signal: &[f32]) -> (Vec<f64>, f64) {
    let n = signal.len().next_power_of_two();
    let mut re: Vec<f64> = signal.iter().map(|&s| f64::from(s)).collect();
    re.resize(n, 0.0);
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im);
    let power = (0..n / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect();
    (power, f64::from(RATE) / n as f64)
}

/// The share of `signal`'s energy between `low` and `high` Hz.
fn band_share(signal: &[f32], low: f64, high: f64) -> f64 {
    let (power, width) = spectrum(signal);
    let total: f64 = power.iter().sum();
    let band: f64 = power
        .iter()
        .enumerate()
        .filter(|(k, _)| (low..high).contains(&(*k as f64 * width)))
        .map(|(_, p)| p)
        .sum();
    band / total.max(1e-30)
}

/// The energy-weighted mean frequency, in Hz.
fn centroid(signal: &[f32]) -> f64 {
    let (power, width) = spectrum(signal);
    let total: f64 = power.iter().sum();
    let weighted: f64 = power
        .iter()
        .enumerate()
        .map(|(k, p)| k as f64 * width * p)
        .sum();
    weighted / total.max(1e-30)
}

/// Zero crossings, in seconds, interpolated between samples.
fn crossings(signal: &[f32]) -> Vec<f32> {
    signal
        .windows(2)
        .enumerate()
        .filter(|(_, w)| (w[0] < 0.0) != (w[1] < 0.0))
        .map(|(i, w)| (i as f32 + w[0] / (w[0] - w[1])) / RATE)
        .collect()
}

/// The frequency over time: for each half cycle, its middle (in seconds)
/// and the frequency it implies.
fn half_cycles(signal: &[f32]) -> Vec<(f32, f32)> {
    crossings(signal)
        .windows(2)
        .map(|w| (0.5 * (w[0] + w[1]), 0.5 / (w[1] - w[0])))
        .collect()
}

/// The frequency of the half cycle around `seconds`.
fn frequency_at(cycles: &[(f32, f32)], seconds: f32) -> f32 {
    cycles
        .iter()
        .min_by(|a, b| (a.0 - seconds).abs().total_cmp(&(b.0 - seconds).abs()))
        .map_or(0.0, |c| c.1)
}

/// The mean frequency between `from` and `to` seconds, from the time
/// between the first and last zero crossing in that span.
fn frequency_between(signal: &[f32], from: f32, to: f32) -> f32 {
    let inside: Vec<f32> = crossings(signal)
        .into_iter()
        .filter(|t| (from..to).contains(t))
        .collect();
    let (first, last) = (inside[0], inside[inside.len() - 1]);
    (inside.len() - 1) as f32 / 2.0 / (last - first)
}

/// When `signal` last rises above `db` below its loudest, in 10 ms windows
/// of RMS: how long it rings.
fn ring_time(signal: &[f32], db: f32) -> f32 {
    let window = seconds(0.01);
    let levels: Vec<f32> = signal.chunks(window).map(rms).collect();
    let top = levels.iter().copied().fold(0.0, f32::max);
    let threshold = top * 10f32.powf(-db / 20.0);
    let last = levels.iter().rposition(|&l| l > threshold).unwrap_or(0);
    (last + 1) as f32 * 0.01
}

fn range(param: &ParamDescriptor) -> [f32; 2] {
    match param.kind {
        ParameterKind::Float { min, max, .. } => [min, max],
        ParameterKind::Enum { labels, .. } => [0.0, (labels.len() - 1) as f32],
    }
}

#[test]
fn parameters_are_laid_out_voice_by_voice_and_layer_by_layer() {
    let params = drums::DESCRIPTOR.params;
    assert_eq!(drums::KIT_PARAMS, 0..3);
    for (index, id) in [
        (drums::KIT_LEVEL, "kit_level"),
        (drums::KIT_TUNE, "kit_tune"),
        (drums::KIT_DRIVE, "kit_drive"),
    ] {
        assert_eq!(params[index].id, id);
    }

    let own = [
        (Voice::TUNE, "Tune", "_transpose"),
        (Voice::LENGTH, "Length", "_length"),
        (Voice::LEVEL, "Level", "_level"),
        (Voice::PAN, "Pan", "_pan"),
        (Voice::DRIVE, "Drive", "_drive"),
        (Voice::VELOCITY, "Velocity", "_velocity"),
        (Voice::CHOKE, "Choke", "_choke"),
        (Voice::FILTER, "Filter", "_filter"),
        (Voice::CUTOFF, "Cutoff", "_cutoff"),
        (Voice::RESONANCE, "Resonance", "_resonance"),
        (Voice::VARY, "Vary Hits", "_vary"),
    ];
    assert_eq!(own.len(), Voice::COMMON);
    let layered = [
        (Layer::MODE, "_mode"),
        (Layer::SOURCE, "_source"),
        (Layer::LEVEL, "_level"),
        (Layer::PAN, "_pan"),
        (Layer::VELOCITY, "_velocity"),
        (Layer::PITCH, "_pitch"),
        (Layer::SHAPE, "_shape"),
        (Layer::PHASE, "_phase"),
        (Layer::FM, "_fm"),
        (Layer::PITCH_ENV, "_pitch_env"),
        (Layer::PITCH_TIME, "_pitch_time"),
        (Layer::DELAY, "_delay"),
        (Layer::ATTACK, "_attack"),
        (Layer::HOLD, "_hold"),
        (Layer::DECAY, "_decay"),
        (Layer::CURVE, "_curve"),
        (Layer::BURSTS, "_bursts"),
        (Layer::GAP, "_gap"),
        (Layer::FILTER, "_filter"),
        (Layer::CUTOFF, "_cutoff"),
        (Layer::RESONANCE, "_resonance"),
        (Layer::FILTER_ENV, "_filter_env"),
        (Layer::FILTER_TIME, "_filter_time"),
        (Layer::TRACK, "_track"),
        (Layer::DRIVE, "_drive"),
    ];
    assert_eq!(layered.len(), Layer::COUNT);
    let mut next = drums::KIT_PARAMS.end;
    for voice in voices() {
        assert_eq!(voice.params.start, next, "{} follows the last", voice.name);
        assert_eq!(voice.params.len(), Voice::COMMON + LAYERS * Layer::COUNT);
        next = voice.params.end;
        for (offset, label, suffix) in own {
            let param = &params[voice.param(offset)];
            assert_eq!(voice.label(param), label, "{}", param.name);
            assert!(param.id.ends_with(suffix), "{}", param.id);
        }
        for layer in 0..LAYERS {
            assert_eq!(voice.layer(layer).start, voice.layer_param(layer, 0));
            for (offset, suffix) in layered {
                let param = &params[voice.layer_param(layer, offset)];
                let prefix = format!("_l{}{suffix}", layer + 1);
                assert!(param.id.ends_with(&prefix), "{}", param.id);
                let name = format!("{} Layer {} ", voice.name, layer + 1);
                assert!(param.name.starts_with(&name), "{}", param.name);
            }
        }
        for param in &params[voice.params.clone()] {
            let rest = param.name.strip_prefix(voice.name);
            assert!(
                rest.is_some_and(|r| r.starts_with(' ') && r.len() > 1),
                "{} starts with its voice's name",
                param.name
            );
        }
        assert!(!voice.notes.is_empty());
        // Every pad sounds out of the box.
        let on = (0..LAYERS)
            .filter(|&l| default(voice.layer_param(l, Layer::MODE)) == Mode::On as usize as f32)
            .count();
        assert!(on > 0, "{} has a layer on", voice.name);
    }
    assert_eq!(next, params.len(), "the voices cover every other parameter");
    assert!(voices().len() <= MAX_METERS);

    // Automation lists parameters by name, and project files by id.
    let mut ids: Vec<&str> = params.iter().map(|p| p.id).collect();
    let mut names: Vec<&str> = params.iter().map(|p| p.name).collect();
    ids.sort_unstable();
    names.sort_unstable();
    assert!(ids.windows(2).all(|w| w[0] != w[1]), "ids are unique");
    assert!(names.windows(2).all(|w| w[0] != w[1]), "names are unique");
    for param in params {
        if let ParameterKind::Float {
            min,
            max,
            default,
            logarithmic,
            ..
        } = param.kind
        {
            assert!(min < max && (min..=max).contains(&default), "{}", param.id);
            // Times may start at zero: their log scale reaches it.
            assert!(!logarithmic || min >= 0.0, "{}", param.id);
        }
    }

    // The hats share a choke group, and nothing else is in one.
    for (i, voice) in voices().iter().enumerate() {
        let group = default(voice.param(Voice::CHOKE));
        let hat = [slot("Closed Hat"), slot("Pedal Hat"), slot("Open Hat")].contains(&i);
        assert_eq!(group, if hat { 1.0 } else { 0.0 }, "{}", voice.name);
    }
}

#[test]
fn each_note_plays_its_voice_and_nothing_else() {
    for (slot, voice) in voices().iter().enumerate() {
        for &note in voice.notes {
            assert_eq!(drums::voice_for_note(note), Some(slot), "note {note}");
            let mut kit = kit();
            let out = render(&mut *kit, &[(0, hit(note, 1.0))], 0.01);
            let meters = out.first_meters();
            assert!(
                meters[slot] > 0.5,
                "{} from note {note}: meter {}",
                voice.name,
                meters[slot]
            );
            for (other, &level) in meters.iter().enumerate() {
                if other != slot {
                    assert_eq!(level, 0.0, "note {note} sounds slot {other} too");
                }
            }
            assert!(out.peak() > 0.01, "{} from note {note}", voice.name);
        }
    }
    for note in 0..128 {
        if drums::voice_for_note(note).is_none() {
            let mut kit = kit();
            let out = render(&mut *kit, &[(0, hit(note, 1.0))], 0.01);
            assert_eq!(out.peak(), 0.0, "note {note} plays nothing");
            assert!(out.first_meters().iter().all(|&m| m == 0.0));
        }
    }
}

#[test]
fn silent_without_notes_and_once_every_hit_has_died_away() {
    let mut kit = kit();
    assert_eq!(render(&mut *kit, &[], 0.5).peak(), 0.0);

    let everything: Vec<_> = voices().iter().map(|v| (0, hit(v.notes[0], 1.0))).collect();
    let out = render(&mut *kit, &everything, 6.0);
    assert!(out.peak() > 0.1);
    let last = seconds(5.9);
    assert_eq!(peak(&out.left[last..]).max(peak(&out.right[last..])), 0.0);
    assert!(out.meters.last().unwrap().iter().all(|&m| m == 0.0));
}

#[test]
fn a_hit_starts_at_its_event_frame() {
    for voice in voices() {
        let mut kit = kit();
        let (mut left, mut right) = ([1.0; BLOCK], [1.0; BLOCK]);
        let event = NoteEvent {
            frame: 40,
            kind: hit(voice.notes[0], 1.0),
        };
        kit.process(&[event], &mut left, &mut right);
        let before = peak(&left[..40]).max(peak(&right[..40]));
        assert_eq!(before, 0.0, "{}", voice.name);
        let after = peak(&left[40..]).max(peak(&right[40..]));
        assert!(after > 0.0, "{}", voice.name);
    }
}

/// A kick that is nothing but its first layer's swept sine, ringing long.
fn pure_kick(hz: f32, env: f32, time_ms: f32, settings: &[(usize, f32)]) -> Vec<f32> {
    let kick = voice("Kick");
    let mut all = only(kick, &[0]);
    all.extend([
        (kick.layer_param(0, Layer::PITCH), hz),
        (kick.layer_param(0, Layer::PITCH_ENV), env),
        (kick.layer_param(0, Layer::PITCH_TIME), time_ms),
        (kick.layer_param(0, Layer::DECAY), 3000.0),
        (kick.param(Voice::FILTER), 0.0),
    ]);
    all.extend_from_slice(settings);
    solo(kick, 0.8, &all).left
}

#[test]
fn a_layers_pitch_falls_from_its_start_to_pitch_over_pitch_time() {
    for (tune, env, time_ms) in [(50.0, 24.0, 50.0), (60.0, 12.0, 100.0), (45.0, 36.0, 30.0)] {
        let out = pure_kick(tune, env, time_ms, &[]);
        let cycles = half_cycles(&out);
        let time = time_ms / 1000.0;
        let start = tune * (env / 12.0).exp2();
        // The drop covers 95% of its semitones in Pitch Time.
        let expected = |t: f32| tune * (env / 12.0 * (-3.0 * t / time).exp()).exp2();

        // It starts at `start`: the first half cycle ends when the phase
        // that pitch curve accumulates reaches one half.
        let mut phase = 0.0;
        let mut t = 0.0;
        while phase < 0.5 {
            phase += expected(t) / RATE;
            t += 1.0 / RATE;
        }
        let first = crossings(&out)[0];
        assert!(
            (first / t - 1.0).abs() < 0.03,
            "a drop from {start} Hz crosses zero at {t} s, this at {first}"
        );
        for &(t, hz) in cycles.iter().filter(|c| c.0 < 4.0 * time) {
            let want = expected(t);
            assert!(
                (hz / want - 1.0).abs() < 0.05,
                "tune {tune}, at {t:.4} s: {hz} Hz, expected {want}"
            );
        }
        let at_time = frequency_at(&cycles, time);
        assert!(
            at_time > tune && at_time < tune * 1.1,
            "within 10% of {tune} Hz after Pitch Time: {at_time}"
        );
        let settled = frequency_between(&out, 4.0 * time, 4.0 * time + 0.3);
        assert!((settled / tune - 1.0).abs() < 0.005, "{settled} vs {tune}");
    }

    // A negative amount rises onto the pitch instead.
    let out = pure_kick(100.0, -12.0, 50.0, &[]);
    let cycles = half_cycles(&out);
    let early = frequency_at(&cycles, 0.01);
    assert!(
        early > 50.0 && early < 80.0,
        "starts an octave down: {early}"
    );
    let settled = frequency_between(&out, 0.3, 0.6);
    assert!((settled / 100.0 - 1.0).abs() < 0.005, "{settled}");
}

#[test]
fn kit_tune_and_pad_tune_transpose() {
    let kick = voice("Kick");
    for (id, semitones, hz) in [
        ("kit_tune", 12.0, 100.0),
        ("kit_tune", -12.0, 25.0),
        ("kick_transpose", 12.0, 100.0),
        ("kick_transpose", -7.0, 50.0 * (-7.0f32 / 12.0).exp2()),
    ] {
        let out = pure_kick(50.0, 24.0, 50.0, &[(index(id), semitones)]);
        let settled = frequency_between(&out, 0.3, 0.6);
        assert!(
            (settled / hz - 1.0).abs() < 0.005,
            "{id} {semitones}: {settled}"
        );
    }
    assert_eq!(index("kick_transpose"), kick.param(Voice::TUNE));
}

#[test]
fn toms_drop_onto_their_pitch_and_their_second_notes_are_higher() {
    for (name, second_note_up) in [("Low Tom", 3.0), ("Mid Tom", 2.0), ("High Tom", 3.0)] {
        let voice = voice(name);
        let tune = default(voice.layer_param(0, Layer::PITCH));
        // A 12 semitone drop over 160 ms on both modes, and a long ring.
        let mut settings = Vec::new();
        for layer in [0, 1] {
            settings.extend([
                (voice.layer_param(layer, Layer::PITCH_ENV), 12.0),
                (voice.layer_param(layer, Layer::PITCH_TIME), 160.0),
                (voice.layer_param(layer, Layer::DECAY), 400.0),
            ]);
        }
        for (note, semitones) in [(voice.notes[0], 0.0), (voice.notes[1], second_note_up)] {
            let mut kit = kit_with(voice, &settings);
            let out = render(&mut *kit, &[(0, hit(note, 1.0))], 0.4).left;
            let pitch = tune * (semitones / 12.0f32).exp2();
            let early = frequency_between(&out, 0.03, 0.05) / pitch;
            assert!(
                early > (4.0f32 / 12.0).exp2() && early < (8.0f32 / 12.0).exp2(),
                "{name} note {note}: {early} times its pitch 40 ms in"
            );
            let late = frequency_between(&out, 0.25, 0.35);
            assert!(
                (late / pitch - 1.0).abs() < 0.01,
                "{name} note {note}: settles at {late} Hz, not {pitch}"
            );
        }
    }
}

#[test]
fn the_kick_is_low_and_the_toms_ring_at_their_pitch() {
    let kick = solo(voice("Kick"), 1.0, &[]).mono();
    let low = band_share(&kick, 0.0, 200.0);
    assert!(low > 0.9, "{low} of the kick is below 200 Hz");
    for name in ["Low Tom", "Mid Tom", "High Tom"] {
        let voice = voice(name);
        let tune = f64::from(default(voice.layer_param(0, Layer::PITCH)));
        let out = solo(voice, 1.0, &[]).mono();
        let share = band_share(&out, tune * 0.9, tune * 1.6);
        assert!(share > 0.6, "{name}: {share} within its drop");
    }
}

#[test]
fn snare_wires_are_noise_above_2_khz_and_its_head_is_at_its_pitch() {
    let snare = voice("Snare");
    let high = |settings: &[(usize, f32)]| {
        let out = solo(snare, 0.6, settings).mono();
        band_share(&out, 2000.0, 1e9)
    };
    let usual = high(&[]);
    assert!(usual > 0.3, "{usual} of a default snare is above 2 kHz");
    let head = high(&only(snare, &[0, 1]));
    assert!(head < 0.03, "{head} of the head alone is above 2 kHz");
    let wires = high(&only(snare, &[2, 3]));
    assert!(wires > 0.75, "{wires} of the wires alone are above 2 kHz");
    // Higher highpasses brighten the wires.
    let brightness = |hz: f32| {
        let mut settings = only(snare, &[2, 3]);
        settings.extend([2, 3].map(|l| (snare.layer_param(l, Layer::CUTOFF), hz)));
        centroid(&solo(snare, 0.6, &settings).mono())
    };
    let (dull, crisp) = (brightness(900.0), brightness(3600.0));
    assert!(crisp > dull * 1.2, "brighter wires: {dull} to {crisp} Hz");

    // Two hits of the wires are the same noise, sample for sample, unless
    // the pad varies its hits.
    let n = seconds(0.05);
    let twice = |vary: f32| {
        let mut settings = only(snare, &[2]);
        settings.push((snare.param(Voice::VARY), vary));
        let mut kit = kit_with(snare, &settings);
        let hits = [(0, hit(38, 1.0)), (seconds(0.5), hit(38, 1.0))];
        let out = render(&mut *kit, &hits, 1.0).left;
        (
            out[..n].to_vec(),
            out[seconds(0.5)..seconds(0.5) + n].to_vec(),
        )
    };
    let (first, second) = twice(0.0);
    assert_eq!(first, second, "the same hit twice");
    assert!(first.iter().any(|s| s.abs() > 0.01));
    let (first, second) = twice(1.0);
    assert!(first.iter().zip(&second).any(|(a, b)| (a - b).abs() > 0.01));

    let tune = default(snare.layer_param(0, Layer::PITCH));
    let out = solo(snare, 0.2, &only(snare, &[0])).left;
    let pitch = frequency_between(&out, 0.03, 0.1);
    assert!((pitch / tune - 1.0).abs() < 0.02, "head at {pitch} Hz");
}

#[test]
fn hats_are_mostly_above_5_khz_and_their_cutoff_moves_them() {
    for name in ["Closed Hat", "Pedal Hat", "Open Hat"] {
        let voice = voice(name);
        let out = solo(voice, 0.8, &[]).mono();
        let share = band_share(&out, 5000.0, 1e9);
        assert!(share > 0.9, "{name}: {share} above 5 kHz");
        // Metal alone and noise alone.
        for layer in [0, 1] {
            let out = solo(voice, 0.8, &only(voice, &[layer])).mono();
            let share = band_share(&out, 5000.0, 1e9);
            assert!(share > 0.85, "{name}, layer {layer} alone: {share}");
        }
        let cutoff = |hz: f32| {
            let mut settings: Vec<_> = [0, 1]
                .map(|l| (voice.layer_param(l, Layer::CUTOFF), hz))
                .into();
            settings.push((voice.param(Voice::CUTOFF), hz * 1.5));
            centroid(&solo(voice, 0.8, &settings).mono())
        };
        let (dark, bright) = (cutoff(3500.0), cutoff(10_600.0));
        assert!(bright > dark * 1.5, "{name}: {dark} to {bright} Hz");
    }
}

#[test]
fn cymbals_are_bright_and_ring_long() {
    for (name, bright_share) in [("Crash", 0.85), ("Ride", 0.55)] {
        let voice = voice(name);
        let out = solo(voice, 3.0, &[]);
        let share = band_share(&out.mono()[..seconds(1.5)], 2000.0, 1e9);
        assert!(share > bright_share, "{name}: {share} above 2 kHz");
        let ringing = out.meters[seconds(1.0) / BLOCK][slot(name)];
        assert!(ringing > 0.01, "{name} after a second: {ringing}");
    }
}

#[test]
fn clap_rim_cowbell_shaker_conga_and_clave_sit_in_their_bands() {
    let clap = band_share(&solo(voice("Clap"), 0.6, &[]).mono(), 500.0, 5000.0);
    assert!(clap > 0.75, "{clap} of the clap between 500 Hz and 5 kHz");
    let rim = band_share(&solo(voice("Rimshot"), 0.3, &[]).mono(), 300.0, 5000.0);
    assert!(rim > 0.8, "{rim} of the rimshot between 300 Hz and 5 kHz");
    let shaker = band_share(&solo(voice("Shaker"), 0.6, &[]).mono(), 3000.0, 1e9);
    assert!(shaker > 0.9, "{shaker} of the shaker above 3 kHz");
    let conga = band_share(&solo(voice("Conga"), 0.6, &[]).mono(), 200.0, 1000.0);
    assert!(conga > 0.7, "{conga} of the conga between 200 Hz and 1 kHz");
    let clave = band_share(&solo(voice("Clave"), 0.3, &[]).mono(), 2000.0, 3000.0);
    assert!(clave > 0.7, "{clave} of the clave around 2.5 kHz");

    // The cowbell's two oscillators stand out from the space between them.
    let cowbell = voice("Cowbell");
    let around = |signal: &[f32], hz: f64| band_share(signal, hz - 15.0, hz + 15.0);
    let out = solo(cowbell, 0.5, &[]).mono();
    let gap = around(&out, 670.0);
    for hz in [540.0, 800.0] {
        let partial = around(&out, hz);
        assert!(partial > 20.0 * gap, "{hz} Hz: {partial} against {gap}");
    }
    let octave = [1, 3].map(|l| (cowbell.layer_param(l, Layer::PITCH), 1080.0));
    let octave = solo(cowbell, 0.5, &octave).mono();
    assert!(around(&octave, 1080.0) > 20.0 * around(&octave, 800.0));
}

/// Times (ms) at which `signal` jumps up in level, in 1 ms windows, at
/// least 8 ms apart.
fn onsets(signal: &[f32]) -> Vec<f32> {
    let levels: Vec<f32> = signal.chunks(seconds(0.001)).map(rms).collect();
    let top = levels.iter().copied().fold(0.0, f32::max);
    let mut found: Vec<f32> = Vec::new();
    for i in 0..levels.len() {
        let before = peak(&levels[i.saturating_sub(3)..i]);
        let recent = found.last().is_some_and(|&last| i as f32 - last < 8.0);
        if levels[i] > 0.15 * top && levels[i] > 2.5 * before && !recent {
            found.push(i as f32);
        }
    }
    found
}

#[test]
fn bursts_strike_count_times_gap_apart() {
    let clap = voice("Clap");
    for claps in 1..=8 {
        // The tail starts with the last clap, and is short, so that it
        // can't pass for another.
        let settings = [
            (clap.layer_param(0, Layer::GAP), 20.0),
            (clap.layer_param(0, Layer::BURSTS), (claps - 1) as f32),
            (clap.layer_param(1, Layer::DELAY), 20.0 * (claps - 1) as f32),
            (clap.layer_param(1, Layer::DECAY), 30.0),
        ];
        let out = solo(clap, 0.4, &settings);
        let onsets = onsets(&out.mono());
        assert_eq!(onsets.len(), claps, "{claps} claps: {onsets:?}");
        for gap in onsets.windows(2) {
            let ms = gap[1] - gap[0];
            assert!((15.0..=25.0).contains(&ms), "{claps} claps: {onsets:?}");
        }
    }
}

#[test]
fn a_layers_delay_holds_it_back() {
    let kick = voice("Kick");
    let mut settings = only(kick, &[0]);
    settings.push((kick.layer_param(0, Layer::DELAY), 50.0));
    let out = solo(kick, 0.2, &settings).mono();
    assert_eq!(peak(&out[..seconds(0.05)]), 0.0);
    assert!(peak(&out[seconds(0.05)..seconds(0.06)]) > 0.05);
}

/// A ringing open hat in a kit with `settings`, for choke tests.
fn ringing_open(settings: &[(usize, f32)]) -> Box<dyn Instrument> {
    let open = voice("Open Hat");
    let mut kit = kit();
    kit.set_param(open.param(Voice::LENGTH), 450.0);
    for &(index, value) in settings {
        kit.set_param(index, value);
    }
    kit
}

#[test]
fn pads_in_a_choke_group_cut_each_other_off() {
    let open_slot = slot("Open Hat");
    let at = seconds(0.1);
    for name in ["Closed Hat", "Pedal Hat"] {
        let close = (at, hit(voice(name).notes[0], 1.0));
        let both = render(&mut *ringing_open(&[]), &[(0, hit(46, 1.0)), close], 0.3);
        let alone = render(&mut *ringing_open(&[]), &[close], 0.3);
        let before = both.meters[at / BLOCK - 1][open_slot];
        assert!(before > 0.5, "rings until {name}: {before}");

        // It fades rather than cuts...
        let fading = seconds(0.002);
        let still_there = both.left[at..at + fading]
            .iter()
            .zip(&alone.left[at..at + fading])
            .any(|(a, b)| (a - b).abs() > 0.01);
        assert!(still_there, "{name} fades the open hat out");
        // ...and is gone within 10 ms, leaving exactly the closing hat.
        let after = at + seconds(0.01);
        assert_eq!(both.meters[after / BLOCK][open_slot], 0.0, "{name}");
        let difference = both.left[after..]
            .iter()
            .zip(&alone.left[after..])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(difference < 1e-6, "{name} leaves {difference}");
    }

    // Nothing outside the group chokes it.
    for (other, voice) in voices().iter().enumerate() {
        if default(voice.param(Voice::CHOKE)) != 0.0 || other == open_slot {
            continue;
        }
        let hits = [(0, hit(46, 1.0)), (at, hit(voice.notes[0], 1.0))];
        let out = render(&mut *ringing_open(&[]), &hits, 0.2);
        let ringing = out.meters[seconds(0.15) / BLOCK][open_slot];
        assert!(ringing > 0.5, "{} chokes it: {ringing}", voice.name);
    }

    // Out of the group, the closed hat lets it ring.
    let closed = voice("Closed Hat").param(Voice::CHOKE);
    let hits = [(0, hit(46, 1.0)), (at, hit(42, 1.0))];
    let out = render(&mut *ringing_open(&[(closed, 0.0)]), &hits, 0.2);
    assert!(out.meters[seconds(0.15) / BLOCK][open_slot] > 0.5);

    // And any pads can share a group: here the ride chokes the crash.
    let crash = voice("Crash");
    let groups = [
        (crash.param(Voice::CHOKE), 2.0),
        (voice("Ride").param(Voice::CHOKE), 2.0),
    ];
    let mut kit = kit();
    for (index, value) in groups {
        kit.set_param(index, value);
    }
    let out = render(&mut *kit, &[(0, hit(49, 1.0)), (at, hit(51, 1.0))], 0.2);
    assert_eq!(out.meters[seconds(0.15) / BLOCK][slot("Crash")], 0.0);
}

#[test]
fn velocity_scales_the_level_by_its_sensitivity() {
    for name in ["Kick", "Snare", "Clap", "Closed Hat", "Crash", "Cowbell"] {
        let voice = voice(name);
        let slot = slot(name);
        for sensitivity in [0.0, 0.5, 1.0] {
            let level = |velocity: f32| {
                let mut kit = kit_with(voice, &[(voice.param(Voice::VELOCITY), sensitivity)]);
                let out = render(&mut *kit, &[(0, hit(voice.notes[0], velocity))], 0.2);
                (rms(&out.mono()), out.first_meters()[slot])
            };
            let (full, full_meter) = level(1.0);
            let expected = 0.5f32.powf(2.0 * sensitivity);
            let (half, half_meter) = level(0.5);
            assert!(
                (half / full / expected - 1.0).abs() < 0.01,
                "{name} at sensitivity {sensitivity}: {} not {expected}",
                half / full
            );
            assert!((half_meter / full_meter / expected - 1.0).abs() < 0.01);
        }
    }
}

#[test]
fn a_layers_velocity_sensitivity_adds_to_the_pads() {
    // The kick's click fades out of soft hits when its layer follows velocity.
    let kick = voice("Kick");
    let click = |velocity: f32, sensitivity: f32| {
        let mut settings = only(kick, &[2]);
        settings.push((kick.param(Voice::VELOCITY), 0.0));
        settings.push((kick.layer_param(2, Layer::VELOCITY), sensitivity));
        let mut kit = kit_with(kick, &settings);
        rms(&render(&mut *kit, &[(0, hit(36, velocity))], 0.05).mono())
    };
    assert!((click(0.5, 0.0) / click(1.0, 0.0) - 1.0).abs() < 0.01);
    assert!((click(0.5, 1.0) / click(1.0, 1.0) - 0.25).abs() < 0.01);
}

#[test]
fn layer_decays_are_as_set_and_length_stretches_them() {
    // One layer alone rings for its Decay: −60 dB from its loudest.
    for (name, layer) in [("Kick", 0), ("Low Tom", 0), ("Open Hat", 1), ("Crash", 3)] {
        let voice = voice(name);
        let ms = default(voice.layer_param(layer, Layer::DECAY));
        let mut settings = only(voice, &[layer]);
        settings.push((voice.param(Voice::FILTER), 0.0));
        let out = solo(voice, ms / 1000.0 * 1.5, &settings).mono();
        let rings = ring_time(&out, 60.0) * 1000.0;
        assert!(
            (rings / ms - 1.0).abs() < 0.2,
            "{name}: {rings} ms, not {ms}"
        );
    }
    // Length scales every layer's: the pad's envelope lasts as much longer.
    for (slot, voice) in voices().iter().enumerate() {
        let lasts = |length: f32| {
            let out = solo(
                voice,
                7.0 * length / 100.0,
                &[(voice.param(Voice::LENGTH), length)],
            );
            out.meter_fall(slot, 0.001)
        };
        let normal = lasts(100.0);
        for length in [50.0, 200.0] {
            let ratio = lasts(length) / normal;
            let expected = length / 100.0;
            assert!(
                (ratio / expected - 1.0).abs() < 0.2,
                "{}: Length {length}% lasts {ratio} times as long",
                voice.name
            );
        }
    }
}

#[test]
fn a_straight_curve_ends_at_the_decay_and_hold_sustains() {
    let kick = voice("Kick");
    let mut settings = only(kick, &[0]);
    settings.extend([
        (kick.layer_param(0, Layer::PITCH_ENV), 0.0),
        (kick.layer_param(0, Layer::DECAY), 200.0),
        (kick.layer_param(0, Layer::CURVE), 1.0),
    ]);
    let out = solo(kick, 0.4, &settings).mono();
    assert!(peak(&out[seconds(0.15)..seconds(0.19)]) > 0.02);
    assert!(peak(&out[seconds(0.201)..]) < 1e-6, "silent at its decay");

    settings.push((kick.layer_param(0, Layer::HOLD), 100.0));
    let held = solo(kick, 0.4, &settings).mono();
    let early = peak(&held[..seconds(0.02)]);
    let late = peak(&held[seconds(0.08)..seconds(0.1)]);
    assert!(
        (late / early - 1.0).abs() < 0.02,
        "full for 100 ms: {early} {late}"
    );
    assert!(peak(&held[seconds(0.29)..]) > 0.0, "then 200 ms of decay");
}

#[test]
fn phase_sets_where_an_oscillator_starts() {
    let kick = voice("Kick");
    let start = |degrees: f32| {
        let mut settings = only(kick, &[0]);
        settings.extend([
            (kick.layer_param(0, Layer::PHASE), degrees),
            (kick.param(Voice::FILTER), 0.0),
        ]);
        solo(kick, 0.01, &settings).left[0]
    };
    assert_eq!(start(0.0), 0.0, "a sine from zero: no click");
    assert!(start(90.0) > 0.2, "from its peak: {}", start(90.0));
    assert!(start(270.0) < -0.2, "from its trough: {}", start(270.0));
}

#[test]
fn a_zero_attack_starts_at_full_level() {
    let kick = voice("Kick");
    let mut settings = only(kick, &[0]);
    settings.extend([
        (kick.layer_param(0, Layer::PHASE), 90.0),
        (kick.layer_param(0, Layer::ATTACK), 0.0),
        (kick.layer_param(0, Layer::PITCH_ENV), 0.0),
        (kick.param(Voice::FILTER), 0.0),
    ]);
    let out = solo(kick, 0.05, &settings).left;
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        out[0] > 0.95 * peak,
        "the first sample is the peak: {} of {peak}",
        out[0]
    );
    // The knob reaches zero.
    let attack = &drums::DESCRIPTOR.params[kick.layer_param(0, Layer::ATTACK)];
    assert_eq!(attack.from_normalized(0.0), 0.0);
}

#[test]
fn every_pad_plays_the_same_hit_each_time() {
    for (i, voice) in voices().iter().enumerate() {
        let mut kit = kit_with(voice, &[]);
        // Far enough apart that the first has died away.
        let hits = [
            (0, hit(voice.notes[0], 1.0)),
            (seconds(6.0), hit(voice.notes[0], 1.0)),
        ];
        let out = render(&mut *kit, &hits, 6.2).left;
        let n = seconds(0.15);
        let (first, second) = (&out[..n], &out[seconds(6.0)..seconds(6.0) + n]);
        let peak = first.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let worst = first
            .iter()
            .zip(second)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(
            worst <= 1e-4 * peak,
            "{} ({i}): {worst} of {peak}",
            voice.name
        );
    }
}

#[test]
fn layers_turn_off_and_on() {
    let kick = voice("Kick");
    // The click is what's above 2 kHz.
    let bright =
        |settings: &[(usize, f32)]| band_share(&solo(kick, 0.5, settings).mono(), 2000.0, 1e9);
    let click = kick.layer_param(2, Layer::MODE);
    assert!(bright(&[(click, 0.0)]) < bright(&[]) * 0.2);

    // A pad with every layer off is silent, its meter too.
    let silent = solo(kick, 0.1, &only(kick, &[]));
    assert_eq!(silent.peak(), 0.0);
    assert!(silent.meters.iter().all(|m| m[slot("Kick")] == 0.0));

    // A spare layer, turned on, adds its sound.
    let spare = kick.layer(5);
    let settings = [
        (spare.start + Layer::MODE, Mode::On as usize as f32),
        (spare.start + Layer::SOURCE, Source::Sine as usize as f32),
        (spare.start + Layer::PITCH, 1000.0),
        (kick.param(Voice::FILTER), 0.0),
    ];
    let out = solo(kick, 0.3, &settings).mono();
    let around = band_share(&out, 950.0, 1050.0);
    assert!(around > 0.05, "{around} of it at the spare's 1 kHz");
}

#[test]
fn an_fm_only_layer_is_silent_but_modulates_the_next() {
    let kick = voice("Kick");
    let setup = |mode: Mode, fm: f32| {
        let mut settings = only(kick, &[0, 1]);
        for (layer, hz) in [(0, 3000.0), (1, 500.0)] {
            settings.extend([
                (kick.layer_param(layer, Layer::PITCH), hz),
                (kick.layer_param(layer, Layer::PITCH_ENV), 0.0),
                (kick.layer_param(layer, Layer::DRIVE), 0.0),
                (kick.layer_param(layer, Layer::DECAY), 2000.0),
            ]);
        }
        settings.extend([
            (kick.layer_param(0, Layer::MODE), mode as usize as f32),
            (kick.layer_param(1, Layer::FM), fm),
            (kick.param(Voice::FILTER), 0.0),
        ]);
        solo(kick, 0.5, &settings).mono()
    };
    // Without FM, an FM-only layer leaves just the other: a pure 500 Hz.
    let plain = setup(Mode::FmOnly, 0.0);
    assert!(band_share(&plain, 450.0, 550.0) > 0.99);
    // With FM from 3 kHz, sidebands 3 kHz either side (folded through 0).
    let modulated = setup(Mode::FmOnly, 1.0);
    let carrier = band_share(&modulated, 450.0, 550.0);
    let sidebands = band_share(&modulated, 2450.0, 2550.0) + band_share(&modulated, 3450.0, 3550.0);
    assert!(carrier < 0.9 && sidebands > 0.1, "{carrier} {sidebands}");
    let modulator = band_share(&modulated, 2950.0, 3050.0);
    assert!(modulator < 0.01, "the modulator is silent: {modulator}");
    // On, the modulator is heard as well.
    let both = setup(Mode::On, 1.0);
    assert!(band_share(&both, 2950.0, 3050.0) > 0.1);
}

#[test]
fn every_source_sounds_stays_bounded_and_follows_pitch() {
    let kick = voice("Kick");
    for (i, source) in Source::ALL.iter().enumerate() {
        for shape in [0.0, 0.5, 1.0] {
            let mut settings = only(kick, &[0]);
            settings.extend([
                (kick.layer_param(0, Layer::SOURCE), i as f32),
                (kick.layer_param(0, Layer::SHAPE), shape),
                (kick.layer_param(0, Layer::PITCH), 440.0),
                (kick.layer_param(0, Layer::PITCH_ENV), 0.0),
                (kick.layer_param(0, Layer::DECAY), 500.0),
                (kick.param(Voice::FILTER), 0.0),
            ]);
            let out = solo(kick, 0.3, &settings);
            let loudest = out.peak();
            assert!(loudest > 0.01, "{source:?} at {shape}: {loudest}");
            assert!(loudest < 2.0, "{source:?} at {shape}: {loudest}");
        }
    }
    // The pitched ones play at their pitch, and Pitch moves the rest.
    for source in [Source::Sine, Source::Triangle, Source::Saw, Source::Square] {
        let mut settings = only(kick, &[0]);
        settings.extend([
            (kick.layer_param(0, Layer::SOURCE), source as usize as f32),
            (kick.layer_param(0, Layer::PITCH), 300.0),
            (kick.layer_param(0, Layer::PITCH_ENV), 0.0),
            (kick.layer_param(0, Layer::DECAY), 2000.0),
            (kick.param(Voice::FILTER), 0.0),
        ]);
        let out = solo(kick, 0.4, &settings).mono();
        let hz = frequency_between(&out, 0.1, 0.3);
        // Two crossings a cycle, for every one of these.
        assert!((hz / 300.0 - 1.0).abs() < 0.01, "{source:?}: {hz} Hz");
    }
    let brighter = |source: Source| {
        let rate = |hz: f32| {
            let mut settings = only(kick, &[0]);
            settings.extend([
                (kick.layer_param(0, Layer::SOURCE), source as usize as f32),
                (kick.layer_param(0, Layer::PITCH), hz),
                (kick.param(Voice::FILTER), 0.0),
            ]);
            centroid(&solo(kick, 0.3, &settings).mono())
        };
        rate(4000.0) / rate(500.0)
    };
    for source in [Source::Digital, Source::Metal, Source::Bell] {
        assert!(brighter(source) > 1.3, "{source:?}: {}", brighter(source));
    }
    // Crackle's Pitch is how many clicks a second: more is louder.
    let crackle = |hz: f32| {
        let mut settings = only(kick, &[0]);
        settings.extend([
            (
                kick.layer_param(0, Layer::SOURCE),
                Source::Crackle as usize as f32,
            ),
            (kick.layer_param(0, Layer::PITCH), hz),
            (kick.param(Voice::FILTER), 0.0),
        ]);
        rms(&solo(kick, 0.3, &settings).mono())
    };
    assert!(crackle(4000.0) > 2.0 * crackle(250.0));
}

#[test]
fn noise_colors_tilt_from_dark_to_bright() {
    let snare = voice("Snare");
    let order = [
        Source::Brown,
        Source::Pink,
        Source::White,
        Source::Blue,
        Source::Violet,
    ]
    .map(|source| {
        let mut settings = only(snare, &[2]);
        settings.extend([
            (snare.layer_param(2, Layer::SOURCE), source as usize as f32),
            (snare.layer_param(2, Layer::FILTER), 0.0),
            (snare.param(Voice::FILTER), 0.0),
        ]);
        centroid(&solo(snare, 0.3, &settings).mono())
    });
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
}

#[test]
fn filters_track_tune_and_sweep_with_their_envelope() {
    let snare = voice("Snare");
    let wires = |settings: &[(usize, f32)]| {
        let mut all = only(snare, &[2]);
        all.extend([
            (
                snare.layer_param(2, Layer::FILTER),
                FilterKind::Band as usize as f32,
            ),
            (snare.layer_param(2, Layer::CUTOFF), 1000.0),
            (snare.layer_param(2, Layer::RESONANCE), 0.6),
            (snare.param(Voice::FILTER), 0.0),
        ]);
        all.extend_from_slice(settings);
        solo(snare, 0.3, &all).mono()
    };
    let center = |out: &[f32]| centroid(out);
    let base = center(&wires(&[]));
    let up = [(snare.param(Voice::TUNE), 12.0)];
    let tracked = [(snare.layer_param(2, Layer::TRACK), 1.0), up[0]];
    let fixed = [(snare.layer_param(2, Layer::TRACK), 0.0), up[0]];
    let (tracked, fixed) = (center(&wires(&tracked)), center(&wires(&fixed)));
    assert!(tracked > base * 1.6, "tracking: {base} to {tracked}");
    assert!((fixed / base - 1.0).abs() < 0.1, "fixed: {base} to {fixed}");

    // A sweep from four octaves up: bright early, back to the band later.
    let swept = wires(&[
        (snare.layer_param(2, Layer::FILTER_ENV), 4.0),
        (snare.layer_param(2, Layer::FILTER_TIME), 60.0),
    ]);
    let early = centroid(&swept[..seconds(0.01)]);
    let late = centroid(&swept[seconds(0.1)..seconds(0.2)]);
    assert!(early > late * 2.0, "{early} then {late}");
}

#[test]
fn the_pad_filter_shapes_the_whole_pad() {
    let snare = voice("Snare");
    let with = |kind: FilterKind, hz: f32| {
        let settings = [
            (snare.param(Voice::FILTER), kind as usize as f32),
            (snare.param(Voice::CUTOFF), hz),
        ];
        centroid(&solo(snare, 0.4, &settings).mono())
    };
    let open = with(FilterKind::Off, 1000.0);
    assert!(with(FilterKind::Low24, 800.0) < open * 0.3);
    assert!(with(FilterKind::High24, 4000.0) > open * 1.2);
}

#[test]
fn a_layers_drive_saturates_it() {
    let kick = voice("Kick");
    let crest = |drive: f32| {
        let mut settings = only(kick, &[0]);
        settings.extend([
            (kick.layer_param(0, Layer::DRIVE), drive),
            (kick.layer_param(0, Layer::PITCH_ENV), 0.0),
            (kick.param(Voice::FILTER), 0.0),
        ]);
        let out = solo(kick, 0.1, &settings).mono();
        peak(&out) / rms(&out)
    };
    assert!(
        crest(1.0) < crest(0.0) * 0.8,
        "squarer: {} {}",
        crest(0.0),
        crest(1.0)
    );
}

#[test]
fn hitting_a_ringing_voice_again_does_not_click() {
    // A long, clean kick, hit again as its sine peaks.
    let kick = voice("Kick");
    let mut settings = only(kick, &[0]);
    settings.push((kick.layer_param(0, Layer::DECAY), 2000.0));
    settings.push((kick.layer_param(0, Layer::LEVEL), 0.0));
    let fresh = solo(kick, 0.05, &settings).left;
    let attack = max_step(&fresh);

    let once = solo(kick, 0.25, &settings).left;
    let near = seconds(0.2);
    let at = (near..near + seconds(0.02))
        .max_by(|&a, &b| once[a].abs().total_cmp(&once[b].abs()))
        .unwrap();
    let mut kit = kit_with(kick, &settings);
    let again = render(&mut *kit, &[(0, hit(36, 1.0)), (at, hit(36, 1.0))], 0.3).left;
    assert!(once[at].abs() > 0.2, "hit again when loud: {}", once[at]);
    let step = max_step(&again[at - 1..at + seconds(0.02)]);
    assert!(
        step < attack * 1.3,
        "hitting again steps by {step}; a fresh kick by {attack}"
    );
}

#[test]
fn level_pan_and_cutoff_ramp_while_a_hit_rings() {
    let kick = voice("Kick");
    let mut settings = only(kick, &[0]);
    settings.extend([
        (kick.layer_param(0, Layer::DECAY), 3000.0),
        (kick.param(Voice::FILTER), 0.0),
    ]);
    let play = |change: &[(usize, f32)]| {
        let mut kit = kit_with(kick, &settings);
        render(&mut *kit, &[(0, hit(36, 1.0))], 0.2);
        for &(index, value) in change {
            kit.set_param(index, value);
        }
        render(&mut *kit, &[], 0.05)
    };
    let steady = play(&[]);
    // Turning the level down, or panning right, takes the left channel to
    // silence over 20 ms.
    for (offset, value) in [(Voice::LEVEL, -60.0), (Voice::PAN, 1.0)] {
        let changed = play(&[(kick.param(offset), value)]);
        // The gain each sample, where the signal is loud enough to tell.
        let gains: Vec<(usize, f32)> = steady
            .left
            .iter()
            .zip(&changed.left)
            .enumerate()
            .filter(|(_, (s, _))| s.abs() > 0.05)
            .map(|(i, (s, c))| (i, c / s))
            .collect();
        assert!(gains[0].1 > 0.95, "starts where it was: {:?}", gains[0]);
        let smooth = gains.windows(2).all(|w| w[1].1 <= w[0].1 + 1e-4);
        assert!(smooth, "falls smoothly");
        let done = gains.iter().find(|(i, _)| *i > seconds(0.021)).unwrap();
        assert!(done.1 < 0.0011, "arrives in 20 ms: {done:?}");
    }
    // Pulling the pad's lowpass down under a ringing kick doesn't click.
    let lowpass = [
        (kick.param(Voice::FILTER), FilterKind::Low12 as usize as f32),
        (kick.param(Voice::CUTOFF), 20_000.0),
    ];
    let mut kit = kit_with(kick, &settings);
    for (index, value) in lowpass {
        kit.set_param(index, value);
    }
    render(&mut *kit, &[(0, hit(36, 1.0))], 0.2);
    kit.set_param(kick.param(Voice::CUTOFF), 30.0);
    let out = render(&mut *kit, &[], 0.1).left;
    assert!(max_step(&out) < 0.02, "steps by {}", max_step(&out));
    assert!(peak(&out[seconds(0.05)..]) < peak(&out[..seconds(0.01)]) * 0.5);
}

#[test]
fn pan_places_a_voice_and_its_layers() {
    let snare = voice("Snare");
    let left = solo(snare, 0.1, &[(snare.param(Voice::PAN), -1.0)]);
    assert!(peak(&left.left) > 0.1);
    assert_eq!(peak(&left.right), 0.0);
    let right = solo(snare, 0.1, &[(snare.param(Voice::PAN), 1.0)]);
    assert!(peak(&right.left) < 1e-5);
    let center = solo(snare, 0.1, &[(snare.param(Voice::PAN), 0.0)]);
    for (l, r) in center.left.iter().zip(&center.right) {
        assert!((l - r).abs() < 1e-6);
    }
    // Constant power, unity in each channel at the center.
    let (side, middle) = (rms(&left.left), rms(&center.left));
    assert!((side / middle - 2f32.sqrt()).abs() < 0.01);

    // A layer panned hard left is only on the left.
    let mut settings = only(snare, &[2]);
    settings.push((snare.layer_param(2, Layer::PAN), -1.0));
    let wires = solo(snare, 0.1, &settings);
    assert!(peak(&wires.left) > 0.05);
    assert_eq!(peak(&wires.right), 0.0);
}

#[test]
fn meters_follow_level_velocity_and_the_envelope() {
    for (slot, voice) in voices().iter().enumerate() {
        let meter = |settings: &[(usize, f32)], velocity: f32| {
            let mut kit = kit();
            for &(index, value) in settings {
                kit.set_param(index, value);
            }
            render(&mut *kit, &[(0, hit(voice.notes[0], velocity))], 0.01).first_meters()[slot]
        };
        let full = meter(&[], 1.0);
        assert!(full > 0.7 && full <= 1.0, "{}: {full}", voice.name);
        let quieter = meter(&[(voice.param(Voice::LEVEL), -6.0)], 1.0) / full;
        assert!((quieter - 0.501).abs() < 0.005, "{}: {quieter}", voice.name);
        let softer = meter(&[], 0.5) / full;
        assert!((softer - 0.5).abs() < 0.005, "{}: {softer}", voice.name);
    }
    let out = solo(voice("Open Hat"), 1.0, &[]);
    let open = slot("Open Hat");
    let levels: Vec<f32> = out.meters.iter().map(|m| m[open]).collect();
    assert!(levels.windows(2).all(|w| w[1] <= w[0]), "falls as it rings");
}

#[test]
fn all_notes_off_lets_hits_ring_and_reset_silences() {
    let open = voice("Open Hat");
    let mut kit = ringing_open(&[]);
    render(&mut *kit, &[(0, hit(open.notes[0], 1.0))], 0.05);
    let release = [
        (0, NoteEventKind::AllNotesOff),
        (
            1,
            NoteEventKind::Off {
                pitch: Pitch::new(open.notes[0]).unwrap(),
                velocity: Velocity::CENTER,
            },
        ),
    ];
    let out = render(&mut *kit, &release, 0.1);
    let ringing = out.meters.last().unwrap()[slot("Open Hat")];
    assert!(ringing > 0.5, "still ringing: {ringing}");
    kit.reset();
    let out = render(&mut *kit, &[], 0.01);
    assert_eq!(out.peak(), 0.0);
    assert!(out.first_meters().iter().all(|&m| m == 0.0));
}

#[test]
fn settings_apply_from_the_next_hit() {
    let kick = voice("Kick");
    let mut settings = only(kick, &[0]);
    settings.push((kick.layer_param(0, Layer::DECAY), 3000.0));
    let mut kit = kit_with(kick, &settings);
    let first = render(&mut *kit, &[(0, hit(36, 1.0))], 0.3).left;
    kit.set_param(kick.layer_param(0, Layer::PITCH), 80.0);
    let rest = render(&mut *kit, &[(seconds(0.3), hit(36, 1.0))], 0.9).left;
    let ringing = frequency_between(&rest, 0.0, 0.25);
    assert!((ringing / 50.0 - 1.0).abs() < 0.01, "{ringing}");
    let next = frequency_between(&rest, 0.6, 0.9);
    assert!((next / 80.0 - 1.0).abs() < 0.01, "the next plays {next}");
    assert!(first.iter().all(|s| s.is_finite()));
}

#[test]
fn kit_level_scales_and_kit_drive_saturates() {
    let snare = voice("Snare");
    let play = |level: f32, drive: f32| {
        let mut kit = kit();
        set(&mut *kit, "kit_level", level);
        set(&mut *kit, "kit_drive", drive);
        render(&mut *kit, &[(0, hit(snare.notes[0], 1.0))], 0.3)
    };
    let plain = play(0.0, 0.0);
    let quieter = rms(&play(-6.0, 0.0).left) / rms(&plain.left);
    assert!((quieter - 0.501).abs() < 0.002, "{quieter}");
    let driven = play(0.0, 1.0);
    assert!(rms(&driven.left) > 1.5 * rms(&plain.left), "thicker");
    assert!(
        driven.peak() <= 1.0 + 1e-6,
        "past full scale: {}",
        driven.peak()
    );
}

#[test]
fn render_hit_draws_a_pad_and_each_layer() {
    let values: Vec<f32> = drums::DESCRIPTOR
        .params
        .iter()
        .map(ParamDescriptor::default_value)
        .collect();
    let kick = slot("Kick");
    let whole = render_hit(&values, kick, None, RATE, 2.0);
    assert!(peak(&whole) > 0.2, "{}", peak(&whole));
    assert!(whole.len() > seconds(0.4) && whole.len() < seconds(1.0));
    // The click alone is short and bright; the sub alone long and low.
    let click = render_hit(&values, kick, Some(2), RATE, 2.0);
    let sub = render_hit(&values, kick, Some(0), RATE, 2.0);
    assert!(ring_time(&click, 40.0) < 0.05);
    assert!(centroid(&click) > 4.0 * centroid(&sub));
    // Capped at the length asked for, and safe with nonsense.
    let open = render_hit(&values, slot("Crash"), None, RATE, 0.5);
    assert_eq!(open.len(), seconds(0.5));
    assert!(render_hit(&values, 99, None, RATE, 1.0).is_empty());
    let mut broken = values.clone();
    broken.truncate(10);
    broken[5] = f32::NAN;
    assert!(peak(&render_hit(&broken, kick, None, RATE, 1.0)) > 0.2);
}

/// Every voice, staggered a little, at `velocity`.
fn every_voice(velocity: f32) -> Vec<(usize, NoteEventKind)> {
    (0..128u8)
        .filter(|&note| drums::voice_for_note(note).is_some())
        .enumerate()
        .map(|(i, note)| (i * 37, hit(note, velocity)))
        .collect()
}

#[test]
fn extreme_settings_stay_finite_and_bounded() {
    let params = drums::DESCRIPTOR.params;
    let mut loudest = 0.0f32;
    // Each pad's own settings at their extremes, and each setting of its
    // first layer, all pads at once.
    for offset in 0..Voice::COMMON + Layer::COUNT {
        for extreme in [0, 1] {
            let mut kit = kit();
            for voice in voices() {
                let index = voice.params.start + offset;
                kit.set_param(index, range(&params[index])[extreme]);
            }
            loudest = loudest.max(render(&mut *kit, &every_voice(1.0), 0.15).peak());
        }
    }
    for extreme in [0, 1] {
        let mut kit = kit();
        for (index, param) in params.iter().enumerate() {
            kit.set_param(index, range(param)[extreme]);
        }
        loudest = loudest.max(render(&mut *kit, &every_voice(1.0), 1.0).peak());
    }
    // Everything on, at random.
    let mut seed = 0x2545_F491u32;
    let mut random = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    for _ in 0..30 {
        let mut kit = kit();
        for (index, param) in params.iter().enumerate() {
            let [min, max] = range(param);
            kit.set_param(index, min + (max - min) * random());
        }
        loudest = loudest.max(render(&mut *kit, &every_voice(random()), 0.2).peak());
    }
    // Sixteen pads of six layers, each up to +24 dB and through resonant
    // filters, can be very loud, but never unbounded.
    assert!(loudest < 2000.0, "peaked at {loudest}");

    // NaN is ignored and infinities clamped.
    let mut kit = kit();
    for index in 0..params.len() + 2 {
        kit.set_param(index, f32::NAN);
    }
    assert!(render(&mut *kit, &every_voice(1.0), 0.1).peak() > 0.1);
    for value in [f32::INFINITY, f32::NEG_INFINITY] {
        for index in 0..params.len() {
            kit.set_param(index, value);
        }
        render(&mut *kit, &every_voice(1.0), 0.1);
    }
}

#[test]
fn the_default_kit_stays_near_full_scale() {
    let mut kit = kit();
    let out = render(&mut *kit, &every_voice(1.0), 1.0);
    assert!(out.peak() > 0.5 && out.peak() < 4.0, "{}", out.peak());
    for voice in voices() {
        let alone = solo(voice, 1.0, &[]).peak();
        assert!(alone > 0.08 && alone < 1.0, "{}: {alone}", voice.name);
    }
}

#[test]
fn other_sample_rates_and_long_blocks_work() {
    let settings = [
        (22_050.0, 512),
        (44_100.0, 4096),
        (96_000.0, 128),
        (192_000.0, 2048),
    ];
    for (rate, block) in settings {
        let mut kit = (drums::DESCRIPTOR.create)();
        kit.prepare(rate, block);
        let events: Vec<NoteEvent> = (0..128u8)
            .filter(|&note| drums::voice_for_note(note).is_some())
            .map(|note| NoteEvent {
                frame: 0,
                kind: hit(note, 1.0),
            })
            .collect();
        let (mut left, mut right) = (vec![0.0; block], vec![0.0; block]);
        let mut loudest = 0.0f32;
        let blocks = (6.0 * rate) as usize / block;
        for i in 0..blocks {
            kit.process(if i == 0 { &events } else { &[] }, &mut left, &mut right);
            let finite = left.iter().chain(&right).all(|s| s.is_finite());
            assert!(finite, "{rate} Hz");
            loudest = loudest.max(peak(&left)).max(peak(&right));
        }
        // Every note at once, each voice's alternative notes stacking on it.
        assert!(loudest > 0.1 && loudest < 8.0, "{rate} Hz: {loudest}");
        let last = peak(&left).max(peak(&right));
        assert_eq!(last, 0.0, "{rate} Hz: silent after");
    }
}

#[test]
#[ignore = "prints a table for tuning the kit; run with --ignored --nocapture"]
fn report() {
    println!(
        "{:<11} {:>6} {:>6} {:>7} {:>6} {:>6} {:>8} {:>7}",
        "voice", "peak", "rms", "<200Hz", ">2k", ">5k", "centroid", "T60 ms"
    );
    for (slot, voice) in voices().iter().enumerate() {
        let out = solo(voice, 12.0, &[]);
        let mono = out.mono();
        let attack = &mono[..seconds(0.1)];
        let body = &mono[..seconds(1.5)];
        println!(
            "{:<11} {:>6.3} {:>6.3} {:>7.3} {:>6.3} {:>6.3} {:>8.0} {:>7.0}",
            voice.name,
            out.peak(),
            rms(attack),
            band_share(body, 0.0, 200.0),
            band_share(body, 2000.0, 1e9),
            band_share(body, 5000.0, 1e9),
            centroid(body),
            out.meter_fall(slot, 0.001) * 1000.0,
        );
    }
}
