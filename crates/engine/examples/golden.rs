//! Golden renders: every built-in effect and instrument through many
//! settings, one hash of the output (audio and meters) per render, so a
//! refactor can show it changed no sample and no meter reading anywhere.
//!
//! ```sh
//! cargo run --release -p engine --example golden -- before.txt
//! # ...refactor...
//! cargo run --release -p engine --example golden -- after.txt
//! diff before.txt after.txt     # nothing: every sample is as it was
//! ```
//!
//! Each device renders its defaults; then, one parameter at a time, every
//! choice of an enum (switched halfway through, so both the switch and the
//! new setting are heard), and every float from its minimum gliding to its
//! maximum (a knob move halfway through) and swept by a fast modulator
//! through [`Effect::modulate`]. Effects hear noise with some tone in it (and
//! a sidechain, if they listen to one); instruments play chords, bends, the
//! mod wheel and an all-notes-off. Devices that play from data (the
//! sampler's bank, the wavetable's user tables, the slicer's loop) get
//! synthetic data.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use effects::Context;
use instruments::builtin::sampler::{self, Decoded};
use instruments::builtin::slicer::SliceData;
use instruments::builtin::wavetable::{UserTable, UserTables};
use instruments::{Instrument, InstrumentData, ParamDescriptor, ParameterKind};
use midi::{MOD_WHEEL, NoteEvent, NoteEventKind, Pitch, Velocity};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 64;
const EFFECT_FRAMES: usize = 12_000;
const INSTRUMENT_FRAMES: usize = 24_000;

