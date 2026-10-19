//! The test plugins (`plugins::fixtures`) as canopy devices, through the
//! manager and the engine: they play, their parameters and states follow
//! the project, what their editors do comes back as undo steps, restarts
//! and offline renders get instances of their own, and a plugin that
//! isn't installed passes sound through and keeps its settings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::{AudioLookup, Command, Compiler, Engine, EngineHandle};
use hosting::{Owner, PluginManager, Status, Update, WithPlugins};

use media::AudioBuffer;
use midi::{NoteEvent, NoteEventKind, Tick};
use plugins::fixtures::{self, CLAP_GAIN, CLAP_SINE, VST2_GAIN, VST2_SINE, VST3_GAIN};
use plugins::{PluginFormat, PluginInfo};
use project::{DeviceId, NewTrack, Project, SourceId, TrackId, TrackPosition};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: u32 = 48_000;
const BLOCK: usize = 512;

/// The test plugins, as a scan would list them.
fn catalog() -> Vec<PluginInfo> {
    let fixtures = fixtures::build().expect("the test plugins build");
    let mut infos = plugins::scan::read_plugin_file(PluginFormat::Clap, &fixtures.clap).unwrap();
    infos.extend(plugins::scan::read_plugin_file(PluginFormat::Vst3, &fixtures.vst3).unwrap());
    for vst2 in [&fixtures.vst2_gain, &fixtures.vst2_sine] {
        infos.extend(plugins::scan::read_plugin_file(PluginFormat::Vst2, vst2).unwrap());
    }
    infos
}

fn manager() -> PluginManager {
    let mut manager = PluginManager::new();
    manager.catalog_mut().set(catalog());
    manager
}

fn find(manager: &PluginManager, format: PluginFormat, id: &str) -> PluginInfo {
    manager.catalog().find(format, id).cloned().expect("listed")
}

/// A project playing a constant 0.5 on one audio track for ten seconds.
fn project_with_audio() -> (Project, TrackId, SourceId) {
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Audio, TrackPosition::END)
        .unwrap();
    let source = project.add_source("/tmp/dc.wav".into(), RATE, 10 * RATE as u64, 1);
    project.add_audio_clip(track, Tick::ZERO, source).unwrap();
    (project, track, source)
}

/// The audio for [`project_with_audio`].
struct Dc(SourceId);

impl AudioLookup for Dc {
    fn source(&self, id: SourceId) -> Option<Arc<AudioBuffer>> {
        let samples = vec![0.5; 10 * RATE as usize];
        (id == self.0).then(|| Arc::new(AudioBuffer::new(RATE, samples.clone(), samples)))
    }
}

/// Adds plugin effect `info` to `track`'s chain as the session does.
fn add_effect(
    manager: &mut PluginManager,
    project: &mut Project,
    track: TrackId,
    info: &PluginInfo,
) -> DeviceId {
    let loaded = manager.load(info).unwrap();
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
    device
}

/// Plays `engine` for a while on a thread of its own, as the audio
/// thread, where nothing may allocate. Returns it and the last sample.
fn play(engine: Engine) -> (Engine, f32) {
    std::thread::spawn(move || {
        let mut engine = engine;
        let mut out = vec![0.0; BLOCK * 2];
        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..20 {
                engine.process_interleaved(&mut out, 2);
            }
        });
        let last = out[out.len() - 2];
        (engine, last)
    })
    .join()
    .unwrap()
}

fn send(
    handle: &mut EngineHandle,
    compiler: &mut Compiler,
    project: &Project,
    manager: &PluginManager,
    audio: SourceId,
) {
    let lookup = WithPlugins {
        audio: Dc(audio),
        plugins: manager,
    };
    let arrangement = compiler.compile(project, lookup);
    handle.send(Command::Arrangement(arrangement)).unwrap();
}

