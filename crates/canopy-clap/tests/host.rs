//! `canopy.clap` loaded the way a host loads it, through canopy's own
//! CLAP host (the `plugins` crate): what the file offers, the guitar's
//! parameters and state, and the guitar playing a riff (notes' own bends
//! included, as tuning note expressions) exactly as the built-in
//! instrument plays it in canopy.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use canopy_clap::bridge::velocity;
use canopy_clap::values::{to_host, to_plain};
use dsp::MAX_BLOCK;
use instruments::{BuiltInInstrumentDescriptor, Instrument, ParameterKind};
use midi::{NoteEvent, NoteEventKind, Pitch, Velocity};
use plugins::{
    AudioConfig, Event, Host, ParamId, PluginFormat, PluginInfo, PluginInstance, PluginKind,
    Transport, scan,
};

const RATE: f64 = 48_000.0;
const GUITAR: &str = "canopy.guitar";

/// The library cargo built for these tests, installed as `canopy.clap`
/// in a folder of the tests' own.
fn canopy_clap() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let name = if cfg!(windows) {
            "canopy_clap.dll"
        } else {
            "libcanopy_clap.so"
        };
        let exe = std::env::current_exe().unwrap();
        let deps = exe.parent().unwrap();
        // The newer of the two: `cargo test` builds the one in `deps`, and
        // the copy beside it is only refreshed by `cargo build`, so it can
        // be an older build's.
        let built = [deps.parent().unwrap(), deps]
            .into_iter()
            .map(|dir| dir.join(name))
            .filter(|path| path.is_file())
            .max_by_key(|path| path.metadata().and_then(|m| m.modified()).ok())
            .unwrap_or_else(|| panic!("no {name} beside {}", exe.display()));
        let file = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("clap")
            .join("canopy.clap");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Through another name, so a process that has the old file loaded
        // keeps its copy.
        let temporary = file.with_extension(format!("tmp{}", std::process::id()));
        std::fs::copy(&built, &temporary).unwrap();
        std::fs::rename(&temporary, &file).unwrap();
        file
    })
}

fn infos() -> Vec<PluginInfo> {
    scan::read_plugin_file(PluginFormat::Clap, canopy_clap()).unwrap()
}

fn load(host: &Host, id: &str) -> PluginInstance {
    let info = infos().into_iter().find(|p| p.id == id).unwrap();
    host.load(&info).unwrap()
}

fn guitar() -> &'static BuiltInInstrumentDescriptor {
    instruments::find_builtin(GUITAR).unwrap()
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |peak, s| peak.max(s.abs()))
}

#[test]
fn the_file_offers_every_built_in_instrument_but_the_sampler() {
    let infos = infos();
    let ids: Vec<&str> = infos.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "canopy.poly_synth",
            "canopy.wavetable",
            "canopy.drums",
            "canopy.guitar",
            "canopy.simple_osc"
        ]
    );
    let guitar = infos.iter().find(|p| p.id == GUITAR).unwrap();
    assert_eq!(guitar.name, "Canopy Guitar");
    assert_eq!(guitar.vendor, "canopy");
    assert_eq!(guitar.kind, PluginKind::Instrument);
    assert!(guitar.audio_inputs.is_empty());
    assert_eq!(guitar.audio_outputs.len(), 1);
    assert_eq!(guitar.audio_outputs[0].channels, 2);
}

#[test]
fn the_guitars_parameters_are_described_in_the_hosts_units() {
    let host = Host::new();
    let instance = load(&host, GUITAR);
    let infos = instance.params();
    assert_eq!(infos.len(), guitar().params.len());
    for (index, (info, param)) in infos.iter().zip(guitar().params).enumerate() {
        assert_eq!(info.id, ParamId(index as u32));
        assert_eq!(info.name, param.name);
        assert!(info.flags.automatable);
        assert!((info.default - to_host(param, param.default_value())).abs() < 1e-9);
        let default_text = instance.param_text(info.id, info.default);
        assert_eq!(
            default_text.as_deref(),
            Some(param.format(param.default_value()).as_str()),
            "{}",
            param.name
        );
        match param.kind {
            ParameterKind::Float { .. } => {
                assert_eq!((info.min, info.max), (0.0, 1.0), "{}", param.name);
                assert!(info.flags.modulatable && !info.flags.stepped);
            }
            ParameterKind::Enum { labels, .. } => {
                assert_eq!((info.min, info.max), (0.0, (labels.len() - 1) as f64));
                assert!(info.flags.stepped && info.flags.enumerated);
                let last = instance.param_from_text(info.id, labels[labels.len() - 1]);
                assert_eq!(last, Some(info.max), "{}", param.name);
            }
        }
    }
}