/// FNV-1a over the samples' bits, and the meters read after every block.
fn hash(left: &[f32], right: &[f32], meters: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for s in left.iter().chain(right).chain(meters) {
        for byte in s.to_bits().to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// How one render sets its parameter.
#[derive(Clone, Copy)]
enum Variant {
    /// The device's own defaults, without [`busy`]'s settings.
    Plain,
    Defaults,
    /// Enum parameter `.0` switched to choice `.1` halfway through.
    Switch(usize, f32),
    /// Float parameter `.0` at its minimum, turned to its maximum halfway.
    Turn(usize),
    /// Float parameter `.0` swept by a modulator, minimum to maximum and
    /// back four times a second.
    Sweep(usize),
}

/// Settings that bring more of a device into play than its defaults do,
/// so changing one parameter is heard: every EQ band and Shaper module on,
/// every Wavetable section and mod slot in use with every LFO shape, every
/// drum layer sounding. The other parameters' renders start from these.
fn busy(id: &str, params: &[ParamDescriptor]) -> Vec<(usize, f32)> {
    let mut out = Vec::new();
    let mut set = |param: String, value: f32| {
        if let Some(index) = params.iter().position(|p| p.id == param) {
            out.push((index, value));
        }
    };
    match id {
        "canopy.eq" => {
            for band in 1..=24 {
                set(format!("b{band}_on"), 1.0);
                set(format!("b{band}_dyn"), (band % 2) as f32);
                set(
                    format!("b{band}_gain"),
                    if band % 3 == 0 { -4.0 } else { 4.0 },
                );
            }
        }
        "canopy.shaper" => {
            for module in ["vol", "flt", "pan"] {
                set(format!("{module}_on"), 1.0);
            }
        }
        "canopy.wavetable" => {
            for switch in [
                "osc_b_on",
                "osc_c_on",
                "sub_on",
                "noise_on",
                "filter_on",
                "filter2_on",
            ] {
                set(switch.to_owned(), 1.0);
            }
            set("osc_a_unison".to_owned(), 3.0);
            let destinations = instruments::builtin::wavetable::DESTINATIONS.len();
            for slot in 1..=64usize {
                set(format!("mod{slot}_source"), (slot % 32 + 1) as f32);
                set(
                    format!("mod{slot}_dest"),
                    (1 + (slot * 7) % (destinations - 1)) as f32,
                );
                set(format!("mod{slot}_amount"), 0.2);
            }
            for lfo in 1..=8usize {
                let shape = match lfo {
                    7 => 8, // Points
                    8 => 7, // Custom
                    n => n - 1,
                };
                set(format!("lfo{lfo}_shape"), shape as f32);
            }
        }
        "canopy.drums" => {
            for param in params {
                if param.id.contains("_l") && param.id.ends_with("_mode") {
                    set(param.id.to_owned(), 1.0);
                }
            }
        }
        _ => {}
    }
    out
}

fn variants(params: &[ParamDescriptor], busy: bool) -> Vec<(String, Variant)> {
    let mut out = vec![("defaults".to_owned(), Variant::Defaults)];
    if busy {
        out.push(("plain".to_owned(), Variant::Plain));
    }
    for (index, param) in params.iter().enumerate() {
        match param.kind {
            ParameterKind::Enum { labels, .. } => {
                for (choice, label) in labels.iter().enumerate() {
                    let value = choice as f32;
                    if value != param.default_value() {
                        out.push((
                            format!("{}={}", param.id, label),
                            Variant::Switch(index, value),
                        ));
                    }
                }
            }
            ParameterKind::Float { min, max, .. } => {
                if min != max {
                    out.push((format!("{}:turn", param.id), Variant::Turn(index)));
                    out.push((format!("{}:sweep", param.id), Variant::Sweep(index)));
                }
            }
        }
    }
    out
}

/// Something with parameters that renders a block.
trait Device {
    fn params(&self) -> &'static [ParamDescriptor];
    fn set_param(&mut self, index: usize, value: f32);
    fn modulate(&mut self, index: usize, value: f32);
}

/// Applies `variant` before block `block` of `blocks`, over `base`.
fn drive(
    device: &mut dyn Device,
    base: &[(usize, f32)],
    variant: Variant,
    block: usize,
    blocks: usize,
) {
    let params = device.params();
    if block == 0 && !matches!(variant, Variant::Plain) {
        for &(index, value) in base {
            device.set_param(index, value);
        }
    }
    match variant {
        Variant::Plain | Variant::Defaults => {}
        Variant::Switch(index, value) => {
            if block == blocks / 2 {
                device.set_param(index, value);
            }
        }
        Variant::Turn(index) => {
            if block == 0 {
                device.set_param(index, params[index].from_normalized(0.0));
            } else if block == blocks / 2 {
                device.set_param(index, params[index].from_normalized(1.0));
            }
        }
        Variant::Sweep(index) => {
            let t = block as f32 * BLOCK as f32 / RATE * 4.0;
            let triangle = 1.0 - (2.0 * (t - t.floor()) - 1.0).abs();
            device.modulate(index, params[index].from_normalized(triangle));
        }
    }
}

struct EffectDevice(Box<dyn effects::Effect>);

impl Device for EffectDevice {
    fn params(&self) -> &'static [ParamDescriptor] {
        self.0.params()
    }
    fn set_param(&mut self, index: usize, value: f32) {
        self.0.set_param(index, value);
    }
    fn modulate(&mut self, index: usize, value: f32) {
        self.0.modulate(index, value);
    }
}

struct InstrumentDevice(Box<dyn Instrument>);

impl Device for InstrumentDevice {
    fn params(&self) -> &'static [ParamDescriptor] {
        self.0.params()
    }
    fn set_param(&mut self, index: usize, value: f32) {
        self.0.set_param(index, value);
    }
    fn modulate(&mut self, index: usize, value: f32) {
        self.0.modulate(index, value);
    }
}

fn noise(len: usize, mut seed: u32) -> Vec<f32> {
    (0..len)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            0.5 * (seed as f32 / u32::MAX as f32 * 2.0 - 1.0) + 0.3 * (i as f32 * 0.02).sin()
        })
        .collect()
}

fn render_effect(descriptor: &effects::EffectDescriptor, variant: Variant) -> u64 {
    let mut effect = EffectDevice((descriptor.create)());
    effect.0.prepare(RATE, BLOCK);
    let base = busy(descriptor.id, descriptor.params);
    let mut left = noise(EFFECT_FRAMES, 0x2545_f491);
    let mut right = noise(EFFECT_FRAMES, 0x1234_5678);
    let side_l = noise(EFFECT_FRAMES, 0x0bad_cafe);
    let side_r = noise(EFFECT_FRAMES, 0x0dec_0ded);
    let blocks = EFFECT_FRAMES.div_ceil(BLOCK);
    let mut meters = Vec::with_capacity(blocks * effects::MAX_METERS);
    for (block, (l, r)) in left
        .chunks_mut(BLOCK)
        .zip(right.chunks_mut(BLOCK))
        .enumerate()
    {
        drive(&mut effect, &base, variant, block, blocks);
        let at = block * BLOCK;
        let context = Context {
            beat: at as f64 / f64::from(RATE) * 2.0,
            playing: true,
            sidechain: descriptor
                .sidechain
                .then(|| (&side_l[at..at + l.len()], &side_r[at..at + r.len()])),
            ..Context::DEFAULT
        };
        effect.0.process(l, r, &context);
        let mut read = [0.0; effects::MAX_METERS];
        effect.0.meters(&mut read);
        meters.extend_from_slice(&read);
    }
    hash(&left, &right, &meters)
}