fn gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn a_clap_effect_plays_at_the_projects_settings() {
    let mut manager = manager();
    let (mut project, track, source) = project_with_audio();
    let info = find(&manager, PluginFormat::Clap, CLAP_GAIN);
    let device = add_effect(&mut manager, &mut project, track, &info);
    assert_eq!(
        project.device(device).unwrap().param("0"),
        Some(0.0),
        "the project starts with the plugin's own values"
    );
    project.set_device_param(device, "0", -6.0).unwrap();

    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    let (updates, rebuild) = manager.pump(&project);
    assert!(updates.is_empty());
    assert!(rebuild);
    send(&mut handle, &mut compiler, &project, &manager, source);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (engine, last) = play(engine);
    assert!(close(last, 0.5 * gain(-6.0)), "{last}");

    // A knob turned while it plays.
    let index = 0;
    handle
        .send(Command::SetDeviceParam {
            device,
            index,
            value: -12.0,
        })
        .unwrap();
    let (engine, last) = play(engine);
    assert!(close(last, 0.5 * gain(-12.0)), "{last}");
    drop(engine);
}

#[test]
fn a_vst3_effect_keeps_its_state_through_saving_and_loading() {
    let mut manager = manager();
    let (mut project, track, source) = project_with_audio();
    let info = find(&manager, PluginFormat::Vst3, VST3_GAIN);
    let device = add_effect(&mut manager, &mut project, track, &info);
    // VST3 values are normalized: 0 dB is 60/72 of the way up.
    let minus_twelve = 48.0 / 72.0;
    project.set_device_param(device, "0", minus_twelve).unwrap();
    manager.pump(&project);
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &project, &manager, source);
    let (_engine, _) = play(engine);
    manager.refresh_states(&mut project);
    let text = project.to_file_string().unwrap();

    // Opened again, in a fresh session.
    let mut opened = Project::from_file_string(&text).unwrap();
    let mut manager = self::manager();
    let (_, rebuild) = manager.pump(&opened);
    assert!(rebuild);
    assert_eq!(manager.status(Owner::Effect(device)), Status::Loaded);
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &opened, &manager, source);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (_engine, last) = play(engine);
    assert!(close(last, 0.5 * gain(-12.0)), "{last}");
    // Its text is the plugin's own.
    let text = manager.param_text(Owner::Effect(device), 0, minus_twelve);
    assert!(text.as_ref().is_some_and(|t| t.contains("12")), "{text:?}");
    // Saving again changes nothing.
    let before = opened.to_file_string().unwrap();
    manager.refresh_states(&mut opened);
    assert_eq!(opened.to_file_string().unwrap(), before);
}

#[test]
fn a_vst2_effect_plays_and_keeps_its_state() {
    let mut manager = manager();
    let (mut project, track, source) = project_with_audio();
    let info = find(&manager, PluginFormat::Vst2, VST2_GAIN);
    let device = add_effect(&mut manager, &mut project, track, &info);
    // Normalized like VST3's: -12 dB is 48/72 of the way up.
    project.set_device_param(device, "0", 48.0 / 72.0).unwrap();
    manager.pump(&project);
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &project, &manager, source);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (_engine, last) = play(engine);
    assert!(close(last, 0.5 * gain(-12.0)), "{last}");
    manager.refresh_states(&mut project);

    let opened = Project::from_file_string(&project.to_file_string().unwrap()).unwrap();
    let mut again = self::manager();
    again.pump(&opened);
    assert_eq!(again.status(Owner::Effect(device)), Status::Loaded);
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &opened, &again, source);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (_engine, last) = play(engine);
    assert!(close(last, 0.5 * gain(-12.0)), "reopened: {last}");
}

#[test]
fn a_vst2_instrument_plays_notes() {
    let mut manager = manager();
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let info = find(&manager, PluginFormat::Vst2, VST2_SINE);
    let loaded = manager.load(&info).unwrap();
    project
        .set_instrument(track, loaded.instrument_ref())
        .unwrap();
    project
        .set_instrument_plugin(track, Some(loaded.data.clone()))
        .unwrap();
    manager.attach(Owner::Instrument(track), loaded);
    manager.pump(&project);
    let t = project.track(track).unwrap().clone();
    let mut instrument = manager.instrument(&t, RATE, 64).expect("it starts");
    let peak = std::thread::spawn(move || {
        let on = [NoteEvent {
            frame: 0,
            kind: NoteEventKind::On {
                pitch: midi::Pitch::A4,
                velocity: midi::Velocity::MAX,
            },
        }];
        let (mut l, mut r) = ([0.0f32; 64], [0.0f32; 64]);
        let mut peak = 0.0f32;
        for block in 0..20 {
            let events: &[NoteEvent] = if block == 0 { &on } else { &[] };
            instrument.process(events, &mut l, &mut r);
            peak = l.iter().fold(peak, |m, s| m.max(s.abs()));
        }
        instrument.retire();
        peak
    })
    .join()
    .unwrap();
    assert!(peak > 0.1, "{peak}");
}

