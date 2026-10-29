//! Real plugins, installed on this computer, through the manager and the
//! engine: each loads, describes its parameters, plays (instruments a
//! note, effects a tone), and comes back from its saved state sounding the
//! same. Ignored unless asked for, with the files to try:
//!
//! ```sh
//! CANOPY_REAL_PLUGINS=/usr/lib/clap/Vital.clap:/usr/lib/vst3/Vital.vst3 \
//!     cargo test -p hosting --test real_plugins -- --ignored --nocapture
//! ```
//!
//! Each file's format comes from its extension (`.clap`, `.vst3`, and
//! `.so`/`.dll` for VST2).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use engine::{AudioLookup, Command, Compiler};
use hosting::{Owner, PluginManager, WithPlugins};
use media::AudioBuffer;
use midi::{Pitch, Tick, Velocity};
use plugins::{PluginFormat, PluginInfo, PluginKind};
use project::{NewTrack, Project, SourceId, TrackPosition};

const RATE: u32 = 48_000;

fn files() -> Vec<PathBuf> {
    std::env::var_os("CANOPY_REAL_PLUGINS")
        .map(|list| std::env::split_paths(&list).collect())
        .unwrap_or_default()
}

fn format_of(path: &std::path::Path) -> Option<PluginFormat> {
    match path.extension()?.to_str()? {
        "clap" => Some(PluginFormat::Clap),
        "vst3" => Some(PluginFormat::Vst3),
        "so" | "dll" => PluginFormat::from_key("vst2"),
        _ => None,
    }
}

/// A sine at 220 Hz, for effects to change.
struct Tone(SourceId);

impl AudioLookup for Tone {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        let samples: Vec<f32> = (0..2 * RATE as usize)
            .map(|i| 0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / RATE as f32).sin())
            .collect();
        (id == self.0).then(|| Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)))
    }
}

/// Plays `project` for a second through `manager`'s plugins and returns
/// the left channel.
fn render(project: &Project, manager: &PluginManager, source: SourceId) -> Vec<f32> {
    let (mut handle, mut engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    let lookup = WithPlugins {
        audio: Tone(source),
        plugins: manager,
    };
    handle
        .send(Command::Arrangement(compiler.compile(project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();
    let mut out = vec![0.0; 1024];
    let mut left = Vec::new();
    for _ in 0..(RATE as usize / 512) {
        engine.process_interleaved(&mut out, 2);
        left.extend(out.chunks(2).map(|f| f[0]));
    }
    left
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

fn try_plugin(info: &PluginInfo) {
    let mut manager = PluginManager::new();
    manager.catalog_mut().set(vec![info.clone()]);
    let mut project = Project::new();
    let start = Instant::now();
    let loaded = manager.load(info).expect("it loads");
    let load_time = start.elapsed();
    let params = loaded.values.len();
    let (owner, source) = match info.kind {
        PluginKind::Instrument => {
            let track = project
                .add_track(NewTrack::Midi, TrackPosition::END)
                .unwrap();
            project
                .set_instrument(track, loaded.instrument_ref())
                .unwrap();
            project
                .set_instrument_plugin(track, Some(loaded.data.clone()))
                .unwrap();
            project
                .set_instrument_params(track, loaded.values.clone())
                .unwrap();
            let clip = project
                .add_midi_clip(track, Tick::ZERO, Tick::new(3840))
                .unwrap();
            let note = project::Note {
                id: project.new_note_id(),
                start: Tick::ZERO,
                len: Tick::new(1920),
                pitch: Pitch::A4,
                velocity: Velocity::MAX,
                release_velocity: Velocity::CENTER,
                muted: false,
                bend: Vec::new(),
            };
            project.set_notes(clip, vec![note]).unwrap();
            manager.attach(Owner::Instrument(track), loaded);
            (Owner::Instrument(track), SourceId(u32::MAX))
        }
        _ => {
            let track = project
                .add_track(NewTrack::Audio, TrackPosition::END)
                .unwrap();
            let source = project.add_source("/tmp/tone.wav".into(), RATE, 2 * RATE as u64, 1);
            project.add_audio_clip(track, Tick::ZERO, source).unwrap();
            let device = project
                .add_device(Some(track), loaded.effect_ref(), None)
                .unwrap();
            project
                .set_device_plugin(device, Some(loaded.data.clone()))
                .unwrap();
            project
                .set_device_params(device, loaded.values.clone())
                .unwrap();
            manager.attach(Owner::Effect(device), loaded);
            (Owner::Effect(device), source)
        }
    };
    let groups = hosting::describe::param_groups(hosting::describe::format(info.format), &info.id)
        .unwrap_or_default();
    let mut outer: Vec<&str> = groups.iter().map(|g| g[0]).collect();
    outer.sort();
    outer.dedup();
    println!("  menu groups: {}", outer.join(", "));
    manager.pump(&project);
    let first = render(&project, &manager, source);
    let level = rms(&first);
    println!(
        "{} ({}, {}): loaded in {:.0} ms, {params} parameters, level {level:.4}",
        info.name,
        info.format.name(),
        info.kind.name(),
        load_time.as_secs_f64() * 1000.0
    );
    assert!(level > 1e-4, "it makes sound");

    // Saved and opened again in a fresh manager: the same sound.
    manager.refresh_states(&mut project);
    let text = project.to_file_string().unwrap();
    let opened = Project::from_file_string(&text).unwrap();
    let mut again = PluginManager::new();
    again.catalog_mut().set(vec![info.clone()]);
    again.pump(&opened);
    assert_eq!(again.status(owner), hosting::Status::Loaded);
    let second = render(&opened, &again, source);
    let difference = rms(&first
        .iter()
        .zip(&second)
        .map(|(a, b)| a - b)
        .collect::<Vec<_>>());
    println!(
        "  reopened: level {:.4}, difference {difference:.5}",
        rms(&second)
    );
    assert!(rms(&second) > 1e-4, "it still makes sound");
}

#[test]
#[ignore = "needs CANOPY_REAL_PLUGINS"]
fn real_plugins_play_and_come_back_from_their_state() {
    let files = files();
    assert!(
        !files.is_empty(),
        "set CANOPY_REAL_PLUGINS to the plugin files to try"
    );
    for path in files {
        let format = format_of(&path).expect("a .clap, .vst3, .so or .dll");
        let infos = plugins::scan::read_plugin_file(format, &path).expect("it scans");
        for info in infos.iter().filter(|i| hosting::usable(i)) {
            try_plugin(info);
        }
    }
}