/// Chords on the instrument's own range, with a bend, the mod wheel and an
/// all-notes-off, so every path a note takes is heard.
fn events_for(id: &str, frame: usize) -> Vec<NoteEventKind> {
    let pads = drum_notes();
    let chord: &[u8] = match id {
        "canopy.drums" => &pads,
        "canopy.guitar" => &[40, 45, 50, 55, 59, 64],
        _ => &[48, 55, 60, 64, 67, 72],
    };
    let period = INSTRUMENT_FRAMES / 4;
    let phase = frame % period;
    let round = frame / period;
    let velocity = |i: usize| Velocity::from_midi1(60 + ((i * 13 + round * 29) % 67) as u8);
    let mut out = Vec::new();
    if phase == 0 {
        for (i, note) in chord.iter().enumerate() {
            out.push(NoteEventKind::On {
                pitch: Pitch::new(note + round as u8).unwrap(),
                velocity: velocity(i),
            });
        }
    }
    if phase == period / 2 {
        for note in chord {
            out.push(NoteEventKind::Off {
                pitch: Pitch::new(note + round as u8).unwrap(),
                velocity: Velocity::CENTER,
            });
        }
    }
    if phase == period / 4 {
        out.push(NoteEventKind::PitchBend(if round.is_multiple_of(2) {
            4000
        } else {
            -6000
        }));
        out.push(NoteEventKind::Control {
            controller: MOD_WHEEL,
            value: (round * 40 % 128) as u8,
        });
    }
    if frame == INSTRUMENT_FRAMES - period / 3 {
        out.push(NoteEventKind::AllNotesOff);
    }
    out
}

/// A note for every drum pad: the first that plays it.
fn drum_notes() -> Vec<u8> {
    let mut seen = Vec::new();
    let mut notes = Vec::new();
    for note in 0..120u8 {
        if let Some(voice) = instruments::builtin::drums::voice_for_note(note)
            && !seen.contains(&voice)
        {
            seen.push(voice);
            notes.push(note);
        }
    }
    notes
}

/// The sampler's synthetic bank: a looped sine and a one-shot, two
/// velocity layers, a release trigger and round robins.
fn sampler_bank() -> InstrumentData {
    let decode = |path: &Path| -> Result<Decoded, String> {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let hz: f64 = name
            .trim_start_matches(char::is_alphabetic)
            .parse()
            .unwrap_or(220.0);
        let rate = 44_100.0;
        let samples = (0..22_050)
            .map(|i| (std::f64::consts::TAU * hz * i as f64 / rate).sin() as f32 * 0.7)
            .collect();
        Ok(Decoded::mono(samples, 44_100))
    };
    let text = "\
<group> loop_mode=loop_continuous loop_start=100 loop_end=20000 ampeg_release=0.2
<region> sample=soft220.wav lovel=0 hivel=90 pitch_keycenter=57 seq_length=2 seq_position=1
<region> sample=soft230.wav lovel=0 hivel=90 pitch_keycenter=57 seq_length=2 seq_position=2
<region> sample=loud440.wav lovel=91 hivel=127 pitch_keycenter=69
<group> trigger=release
<region> sample=rel880.wav pitch_keycenter=81 ampeg_release=0.05
";
    let bank = sampler::load_str(text, Path::new("/golden/golden.sfz"), &decode).unwrap();
    Arc::new(bank)
}

/// User tables for the wavetable's oscillators: a morphing pulse.
fn wavetable_tables() -> InstrumentData {
    let frames = 8;
    let size = 2048;
    let samples: Vec<f32> = (0..frames * size)
        .map(|i| {
            let (frame, at) = (i / size, i % size);
            let width = 0.1 + 0.1 * frame as f32;
            if (at as f32 / size as f32) < width {
                0.8
            } else {
                -0.8
            }
        })
        .collect();
    let table = Arc::new(UserTable::from_samples(&samples, size).unwrap());
    Arc::new(UserTables {
        tables: [Some(table.clone()), Some(table.clone()), Some(table)],
    })
}