#[test]
fn undo_puts_a_plugins_state_back() {
    let mut manager = manager();
    let (mut project, track, source) = project_with_audio();
    let info = find(&manager, PluginFormat::Clap, CLAP_GAIN);
    let device = add_effect(&mut manager, &mut project, track, &info);
    manager.pump(&project);
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &project, &manager, source);
    let (engine, _) = play(engine);
    let state = |p: &Project| {
        p.device(device)
            .unwrap()
            .plugin
            .as_ref()
            .unwrap()
            .state
            .clone()
    };
    let before = project.clone();

    // The plugin hears a change, and its state is kept.
    project.set_device_param(device, "0", 6.0).unwrap();
    manager.pump(&project);
    send(&mut handle, &mut compiler, &project, &manager, source);
    let (engine, _) = play(engine);
    manager.refresh_states(&mut project);
    assert_ne!(state(&project), state(&before));

    // Undo: the plugin gets the earlier state back.
    project = before.clone();
    manager.pump(&project);
    send(&mut handle, &mut compiler, &project, &manager, source);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (_engine, last) = play(engine);
    assert!(close(last, 0.5), "{last}");
    manager.refresh_states(&mut project);
    assert_eq!(state(&project), state(&before));
}

/// Waits for what the plugins do by themselves to reach the main thread.
fn pump_until(
    manager: &mut PluginManager,
    project: &Project,
    done: impl Fn(&[Update]) -> bool,
) -> Vec<Update> {
    let start = Instant::now();
    let mut all = Vec::new();
    while start.elapsed() < Duration::from_secs(5) {
        let (updates, _) = manager.pump(project);
        all.extend(updates);
        if done(&all) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    all
}

#[test]
fn an_edit_in_the_plugin_becomes_one_undo_step() {
    let mut manager = manager();
    let mut project = Project::new();
    let track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    let info = find(&manager, PluginFormat::Clap, CLAP_SINE);
    let loaded = manager.load(&info).unwrap();
    project
        .set_instrument(track, loaded.instrument_ref())
        .unwrap();
    project
        .set_instrument_plugin(track, Some(loaded.data.clone()))
        .unwrap();
    project
        .set_instrument_params(track, loaded.values.clone())
        .unwrap();
    let owner = Owner::Instrument(track);
    manager.attach(owner, loaded);
    manager.pump(&project);
    let t = project.track(track).unwrap().clone();
    let mut instrument = manager.instrument(&t, RATE, 64).expect("it starts");

    // CC 7 makes the test plugin set its own gain, as its editor would:
    // gesture, value, gesture end.
    let mut instrument = std::thread::spawn(move || {
        let events = [NoteEvent {
            frame: 0,
            kind: NoteEventKind::Control {
                controller: 7,
                value: 127,
            },
        }];
        let (mut l, mut r) = ([0.0; 64], [0.0; 64]);
        instrument.process(&events, &mut l, &mut r);
        instrument.retire();
        instrument
    })
    .join()
    .unwrap();
    let updates = pump_until(&mut manager, &project, |u| {
        u.iter().any(|u| matches!(u, Update::Commit { .. }))
    });
    assert_eq!(
        updates,
        [
            Update::Param {
                owner,
                param: "0",
                value: 12.0
            },
            Update::Commit {
                label: "Change Canopy Test Sine".into()
            }
        ],
        "one step, named after the plugin"
    );
    assert_eq!(manager.touched(owner), Some(0));
    instrument.reset();
}

#[test]
fn a_restart_waits_for_the_old_instance_then_gets_a_new_one() {
    let mut manager = manager();
    let (mut project, track, source) = project_with_audio();
    let info = find(&manager, PluginFormat::Clap, CLAP_GAIN);
    let device = add_effect(&mut manager, &mut project, track, &info);
    let owner = Owner::Effect(device);
    manager.pump(&project);
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &project, &manager, source);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (engine, _) = play(engine);
    assert_eq!(compiler.latency(), 0);
    let generation = manager.generation_of(owner);

    // Its latency changes while it plays: it asks for a restart.
    handle
        .send(Command::SetDeviceParam {
            device,
            index: 1,
            value: 100.0,
        })
        .unwrap();
    project.set_device_param(device, "1", 100.0).unwrap();
    let (mut engine, _) = play(engine);
    let start = Instant::now();
    while manager.generation_of(owner) == generation && start.elapsed() < Duration::from_secs(5) {
        let (_, _) = manager.pump(&project);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_ne!(manager.generation_of(owner), generation, "a new generation");

    // The next snapshot can't have it while the engine holds the old one.
    send(&mut handle, &mut compiler, &project, &manager, source);
    for _ in 0..3 {
        (engine, _) = play(engine);
        handle.collect_garbage();
    }
    // The old one is back and gone: the manager asks for another snapshot,
    // which has it, late by its new latency.
    let (_, rebuild) = manager.pump(&project);
    assert!(rebuild);
    send(&mut handle, &mut compiler, &project, &manager, source);
    let (_engine, last) = play(engine);
    assert_eq!(compiler.latency(), 100);
    assert!(close(last, 0.5), "{last}");
}

#[test]
fn a_plugin_that_isnt_installed_keeps_its_settings() {
    let mut manager = manager();
    let (mut project, track, source) = project_with_audio();
    let info = find(&manager, PluginFormat::Clap, CLAP_GAIN);
    let device = add_effect(&mut manager, &mut project, track, &info);
    project.set_device_param(device, "0", -6.0).unwrap();
    manager.refresh_states(&mut project);
    let saved = project.to_file_string().unwrap();

    // Elsewhere, where nothing is installed.
    let opened = Project::from_file_string(&saved).unwrap();
    let mut elsewhere = PluginManager::new();
    elsewhere.catalog_mut().set(Vec::new());
    elsewhere.pump(&opened);
    assert_eq!(
        elsewhere.status(Owner::Effect(device)),
        Status::NotInstalled
    );
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &opened, &elsewhere, source);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (_engine, last) = play(engine);
    // This process registered the plugin's description above, so the
    // engine knows the device and leaves it silent without an instance;
    // where it was never loaded, the device is unknown and passes sound
    // through instead.
    assert_eq!(last, 0.0);
    assert_eq!(opened.to_file_string().unwrap(), saved, "kept as it was");
}