#[test]
fn the_guitars_state_round_trips_through_the_host() {
    let host = Host::new();
    let mut first = load(&host, GUITAR);
    first.set_param(ParamId(0), 0.3);
    first.set_param(ParamId(12), 2.0);
    let state = first.save_state().unwrap();
    assert!(String::from_utf8_lossy(&state).contains("\"canopy.guitar\""));
    let mut second = load(&host, GUITAR);
    second.load_state(&state).unwrap();
    for info in first.params() {
        assert_eq!(
            second.param_value(info.id),
            first.param_value(info.id),
            "{}",
            info.name
        );
    }
    assert_eq!(second.param_value(ParamId(12)), Some(2.0));
    let wavetable = load(&host, "canopy.wavetable").save_state().unwrap();
    assert!(second.load_state(&wavetable).is_err());
}

/// Something in the riff, at a frame from its start.
#[derive(Clone, Copy)]
enum Play {
    On(u8, u8),
    Off(u8),
    /// The pedal goes down; keys let go while it's down are released when
    /// it comes up (listed with the pedal's coming up).
    PedalDown,
    PedalUp(&'static [u8]),
    /// A key let go while the pedal is down.
    OffHeld(u8),
    Bend(i16),
    /// One note's own bend, in semitones: a tuning note expression.
    Tune(u8, f64),
    /// A parameter, in the host's units.
    Param(usize, f64),
}

/// Strums, one string of a chord bent up and back on its own, a chord held
/// by the pedal and bent, changes of tone, drive and guitar model, and
/// notes on frames away from engine-block boundaries.
const RIFF: &[(usize, Play)] = &[
    (0, Play::On(40, 100)),
    (0, Play::On(47, 90)),
    (0, Play::On(52, 80)),
    (1000, Play::Param(0, 0.3)),
    (4000, Play::Tune(52, 0.5)),
    (4064, Play::Tune(52, 1.25)),
    (4128, Play::Tune(52, 2.0)),
    (9000, Play::Tune(52, 0.0)),
    (12_000, Play::Off(40)),
    (12_000, Play::Off(47)),
    (12_000, Play::Off(52)),
    (13_000, Play::PedalDown),
    (13_100, Play::On(55, 110)),
    (13_100, Play::On(59, 100)),
    (13_100, Play::On(62, 90)),
    (20_000, Play::OffHeld(55)),
    (20_001, Play::OffHeld(59)),
    (20_002, Play::OffHeld(62)),
    (24_037, Play::Bend(4096)),
    (30_000, Play::PedalUp(&[55, 59, 62])),
    (30_500, Play::Bend(0)),
    (31_000, Play::Param(9, 0.8)),
    (31_000, Play::Param(12, 2.0)),
    (40_000, Play::On(64, 127)),
    (40_010, Play::On(67, 60)),
    (60_000, Play::Off(64)),
    (60_003, Play::Off(67)),
];

const RIFF_FRAMES: usize = 96_000;
const TEMPO: f64 = 100.0;

fn unit_velocity(midi: u8) -> f32 {
    Velocity::from_midi1(midi).as_f32()
}

fn bend_bytes(value: i16) -> [u8; 3] {
    let raw = (i32::from(value) + 8192) as u16;
    [0xE0, (raw & 0x7F) as u8, (raw >> 7) as u8]
}

/// The riff as the host sends it, at frames within the block.
fn host_events(start: usize, end: usize) -> Vec<Event> {
    let mut events = Vec::new();
    for &(at, play) in RIFF.iter().filter(|(at, _)| (start..end).contains(at)) {
        let frame = (at - start) as u32;
        let (channel, off_velocity) = (0, 0.5);
        events.push(match play {
            Play::On(key, v) => Event::NoteOn {
                frame,
                channel,
                key,
                velocity: unit_velocity(v),
            },
            Play::Off(key) | Play::OffHeld(key) => Event::NoteOff {
                frame,
                channel,
                key,
                velocity: off_velocity,
            },
            Play::PedalDown => Event::Midi {
                frame,
                data: [0xB0, 64, 127],
            },
            Play::PedalUp(_) => Event::Midi {
                frame,
                data: [0xB0, 64, 0],
            },
            Play::Bend(value) => Event::Midi {
                frame,
                data: bend_bytes(value),
            },
            Play::Tune(key, semitones) => Event::NoteTuning {
                frame,
                channel,
                key,
                semitones,
            },
            Play::Param(index, value) => Event::Param {
                frame,
                id: ParamId(index as u32),
                value,
            },
        });
    }
    events
}

fn transport() -> Transport {
    Transport {
        playing: true,
        tempo: TEMPO,
        ..Transport::STOPPED
    }
}

/// The riff through the plugin, in host blocks of `block` frames.
fn through_plugin(block: usize) -> (Vec<f32>, Vec<f32>) {
    let host = Host::new();
    let mut instance = load(&host, GUITAR);
    let mut processor = instance
        .activate(AudioConfig::new(RATE, block as u32))
        .unwrap();
    let (mut left, mut right) = (vec![0.0f32; RIFF_FRAMES], vec![0.0f32; RIFF_FRAMES]);
    for start in (0..RIFF_FRAMES).step_by(block) {
        let end = (start + block).min(RIFF_FRAMES);
        processor.process_stereo(
            &mut left[start..end],
            &mut right[start..end],
            None,
            &host_events(start, end),
            &transport(),
        );
    }
    instance.deactivate(processor);
    (left, right)
}

/// The riff through the built-in guitar, run as canopy's engine runs an
/// instrument: settled at its values, then in blocks of `MAX_BLOCK` with
/// each block's parameter changes applied at its start, and the sustain
/// pedal resolved before the instrument sees the notes.
fn through_canopy() -> (Vec<f32>, Vec<f32>) {
    let params = guitar().params;
    let mut instrument: Box<dyn Instrument> = (guitar().create)();
    instrument.prepare(RATE as f32, MAX_BLOCK);
    instrument.set_tempo(120.0);
    for (index, param) in params.iter().enumerate() {
        instrument.set_param(index, param.default_value());
    }
    instrument.reset();
    instrument.set_tempo(TEMPO);
    let (mut left, mut right) = (vec![0.0f32; RIFF_FRAMES], vec![0.0f32; RIFF_FRAMES]);
    for start in (0..RIFF_FRAMES).step_by(MAX_BLOCK) {
        let end = (start + MAX_BLOCK).min(RIFF_FRAMES);
        let mut events = Vec::new();
        for &(at, play) in RIFF.iter().filter(|(at, _)| (start..end).contains(at)) {
            let frame = (at - start) as u32;
            let pitch = |key| Pitch::new(key).unwrap();
            let mut note = |kind| events.push(NoteEvent { frame, kind });
            match play {
                Play::On(key, v) => note(NoteEventKind::On {
                    pitch: pitch(key),
                    velocity: velocity(f64::from(unit_velocity(v))),
                }),
                Play::Off(key) => note(NoteEventKind::Off {
                    pitch: pitch(key),
                    velocity: velocity(0.5),
                }),
                Play::PedalUp(keys) => {
                    for &key in keys {
                        note(NoteEventKind::Off {
                            pitch: pitch(key),
                            velocity: Velocity::CENTER,
                        });
                    }
                }
                Play::PedalDown | Play::OffHeld(_) => {}
                Play::Bend(value) => note(NoteEventKind::PitchBend(value)),
                Play::Tune(key, semitones) => note(NoteEventKind::NoteBend {
                    pitch: pitch(key),
                    cents: (semitones * 100.0).round() as i16,
                }),
                Play::Param(index, value) => {
                    instrument.set_param(index, to_plain(&params[index], value));
                }
            }
        }
        instrument.process(&events, &mut left[start..end], &mut right[start..end]);
    }
    (left, right)
}

fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

#[test]
fn the_guitar_plays_exactly_as_it_does_in_canopy() {
    let (left, right) = through_plugin(512);
    let (expected_left, expected_right) = through_canopy();
    assert!(peak(&left) > 0.01, "the riff sounds: {}", peak(&left));
    assert!(same_bits(&left, &expected_left), "left differs");
    assert!(same_bits(&right, &expected_right), "right differs");
}

#[test]
fn any_host_block_size_plays_the_guitar() {
    let (left, right) = through_plugin(100);
    assert!(left.iter().chain(&right).all(|s| s.is_finite()));
    assert!(peak(&left) > 0.01);
    // The pedal held the second chord until frame 30,000.
    assert!(peak(&left[28_000..30_000]) > 1e-3);
}

#[test]
fn every_packaged_instrument_plays_a_note() {
    let host = Host::new();
    for info in infos() {
        let mut instance = host.load(&info).unwrap();
        let mut processor = instance.activate(AudioConfig::new(RATE, 256)).unwrap();
        let frames = 256 * 94;
        let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        for start in (0..frames).step_by(256) {
            let on = Event::NoteOn {
                frame: 0,
                channel: 0,
                key: 60,
                velocity: 1.0,
            };
            let events = if start == 0 { vec![on] } else { vec![] };
            processor.process_stereo(
                &mut left[start..start + 256],
                &mut right[start..start + 256],
                None,
                &events,
                &Transport::STOPPED,
            );
        }
        assert!(left.iter().all(|s| s.is_finite()), "{}", info.id);
        assert!(peak(&left) > 1e-3, "{} is silent", info.id);
        instance.deactivate(processor);
    }
}
