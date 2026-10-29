//! Aborts the test binary if the engine allocates or frees memory while
//! processing audio. Allocation on the audio thread can block on a lock inside
//! the allocator, which shows up as random dropouts that are hard to trace.
//!
//! Only meaningful in debug builds: assert_no_alloc is a no-op under --release.

use std::sync::Arc;

use engine::{Command, Compiler};
use media::AudioBuffer;
use midi::{NoteEventKind, Pitch, Tick, Velocity};
use project::{
    AutomationPoint, AutomationTarget, Cable, CableKey, EffectRef, Mixer, ModRouteId, ModulatorId,
    NewTrack, Note, PortRef, Project, TrackId, TrackPosition,
};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: u32 = 44_100;

/// A parallel path from a chain's input to its output, at -6 dB, so the
/// chain is a graph. Returns the new cable.
fn patch(project: &mut Project, owner: Option<TrackId>) -> CableKey {
    let graph = project.ensure_routing(owner).unwrap();
    let cable = Cable {
        from: PortRef {
            node: graph.input().unwrap(),
            port: 0,
        },
        to: PortRef {
            node: graph.output().unwrap(),
            port: 0,
        },
        gain_db: -6.0,
        muted: false,
    };
    let key = cable.key();
    graph.cables.retain(|c| c.key() != key);
    graph.cables.push(cable);
    project.normalize_routing(owner).unwrap();
    assert!(project.routing(owner).is_some());
    key
}

/// MIDI, audio and group tracks, automation, a solo and effect graphs:
/// every path the render loop has.
fn busy_project() -> Project {
    let mut project = Project::with_default_tracks();
    let ids = project.track_ids();
    let group = project.group_tracks(&[ids[1], ids[2]]).unwrap();
    let bar = project.tempo.bar_len();
    for (i, track) in [ids[0], ids[1]].into_iter().enumerate() {
        let clip = project.add_midi_clip(track, Tick::ZERO, bar * 4).unwrap();
        let notes: Vec<_> = (0..64)
            .map(|n| Note {
                id: project.new_note_id(),
                start: Tick::new(n * 240),
                len: Tick::new(200 + i as i64 * 400),
                pitch: Pitch::new(48 + (n % 24) as u8).unwrap(),
                velocity: Velocity::MAX,
                release_velocity: Velocity::CENTER,
                muted: false,
            })
            .collect();
        project.set_notes(clip, notes).unwrap();
    }
    let source = project.add_source("/tmp/noise.wav".into(), RATE, RATE as u64, 2);
    project
        .add_audio_clip(ids[2], Tick::QUARTER, source)
        .unwrap();
    project.add_audio_clip(ids[3], Tick::ZERO, source).unwrap();
    let ramp = vec![
        AutomationPoint {
            tick: Tick::ZERO,
            value: 0.0,
            curve: 0.0,
        },
        AutomationPoint {
            tick: bar,
            value: 1.0,
            curve: 0.0,
        },
    ];
    project
        .set_automation(group, AutomationTarget::Pan, ramp.clone())
        .unwrap();
    project
        .set_automation(ids[0], AutomationTarget::Volume, ramp)
        .unwrap();
    project
        .set_mixer(
            ids[1],
            Mixer {
                solo: true,
                ..Mixer::default()
            },
        )
        .unwrap();
    // Every built-in effect, on tracks, a group and the master, one of them
    // automated; those that listen to a sidechain listen to the first track
    // (which renders before some of them and after others).
    let effect = |id: &str| EffectRef::builtin(id);
    let eq = project
        .add_device(Some(ids[0]), effect("canopy.eq"), None)
        .unwrap();
    let owners = [Some(ids[0]), Some(group), Some(ids[3]), None];
    for (i, descriptor) in effects::builtins().iter().enumerate() {
        let owner = owners[i % owners.len()];
        let device = project
            .add_device(owner, effect(descriptor.id), None)
            .unwrap();
        if descriptor.sidechain {
            project.set_device_sidechain(device, Some(ids[0])).unwrap();
        }
    }
    project
        .add_device(None, effect("canopy.compressor"), Some(0))
        .unwrap();
    let sweep = vec![
        AutomationPoint {
            tick: Tick::ZERO,
            value: 0.0,
            curve: 0.0,
        },
        AutomationPoint {
            tick: bar,
            value: 1.0,
            curve: 0.0,
        },
    ];
    let target = AutomationTarget::DeviceParam {
        device: eq,
        param: "b3_freq".into(),
    };
    project.set_automation(ids[0], target, sweep).unwrap();
    patch(&mut project, Some(group));
    patch(&mut project, None);
    project
}