/// The slicer's synthetic loop: sixteen hits, each a decaying tone of its
/// own pitch with a click on top, played a little fast.
fn slicer_loop() -> InstrumentData {
    let hit = 3000;
    let audio: Vec<f32> = (0..16 * hit)
        .map(|i| {
            let (k, at) = (i / hit, (i % hit) as f32);
            let hz = 110.0 * (1.0 + k as f32 * 0.37);
            let decay = (-at / 600.0).exp();
            let click = if at < 20.0 { 0.5 } else { 0.0 };
            (std::f32::consts::TAU * hz * at / 44_100.0).sin() * 0.7 * decay + click
        })
        .collect();
    Arc::new(SliceData {
        left: audio.iter().map(|s| s * 0.9).collect(),
        right: audio.into(),
        sample_rate: 44_100.0,
        starts: (0..16).map(|k| k * hit).collect(),
        speed: 1.25,
    })
}

fn render_instrument(
    descriptor: &instruments::BuiltInInstrumentDescriptor,
    variant: Variant,
) -> u64 {
    let mut instrument = InstrumentDevice((descriptor.create)());
    match descriptor.id {
        "canopy.sampler" => instrument.0.set_data(sampler_bank()),
        "canopy.wavetable" => instrument.0.set_data(wavetable_tables()),
        "canopy.slicer" => instrument.0.set_data(slicer_loop()),
        _ => {}
    }
    instrument.0.prepare(RATE, BLOCK);
    instrument.0.set_tempo(124.0);
    instrument.0.reset();
    let base = busy(descriptor.id, descriptor.params);
    let mut left = vec![0.0f32; INSTRUMENT_FRAMES];
    let mut right = vec![0.0f32; INSTRUMENT_FRAMES];
    let blocks = INSTRUMENT_FRAMES.div_ceil(BLOCK);
    let mut meters = Vec::with_capacity(blocks * instruments::MAX_METERS);
    for (block, (l, r)) in left
        .chunks_mut(BLOCK)
        .zip(right.chunks_mut(BLOCK))
        .enumerate()
    {
        drive(&mut instrument, &base, variant, block, blocks);
        let at = block * BLOCK;
        let events: Vec<NoteEvent> = (at..at + l.len())
            .flat_map(|frame| {
                events_for(descriptor.id, frame)
                    .into_iter()
                    .map(move |kind| NoteEvent {
                        frame: (frame - at) as u32,
                        kind,
                    })
            })
            .collect();
        instrument.0.process(&events, l, r);
        let mut read = [0.0; instruments::MAX_METERS];
        instrument.0.meters(&mut read);
        meters.extend_from_slice(&read);
    }
    hash(&left, &right, &meters)
}

/// One job: a device, a variant, and what it rendered.
struct Job {
    line: String,
    run: Box<dyn Fn() -> u64 + Send + Sync>,
}

fn main() {
    let out = std::env::args().nth(1).expect("an output file");
    let only = std::env::args().nth(2);
    let mut jobs: Vec<Job> = Vec::new();
    let has_busy = |id: &str| {
        matches!(
            id,
            "canopy.eq" | "canopy.shaper" | "canopy.wavetable" | "canopy.drums"
        )
    };
    for descriptor in effects::builtins() {
        for (name, variant) in variants(descriptor.params, has_busy(descriptor.id)) {
            jobs.push(Job {
                line: format!("effect {} {name}", descriptor.id),
                run: Box::new(move || render_effect(descriptor, variant)),
            });
        }
    }
    for descriptor in instruments::builtins() {
        for (name, variant) in variants(descriptor.params, has_busy(descriptor.id)) {
            jobs.push(Job {
                line: format!("instrument {} {name}", descriptor.id),
                run: Box::new(move || render_instrument(descriptor, variant)),
            });
        }
    }
    if let Some(only) = &only {
        jobs.retain(|job| job.line.contains(only.as_str()));
    }
    let start = Instant::now();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<u64>>> =
        jobs.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(job) = jobs.get(i) else { break };
                    *results[i].lock().unwrap() = Some((job.run)());
                }
            });
        }
    });
    let mut text = String::new();
    for (job, result) in jobs.iter().zip(&results) {
        let hash = result.lock().unwrap().expect("rendered");
        writeln!(text, "{} {hash:016x}", job.line).unwrap();
    }
    std::fs::write(&out, text).expect("write the hashes");
    println!(
        "{} renders in {:.1} s, written to {out}",
        jobs.len(),
        start.elapsed().as_secs_f64()
    );
}