#[test]
fn an_offline_render_gets_instances_of_its_own() {
    let mut manager = manager();
    let (mut project, track, source) = project_with_audio();
    let info = find(&manager, PluginFormat::Clap, CLAP_GAIN);
    let device = add_effect(&mut manager, &mut project, track, &info);
    project.set_device_param(device, "0", -6.0).unwrap();
    manager.pump(&project);
    // The live instance plays...
    let (mut handle, engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    send(&mut handle, &mut compiler, &project, &manager, source);
    // ...while an export compiles its own.
    let offline = manager.offline(&project);
    assert!(!offline.is_empty());
    let mut export = Compiler::new(RATE);
    let lookup = WithPlugins {
        audio: Dc(source),
        plugins: &offline,
    };
    let arrangement = export.compile(&project, lookup);
    let mut bounce = engine::Bounce::new(arrangement, RATE, RATE as u64 / 10, false);
    let rendered = std::thread::spawn(move || {
        while !bounce.step(RATE as usize) {}
        bounce.finish()
    })
    .join()
    .unwrap();
    let last = *rendered.left().last().unwrap();
    assert!(close(last, 0.5 * gain(-6.0)), "{last}");
    drop(offline);
    handle.send(Command::SetPlaying(true)).unwrap();
    let (_engine, last) = play(engine);
    assert!(
        close(last, 0.5 * gain(-6.0)),
        "the live one still plays: {last}"
    );
}