#[test]
fn processing_never_allocates() {
    let (mut handle, mut engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    let mut project = busy_project();
    let source = project.sources[0].id;
    let noise: Vec<f32> = (0..RATE)
        .map(|i| ((i * 7919) % 200) as f32 / 100.0 - 1.0)
        .collect();
    let audio = Arc::new(AudioBuffer::new(RATE, noise.clone(), noise));
    let lookup = |id| (id == source).then(|| audio.clone());
    let mut out = vec![0.0; 4096 * 2];
    let track = project.tracks[0].id;

    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..50 {
            engine.process_interleaved(&mut out, 2);
        }
    });

    // Swapping snapshots, both fresh processors and reused ones, plus every
    // live command; a graph made and one changed.
    project.rename_track(track, "edited").unwrap();
    let cable = patch(&mut project, None);
    patch(&mut project, Some(track));
    let new_track = project
        .add_track(NewTrack::Midi, TrackPosition::END)
        .unwrap();
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    for command in [
        Command::SetTrackVolume { track, db: -6.0 },
        Command::SetTrackPan { track, pan: 0.5 },
        Command::SetTrackMute { track, mute: true },
        Command::SetMasterVolume(-3.0),
        Command::SetInstrumentParam {
            track,
            index: 0,
            value: 1.0,
        },
        Command::PreviewNote {
            track: new_track,
            event: NoteEventKind::On {
                pitch: Pitch::A4,
                velocity: Velocity::MAX,
            },
        },
        Command::Seek(10_000),
        Command::SetLiveTarget(Some(new_track)),
        Command::SetMetronome(true),
        Command::SetDeviceParam {
            device: project.master_devices[0].id,
            index: 0,
            value: -40.0,
        },
        Command::SetDeviceParam {
            device: project.tracks[0].devices[0].id,
            index: 0,
            value: 1.0,
        },
        Command::SetModAmount {
            owner: Some(track),
            route: ModRouteId(1),
            amount: 0.5,
        },
        Command::SetModulatorParam {
            modulator: ModulatorId(1),
            index: 0,
            value: 0.5,
        },
        Command::SetCableGain {
            owner: None,
            cable,
            gain_db: -3.0,
        },
    ] {
        handle.send(command).unwrap();
    }
    // A keyboard playing, with the pedal and the pitch wheel.
    let live = handle.live_sender();
    let key = |on| {
        let (pitch, velocity) = (Pitch::new(64).unwrap(), Velocity::MAX);
        engine::LiveEvent::Note(if on {
            NoteEventKind::On { pitch, velocity }
        } else {
            NoteEventKind::Off { pitch, velocity }
        })
    };
    for event in [
        key(true),
        engine::LiveEvent::Sustain(true),
        key(false),
        engine::LiveEvent::Note(NoteEventKind::PitchBend(4000)),
        engine::LiveEvent::Sustain(false),
    ] {
        live.send(event);
    }
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..50 {
            engine.process_interleaved(&mut out, 2);
        }
    });
    handle.collect_garbage();

    project.delete_tracks(&[track]).unwrap();
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(false)).unwrap();
    handle.send(Command::SetLiveTarget(None)).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..10 {
            engine.process_interleaved(&mut out, 2);
        }
    });
    handle.collect_garbage();
    while handle.pop_played().is_some() {}
}
