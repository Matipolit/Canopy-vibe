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
    AutomationPoint, AutomationTarget, Cable, CableKey, EffectRef, GraphNode, Join, Mixer,
    ModRouteId, ModulatorId, NewTrack, NodeKind, Note, PortRef, Project, Slope, Split, TrackId,
    TrackPosition,
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

/// Beside a chain's devices: a band split, one band through a mid/side
/// split and join, and a left/right split feeding the first listening
/// device's sidechain, so every kind of step plays.
fn split_graph(project: &mut Project, owner: Option<TrackId>) {
    let listening = project
        .devices(owner)
        .unwrap()
        .iter()
        .find(|d| d.effect.descriptor().is_some_and(|e| e.sidechain))
        .map(|d| d.id);
    let [bands, ms, join, lr] = [(); 4].map(|_| project.new_node_id());
    let graph = project.ensure_routing(owner).unwrap();
    let (input, output) = (graph.input().unwrap(), graph.output().unwrap());
    let listening = listening.and_then(|d| graph.device_node(d));
    for (id, kind) in [
        (
            bands,
            NodeKind::Split(Split::Bands {
                crossovers: vec![200.0, 2000.0],
                slope: Slope::Db48,
            }),
        ),
        (ms, NodeKind::Split(Split::MidSide)),
        (join, NodeKind::Join(Join::MidSide)),
        (lr, NodeKind::Split(Split::LeftRight)),
    ] {
        graph.nodes.push(GraphNode {
            id,
            kind,
            pos: [0.0, 0.0],
        });
    }
    let port = |node, port| PortRef { node, port };
    let mut cables = vec![
        (port(input, 0), port(bands, 0)),
        (port(bands, 0), port(output, 0)),
        (port(bands, 1), port(ms, 0)),
        (port(ms, 0), port(join, 0)),
        (port(ms, 1), port(join, 1)),
        (port(join, 0), port(output, 0)),
        (port(input, 0), port(lr, 0)),
    ];
    cables.push(match listening {
        Some(device) => (port(lr, 1), port(device, 1)),
        None => (port(lr, 1), port(output, 0)),
    });
    for (from, to) in cables {
        graph.cables.push(Cable {
            from,
            to,
            gain_db: -6.0,
            muted: false,
        });
    }
    project.normalize_routing(owner).unwrap();
    assert!(project.routing(owner).is_some());
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
                // Every other note bent, so bends go through the audio
                // thread too.
                bend: if n % 2 == 0 {
                    vec![
                        project::BendPoint {
                            at: Tick::ZERO,
                            semitones: 0.0,
                            curve: 0.0,
                        },
                        project::BendPoint {
                            at: Tick::new(180),
                            semitones: 7.0,
                            curve: 0.5,
                        },
                    ]
                } else {
                    Vec::new()
                },
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
    split_graph(&mut project, Some(ids[3]));
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
    // A split with another crossover (its filters start afresh), and a
    // second set of splits.
    let is_band_split = |kind: &NodeKind| matches!(kind, NodeKind::Split(Split::Bands { .. }));
    let split_track = project
        .track_ids()
        .into_iter()
        .find(|id| {
            project
                .routing(Some(*id))
                .is_some_and(|r| r.nodes.iter().any(|n| is_band_split(&n.kind)))
        })
        .unwrap();
    for node in &mut project.ensure_routing(Some(split_track)).unwrap().nodes {
        if let NodeKind::Split(Split::Bands { crossovers, .. }) = &mut node.kind {
            crossovers.push(8000.0);
        }
    }
    split_graph(&mut project, Some(split_track));
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

/// Sources from a function, and every MIDI track's cache from a map.
struct Cached<'a> {
    source: &'a dyn Fn(project::SourceId) -> Option<Arc<AudioBuffer>>,
    caches: &'a std::collections::HashMap<TrackId, engine::TrackCache>,
}

impl engine::AudioLookup for Cached<'_> {
    fn source(&self, id: project::SourceId) -> Option<Arc<AudioBuffer>> {
        (self.source)(id)
    }

    fn track_cache(&self, track: &project::Track) -> Option<engine::TrackCache> {
        self.caches.get(&track.id).cloned()
    }
}

#[test]
fn playing_from_caches_and_leaving_them_never_allocates() {
    let (mut handle, mut engine) = engine::create(RATE as f32);
    let mut compiler = Compiler::new(RATE);
    let project = busy_project();
    let source = project.sources[0].id;
    let noise: Vec<f32> = (0..RATE)
        .map(|i| ((i * 7919) % 200) as f32 / 100.0 - 1.0)
        .collect();
    let audio = Arc::new(AudioBuffer::new(RATE, noise.clone(), noise));
    let find = |id| (id == source).then(|| audio.clone());
    let mut caches = std::collections::HashMap::new();
    for track in project.track_ids() {
        caches.insert(track, engine::cache::active(audio.clone()));
    }
    let mut out = vec![0.0; 4096 * 2];
    let lookup = Cached {
        source: &find,
        caches: &caches,
    };
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::SetPlaying(true)).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..20 {
            engine.process_interleaved(&mut out, 2);
        }
    });
    // Every cache going stale at once, mid-song: the tracks fade back to
    // their own chains, which start again from silence.
    for cache in caches.values_mut() {
        cache.active = false;
    }
    let lookup = Cached {
        source: &find,
        caches: &caches,
    };
    handle
        .send(Command::Arrangement(compiler.compile(&project, lookup)))
        .unwrap();
    handle.send(Command::Seek(20_000)).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..20 {
            engine.process_interleaved(&mut out, 2);
        }
    });
    handle.send(Command::SetPlaying(false)).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..10 {
            engine.process_interleaved(&mut out, 2);
        }
    });
    handle.collect_garbage();
}
