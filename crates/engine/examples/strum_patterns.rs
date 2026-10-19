//! Renders the strum pattern tool's demos: a chord progression held in a
//! MIDI clip, strummed with one of the tool's presets
//! (`project::note_tools::strum_pattern`, as the piano roll's Strum
//! Pattern window applies it) and played by the Guitar through the engine,
//! the way an export plays a project.
//!
//! ```text
//! cargo run --release -p engine --example strum_patterns            # into demos/guitar/
//! cargo run --release -p engine --example strum_patterns -- <dir>   # somewhere else
//! ```
//!
//! Each file is 24-bit stereo WAV at 48 kHz, its peak at -1 dBFS like the
//! Guitar's other demos (`instruments/examples/guitar_demos.rs`).

use std::path::{Path, PathBuf};

use engine::{Bounce, Compiler};
use instruments::InstrumentRef;
use media::{AudioBuffer, WavFormat};
use midi::{Pitch, Tick, Velocity};
use project::note_tools::patterns::{Articulation, PRESETS, StrumPattern, strum_pattern};
use project::{NewTrack, Note, Project, TrackPosition};

const RATE: u32 = 48_000;
const BAR: i64 = 3840;

/// One demo: a file name, what it shows, the Guitar's settings, the tempo,
/// the chords (notes, bars), the preset and how much to humanize it.
struct Demo {
    name: &'static str,
    about: &'static str,
    settings: &'static [(&'static str, f32)],
    bpm: f64,
    chords: &'static [(&'static [u8], i64)],
    preset: &'static str,
    humanize: f32,
}

const G: &[u8] = &[43, 47, 50, 55, 59, 67];
const EM: &[u8] = &[40, 47, 52, 55, 59, 64];
const C: &[u8] = &[48, 52, 55, 60, 64];
const D: &[u8] = &[50, 57, 62, 66];
const E9: &[u8] = &[52, 56, 62, 66, 71];
const A9: &[u8] = &[45, 49, 55, 59, 64];

const DEMOS: &[Demo] = &[
    Demo {
        name: "15_pattern_tool_folk",
        about: "steel-string acoustic: G Em C D held two bars each, strummed by the \
                Folk / pop preset (compare 05, played by hand), a last G left to ring",
        settings: &[("model", 0.0)],
        bpm: 96.0,
        chords: &[(G, 2), (EM, 2), (C, 2), (D, 2)],
        preset: "Folk / pop",
        humanize: 0.0,
    },
    Demo {
        name: "16_pattern_tool_funk",
        about: "electric, middle and bridge pickups, clean amp: E9 and A9 strummed by the \
                Funk sixteenths preset, chucks as dead notes through the key-switches",
        settings: &[
            ("model", 2.0),
            ("pickup", 3.0),
            ("amp", 1.0),
            ("drive", 0.35),
        ],
        bpm: 100.0,
        chords: &[(E9, 2), (A9, 1), (E9, 1), (E9, 2), (A9, 1), (E9, 1)],
        preset: "Funk sixteenths",
        humanize: 0.2,
    },
];

/// The demo's project: one Guitar track, its chords held in one clip, the
/// chords strummed and a last one left to ring.
fn project(demo: &Demo) -> Project {
    let mut project = Project::new();
    project.set_tempo_bpm(demo.bpm);
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .expect("a new track");
    project
        .set_instrument(track, InstrumentRef::builtin("canopy.guitar"))
        .expect("a MIDI track");
    for &(id, value) in demo.settings {
        project
            .set_instrument_param(track, id, value)
            .expect("a Guitar setting");
    }
    let bars: i64 = demo.chords.iter().map(|c| c.1).sum();
    let clip = project
        .add_midi_clip(track, Tick::ZERO, Tick::new((bars + 1) * BAR))
        .expect("a clip");

    let mut notes = Vec::new();
    let mut held = Vec::new();
    let mut at = 0;
    let last = demo.chords[0].0;
    for &(pitches, length) in demo.chords.iter().chain([&(last, 1)]) {
        for &pitch in pitches {
            let note = Note {
                id: project.new_note_id(),
                start: Tick::new(at * BAR),
                len: Tick::new(length * BAR),
                pitch: Pitch::new(pitch).expect("a MIDI note"),
                velocity: Velocity::from_midi1(100),
                release_velocity: Velocity::CENTER,
                muted: false,
            };
            // Every chord but the last is strummed.
            if at < bars {
                held.push(note.id);
            }
            notes.push(note);
        }
        at += length;
    }

    let preset = PRESETS
        .iter()
        .find(|p| p.name == demo.preset)
        .expect("a preset");
    let mut settings = StrumPattern {
        humanize: demo.humanize,
        ..StrumPattern::default()
    };
    preset.apply(&mut settings);
    let articulation = Articulation::for_track(project.track(track).expect("the track"));
    let bar = project.tempo.bar_len();
    let strummed = strum_pattern(
        &notes,
        &held,
        &settings,
        articulation,
        Tick::ZERO,
        bar,
        &mut || project.new_note_id(),
    );
    project.set_notes(clip, strummed).expect("the clip");
    project
}

/// Plays `project` to the end and on until it has rung out.
fn bounce(project: &Project) -> AudioBuffer {
    let arrangement = Compiler::new(RATE).compile(project, |_| None);
    let end = project.tempo.tick_to_sample(project.end(), RATE) as u64;
    let mut bounce = Bounce::new(arrangement, RATE, end, true);
    while !bounce.step(8192) {}
    bounce.finish()
}

/// `audio` with its peak at -1 dBFS if it would be louder; quiet takes are
/// left alone.
fn normalised(audio: &AudioBuffer) -> AudioBuffer {
    let peak = audio
        .left()
        .iter()
        .chain(audio.right())
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let limit = 10f32.powf(-1.0 / 20.0);
    let scale = if peak > limit { limit / peak } else { 1.0 };
    let side = |samples: &[f32]| samples.iter().map(|s| s * scale).collect();
    AudioBuffer::new(RATE, side(audio.left()), side(audio.right()))
}

fn main() {
    let out: PathBuf = std::env::args().nth(1).map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demos/guitar"),
        PathBuf::from,
    );
    for demo in DEMOS {
        let audio = normalised(&bounce(&project(demo)));
        let path = out.join(format!("{}.wav", demo.name));
        media::write_wav_as(&path, &audio, WavFormat::Pcm24)
            .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
        println!(
            "{:<22} {:>5.1} s  {}",
            demo.name,
            audio.duration_seconds(),
            demo.about
        );
    }
}
