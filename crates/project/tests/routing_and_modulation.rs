//! Effect graphs, modulation and plugins in the project model: files from
//! before them load and save exactly as they did, the new features
//! round-trip (as format 2), damaged ones are repaired, and what refers to
//! an effect follows it through edits.

use std::collections::BTreeMap;

use effects::{Category, Context, Effect, EffectDescriptor, Pace};
use instruments::{InstrumentRef, ParamDescriptor, ParameterKind};
use midi::Tick;
use project::*;

/// A project saved by canopy before graphs, modulation and plugins:
/// effects with settings and a sidechain, automation of the mixer, an
/// instrument and an effect, a group, the master's chain and a controller
/// link.
const LINEAR: &str = include_str!("fixtures/linear.canopy");

#[test]
fn files_from_before_load_and_save_unchanged() {
    let p = Project::from_file_string(LINEAR).unwrap();
    assert_eq!(p.to_file_string().unwrap(), LINEAR, "byte for byte");
    assert_eq!(p.file_format(), 1);
    let bass = p.track(TrackId(1)).unwrap();
    let names: Vec<&str> = bass.devices.iter().map(Device::name).collect();
    assert_eq!(names, ["EQ", "Compressor"]);
    assert_eq!(bass.devices[1].sidechain, Some(TrackId(7)));
    assert_eq!(bass.devices[1].param("threshold"), Some(-20.0));
    assert!(
        bass.devices
            .iter()
            .all(|d| d.effect.format == Format::BuiltIn)
    );
    assert!(bass.routing.is_none() && bass.modulators.is_empty());
    assert_eq!(bass.automation.len(), 3);
    assert_eq!(p.controller_links.len(), 1);
    assert_eq!(p.master_devices[0].name(), "Limiter");
    assert!(p.master_routing.is_none());
}

/// Every `.canopy` file in `$CANOPY_PROJECTS` (projects saved by older
/// builds) loads, saves as format 1 with nothing new in it, and loads back
/// the same. Skipped unless the variable is set.
#[test]
fn saved_projects_still_load() {
    let Some(dir) = std::env::var_os("CANOPY_PROJECTS") else {
        return;
    };
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != FILE_EXTENSION) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let p = Project::from_file_string(&text).unwrap();
        let again = p.to_file_string().unwrap();
        assert_eq!(p.file_format(), 1, "{}", path.display());
        for new in ["routing", "modulat", "plugin"] {
            assert!(!again.contains(new), "{}: {new}", path.display());
        }
        assert_eq!(Project::from_file_string(&again).unwrap(), p);
    }
}

/// Passes sound through; stands in for a plugin.
struct Thru;

impl Effect for Thru {
    fn prepare(&mut self, _: f32, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &Context) {}
    fn params(&self) -> &'static [ParamDescriptor] {
        &GAIN
    }
    fn receive(&mut self, _: usize, _: f32, _: Pace) {}
}

static GAIN: [ParamDescriptor; 1] = [ParamDescriptor {
    id: "7",
    name: "Gain",
    kind: ParameterKind::Float {
        min: 0.0,
        max: 1.0,
        default: 0.5,
        unit: "",
        logarithmic: false,
    },
}];

/// A CLAP effect, as the plugin host would register it.
fn plugin_effect(id: &'static str, name: &'static str) -> EffectRef {
    effects::register_external(
        Format::Clap,
        id,
        EffectDescriptor {
            id,
            name,
            description: "",
            version: "2.1",
            category: Category::Distortion,
            params: &GAIN,
            settings: &[],
            meters: &[],
            tap: false,
            sidechain: false,
            latency: effects::no_latency,
            create: || Box::new(Thru),
        },
    );
    EffectRef {
        format: Format::Clap,
        id: id.to_owned(),
        version: "2.1".to_owned(),
    }
}

#[test]
fn plugins_are_found_through_the_registry() {
    let reference = plugin_effect("test.registry", "First");
    let first = reference.descriptor().unwrap();
    assert_eq!(first.name, "First");
    assert!(
        EffectRef {
            format: Format::Vst3,
            ..reference.clone()
        }
        .descriptor()
        .is_none()
    );
    plugin_effect("test.registry", "Second");
    assert_eq!(reference.descriptor().unwrap().name, "Second", "replaced");
    assert_eq!(first.name, "First", "the old one lives on");
    assert!(reference.create().is_some());
}

#[test]
fn effect_formats_are_written_only_for_plugins() {
    let builtin = ron::to_string(&EffectRef::builtin("canopy.eq")).unwrap();
    assert!(!builtin.contains("format"), "{builtin}");
    let old: EffectRef = ron::from_str(r#"(id: "canopy.eq", version: "1.0")"#).unwrap();
    assert_eq!(old.format, Format::BuiltIn);
    let clap = EffectRef {
        format: Format::Clap,
        id: "com.example.gain".into(),
        version: "1".into(),
    };
    let text = ron::to_string(&clap).unwrap();
    assert!(text.contains("Clap"), "{text}");
    assert_eq!(ron::from_str::<EffectRef>(&text).unwrap(), clap);
}

fn port(node: NodeId, port: u8) -> PortRef {
    PortRef { node, port }
}

fn cable(from: PortRef, to: PortRef, gain_db: f32) -> Cable {
    Cable {
        from,
        to,
        gain_db,
        muted: false,
    }
}

fn modulator(p: &mut Project, kind: &str) -> Modulator {
    Modulator {
        id: p.new_modulator_id(),
        kind: ModulatorRef {
            id: kind.into(),
            version: "1".into(),
        },
        params: BTreeMap::new(),
        name: None,
        listen: None,
        folded: false,
    }
}

fn route(p: &mut Project, source: ModSource, target: AutomationTarget) -> ModRoute {
    ModRoute {
        id: p.new_mod_route_id(),
        source,
        target,
        amount: 0.25,
        polarity: Polarity::Source,
        curve: 0.0,
        scale_by: None,
        bypassed: false,
    }
}

fn threshold(device: DeviceId) -> AutomationTarget {
    AutomationTarget::DeviceParam {
        device,
        param: "threshold".into(),
    }
}

/// Every new thing at once: a graph with a split, a join, a parallel
/// branch at a gain and a sidechain cable; modulators with every field;
/// routes from a modulator and the instrument, one scaled by a macro, to an
/// effect, the volume and a modulator; automation and a controller link on
/// a modulator; a plugin effect and a plugin instrument with state; and a
/// graph on the master.
struct Rich {
    p: Project,
    bass: TrackId,
    other: TrackId,
    devices: [DeviceId; 3],
    lfo: ModulatorId,
}

fn rich() -> Rich {
    let mut p = Project::with_default_tracks();
    let ids = p.track_ids();
    let (bass, other) = (ids[0], ids[1]);
    let devices = ["canopy.eq", "canopy.compressor", "canopy.reverb"].map(|id| {
        p.add_device(Some(bass), EffectRef::builtin(id), None)
            .unwrap()
    });
    let [eq, comp, _] = devices;
    let (split, join) = (p.new_node_id(), p.new_node_id());
    let graph = p.ensure_routing(Some(bass)).unwrap();
    let input = graph.input().unwrap();
    let output = graph.output().unwrap();
    let (eq_node, comp_node) = (
        graph.device_node(eq).unwrap(),
        graph.device_node(comp).unwrap(),
    );
    graph.nodes.push(GraphNode {
        id: split,
        kind: NodeKind::Split(Split::MidSide),
        pos: [100.0, 200.0],
    });
    graph.nodes.push(GraphNode {
        id: join,
        kind: NodeKind::Join(Join::MidSide),
        pos: [200.0, 200.0],
    });
    graph.cables.extend([
        cable(port(input, 0), port(split, 0), 0.0),
        cable(port(split, 0), port(join, 0), 0.0),
        cable(port(split, 1), port(join, 1), -3.0),
        cable(port(join, 0), port(eq_node, 0), 0.0),
        cable(port(split, 1), port(comp_node, 1), 0.0),
        Cable {
            muted: true,
            ..cable(port(eq_node, 0), port(output, 0), -6.0)
        },
    ]);
    p.normalize_routing(Some(bass)).unwrap();

    let bass_track = p.track(bass).unwrap();
    assert!(bass_track.routing.is_some(), "not linear");
    let comp_node = bass_track
        .routing
        .as_ref()
        .unwrap()
        .device_node(comp)
        .unwrap();
    let mut lfo = modulator(&mut p, "canopy.lfo");
    lfo.params.insert("rate".into(), 2.5);
    lfo.name = Some("Wobble".into());
    lfo.folded = true;
    let mut follower = modulator(&mut p, "canopy.follower");
    follower.listen = Some(Listen::Track(other));
    let mut node_follower = modulator(&mut p, "canopy.follower");
    node_follower.listen = Some(Listen::Node(port(comp_node, 0)));
    let mac = modulator(&mut p, "canopy.macro");
    let lfo_id = lfo.id;
    let mac_source = ModSource::Modulator {
        id: mac.id,
        output: 0,
    };
    let lfo_source = ModSource::Modulator {
        id: lfo.id,
        output: 1,
    };
    let mut to_comp = route(&mut p, lfo_source.clone(), threshold(comp));
    to_comp.polarity = Polarity::Bipolar;
    to_comp.curve = 0.3;
    to_comp.amount = -0.5;
    to_comp.scale_by = Some(mac_source.clone());
    let mut to_volume = route(
        &mut p,
        ModSource::Instrument("lfo1".into()),
        AutomationTarget::Volume,
    );
    to_volume.bypassed = true;
    let rate = AutomationTarget::ModulatorParam {
        modulator: lfo.id,
        param: "rate".into(),
    };
    let to_rate = route(&mut p, mac_source, rate.clone());
    let track = p.track_mut(bass).unwrap();
    track.modulators.extend([lfo, follower, node_follower, mac]);
    track.modulations.extend([to_comp, to_volume, to_rate]);
    p.set_automation(
        bass,
        rate.clone(),
        vec![AutomationPoint {
            tick: Tick::ZERO,
            value: 0.5,
            curve: 0.0,
        }],
    )
    .unwrap();
    p.link_controller(ControllerLink {
        channel: 1,
        controller: 74,
        track: Some(bass),
        target: rate,
    })
    .unwrap();

    let gain = p
        .add_device(Some(other), plugin_effect("test.rich", "Rich Gain"), None)
        .unwrap();
    p.set_device_param(gain, "7", 0.75).unwrap();
    let data = PluginData {
        name: "Rich Gain".into(),
        vendor: "Test".into(),
        path: Some("/usr/lib/clap/rich.clap".into()),
        state: Blob::from(vec![0, 1, 2, 250, 255]),
    };
    p.set_device_plugin(gain, Some(data.clone())).unwrap();
    let vst = InstrumentRef {
        format: Format::Vst3,
        id: "0123456789ABCDEF0123456789ABCDEF".into(),
        version: "1.2".into(),
    };
    p.set_instrument(other, vst).unwrap();
    p.set_instrument_param(other, "12", 0.5).unwrap();
    p.set_instrument_plugin(other, Some(data)).unwrap();

    p.add_device(None, EffectRef::builtin("canopy.limiter"), None)
        .unwrap();
    let master = p.ensure_routing(None).unwrap();
    let (input, output) = (master.input().unwrap(), master.output().unwrap());
    master
        .cables
        .push(cable(port(input, 0), port(output, 0), -12.0));
    p.normalize_routing(None).unwrap();
    assert!(p.master_routing.is_some());
    Rich {
        p,
        bass,
        other,
        devices,
        lfo: lfo_id,
    }
}

#[test]
fn every_new_feature_round_trips_as_format_2() {
    let Rich { p, .. } = rich();
    let text = p.to_file_string().unwrap();
    assert!(text.contains("format: 2"), "{text}");
    let loaded = Project::from_file_string(&text).unwrap();
    assert_eq!(loaded, p);
    assert_eq!(loaded.to_file_string().unwrap(), text);

    // Any one of them is enough for format 2, and none of them is format 1.
    let plain = Project::with_default_tracks();
    let plain_text = plain.to_file_string().unwrap();
    assert!(plain_text.contains("format: 1"));
    for new in ["routing", "modulat", "plugin", "Clap", "Vst3"] {
        assert!(!plain_text.contains(new), "{new}");
    }
    let mut one = plain.clone();
    let id = one.new_modulator_id();
    let track = one.track_ids()[0];
    one.track_mut(track).unwrap().modulators.push(Modulator {
        id,
        kind: ModulatorRef {
            id: "canopy.macro".into(),
            version: "1".into(),
        },
        params: BTreeMap::new(),
        name: None,
        listen: None,
        folded: false,
    });
    assert_eq!(one.file_format(), 2);
    let mut one = plain.clone();
    one.add_device(None, plugin_effect("test.one", "One"), None)
        .unwrap();
    assert_eq!(one.file_format(), 2, "a plugin's format alone");
}

#[test]
fn older_formats_load_and_newer_ones_are_refused() {
    let Rich { p, .. } = rich();
    let text = p.to_file_string().unwrap();
    // Canopy before format 2 refused anything above 1, as this refuses
    // anything above 2.
    let newer = text.replacen("format: 2", "format: 3", 1);
    assert_eq!(Project::from_file_string(&newer), Err(LoadError::TooNew(3)));
    // A format 2 file needing nothing new still loads (and saves as 1).
    let plain = Project::with_default_tracks().to_file_string().unwrap();
    let marked = plain.replacen("format: 1", "format: 2", 1);
    let loaded = Project::from_file_string(&marked).unwrap();
    assert_eq!(loaded.to_file_string().unwrap(), plain);
}

/// Serializes `p` as it is (unrepaired) and loads it back.
fn reload(p: &Project) -> Project {
    Project::from_file_string(&p.to_file_string().unwrap()).unwrap()
}

#[test]
fn graphs_that_cannot_play_are_dropped_on_load() {
    let Rich {
        p,
        bass,
        devices: [eq, comp, reverb],
        ..
    } = rich();
    let graph = |p: &Project| p.track(bass).unwrap().routing.clone().unwrap();
    let pool = |p: &Project| -> Vec<DeviceId> {
        p.track(bass)
            .unwrap()
            .devices
            .iter()
            .map(|d| d.id)
            .collect()
    };
    let node = |p: &Project, d| graph(p).device_node(d).unwrap();

    // A loop: reverb back into the EQ.
    let mut looped = p.clone();
    let (from, to) = (node(&p, reverb), node(&p, eq));
    let track = looped.track_mut(bass).unwrap();
    track
        .routing
        .as_mut()
        .unwrap()
        .cables
        .push(cable(port(from, 0), port(to, 0), 0.0));
    let loaded = reload(&looped);
    assert!(loaded.track(bass).unwrap().routing.is_none());
    assert_eq!(pool(&loaded), [eq, comp, reverb], "still in sound order");
    let modulations = &loaded.track(bass).unwrap().modulations;
    assert_eq!(modulations.len(), 3, "modulation doesn't need the graph");

    // Two nodes with one id.
    let mut doubled = p.clone();
    let routing = doubled.track_mut(bass).unwrap().routing.as_mut().unwrap();
    routing.nodes[1].id = routing.nodes[2].id;
    assert!(reload(&doubled).track(bass).unwrap().routing.is_none());

    // A node for an effect that isn't there.
    let mut dangling = p.clone();
    let id = dangling.new_node_id();
    let routing = dangling.track_mut(bass).unwrap().routing.as_mut().unwrap();
    routing.nodes.push(GraphNode {
        id,
        kind: NodeKind::Device(DeviceId(99_999)),
        pos: [0.0; 2],
    });
    assert!(reload(&dangling).track(bass).unwrap().routing.is_none());

    // An effect with no node.
    let mut orphan = p.clone();
    let routing = orphan.track_mut(bass).unwrap().routing.as_mut().unwrap();
    routing.nodes.retain(|n| n.kind != NodeKind::Device(comp));
    routing.cables.retain(|c| {
        let gone = |n: NodeId| routing_has(&p, bass, n, comp);
        !gone(c.from.node) && !gone(c.to.node)
    });
    let loaded = reload(&orphan);
    assert!(loaded.track(bass).unwrap().routing.is_none());
    assert_eq!(pool(&loaded).len(), 3, "no effect is lost");

    // Unplayable values are put right without dropping anything.
    let mut odd = p.clone();
    let routing = odd.track_mut(bass).unwrap().routing.as_mut().unwrap();
    routing.cables[0].gain_db = f32::NAN;
    routing.nodes[0].pos = [f32::INFINITY, 0.0];
    odd.track_mut(bass).unwrap().modulations[0].amount = f32::NAN;
    let loaded = reload(&odd);
    let routing = loaded.track(bass).unwrap().routing.clone().unwrap();
    assert_eq!(routing.cables[0].gain_db, 0.0);
    assert_eq!(routing.nodes[0].pos, [0.0, 0.0]);
    assert_eq!(loaded.track(bass).unwrap().modulations[0].amount, 0.0);
}

/// Whether node `n` is device `d`'s in `track`'s graph in `p`.
fn routing_has(p: &Project, track: TrackId, n: NodeId, d: DeviceId) -> bool {
    p.track(track)
        .and_then(|t| t.routing.as_ref())
        .and_then(|r| r.node(n))
        .is_some_and(|node| node.kind == NodeKind::Device(d))
}

#[test]
fn dangling_modulation_is_pruned_on_load() {
    let Rich {
        mut p,
        bass,
        other,
        devices: [_, comp, _],
        lfo,
    } = rich();
    let ghost = p.new_modulator_id();
    let track = p.track_mut(bass).unwrap();
    let lfo_source = ModSource::Modulator { id: lfo, output: 0 };
    let ghost_source = ModSource::Modulator {
        id: ghost,
        output: 0,
    };
    let mut add = |source: ModSource, target: AutomationTarget, scale_by: Option<ModSource>| {
        let id = ModRouteId(90_000 + track.modulations.len() as u32);
        track.modulations.push(ModRoute {
            id,
            source,
            target,
            amount: 0.5,
            polarity: Polarity::Unipolar,
            curve: 0.0,
            scale_by,
            bypassed: false,
        });
        id
    };
    let to_missing_device = add(lfo_source.clone(), threshold(DeviceId(99_999)), None);
    let from_missing = add(ghost_source.clone(), threshold(comp), None);
    let scaled_by_missing = add(lfo_source, threshold(comp), Some(ghost_source));
    let to_missing_modulator = add(
        ModSource::Instrument("lfo1".into()),
        AutomationTarget::ModulatorParam {
            modulator: ghost,
            param: "rate".into(),
        },
        None,
    );
    track.modulators[1].listen = Some(Listen::Track(TrackId(99_999)));
    track.modulators[2].listen = Some(Listen::Node(port(NodeId(99_999), 0)));
    track.automation.push(AutomationLane {
        target: AutomationTarget::ModulatorParam {
            modulator: ghost,
            param: "rate".into(),
        },
        points: vec![AutomationPoint {
            tick: Tick::ZERO,
            value: 0.5,
            curve: 0.0,
        }],
    });
    p.controller_links.push(ControllerLink {
        channel: 2,
        controller: 3,
        track: Some(bass),
        target: AutomationTarget::ModulatorParam {
            modulator: ghost,
            param: "rate".into(),
        },
    });
    // Routes on an audio track can't use or move an instrument.
    let audio = p.track_ids()[2];
    let on_audio = ModRoute {
        id: ModRouteId(95_000),
        source: ModSource::Instrument("lfo1".into()),
        target: AutomationTarget::Volume,
        amount: 0.5,
        polarity: Polarity::Source,
        curve: 0.0,
        scale_by: None,
        bypassed: false,
    };
    p.track_mut(audio).unwrap().modulations.push(on_audio);

    let loaded = reload(&p);
    let track = loaded.track(bass).unwrap();
    let kept: Vec<ModRouteId> = track.modulations.iter().map(|r| r.id).collect();
    for gone in [to_missing_device, from_missing, to_missing_modulator] {
        assert!(!kept.contains(&gone), "{gone}");
    }
    let scaled = track
        .modulations
        .iter()
        .find(|r| r.id == scaled_by_missing)
        .expect("only its scaling goes");
    assert_eq!(scaled.scale_by, None);
    assert_eq!(track.modulators[1].listen, None);
    assert_eq!(track.modulators[2].listen, None);
    assert_eq!(track.automation.len(), 1, "only the lane on a real one");
    assert_eq!(loaded.controller_links.len(), 1);
    assert!(loaded.track(audio).unwrap().modulations.is_empty());
    assert!(loaded.track(other).is_some());
}

#[test]
fn ids_that_collide_are_renumbered_and_cables_follow() {
    let Rich {
        mut p,
        bass,
        devices: [eq, _, _],
        ..
    } = rich();
    let routing = p.track_mut(bass).unwrap().routing.as_mut().unwrap();
    let node = routing.device_node(eq).unwrap();
    // The node takes the id of a track.
    let clash = NodeId(bass.0);
    routing.nodes.iter_mut().find(|n| n.id == node).unwrap().id = clash;
    for cable in &mut routing.cables {
        for end in [&mut cable.from.node, &mut cable.to.node] {
            if *end == node {
                *end = clash;
            }
        }
    }
    let track = p.track_mut(bass).unwrap();
    track.modulators[2].listen = Some(Listen::Node(port(clash, 0)));
    let loaded = reload(&p);
    let track = loaded.track(bass).unwrap();
    let routing = track.routing.as_ref().expect("still valid");
    let renumbered = routing.device_node(eq).unwrap();
    assert_ne!(renumbered, clash);
    assert_eq!(routing.validate(&track.devices), Ok(()));
    assert_eq!(
        track.modulators[2].listen,
        Some(Listen::Node(port(renumbered, 0))),
        "the follower follows"
    );
}

#[test]
fn removing_an_effect_splices_its_node_and_takes_its_routes() {
    let Rich {
        mut p,
        bass,
        devices: [eq, comp, reverb],
        ..
    } = rich();
    let mut history = History::default();
    let before = p.clone();
    history
        .edit(&mut p, "Remove", |p| p.remove_device(comp))
        .unwrap();
    let track = p.track(bass).unwrap();
    let routing = track.routing.as_ref().unwrap();
    assert_eq!(routing.validate(&track.devices), Ok(()));
    assert!(routing.device_node(comp).is_none());
    assert!(
        track
            .modulations
            .iter()
            .all(|r| r.target != threshold(comp))
    );
    assert_eq!(track.modulators[2].listen, None, "its node is gone");
    // What fed the compressor now feeds the reverb.
    let (eq_node, reverb_node) = (
        routing.device_node(eq).unwrap(),
        routing.device_node(reverb).unwrap(),
    );
    assert!(
        routing
            .cables
            .iter()
            .any(|c| c.from.node == eq_node && c.to.node == reverb_node)
    );
    history.undo(&mut p);
    assert_eq!(p, before);
}

#[test]
fn copies_and_moves_take_routes_where_their_source_is() {
    let Rich {
        mut p,
        bass,
        other,
        devices: [_, comp, _],
        lfo,
    } = rich();
    let count = |p: &Project, t: TrackId, d: DeviceId| {
        p.track(t)
            .unwrap()
            .modulations
            .iter()
            .filter(|r| r.target == threshold(d))
            .count()
    };
    let copied = p.copy_devices(&[comp]);
    assert_eq!(copied[0].routes.len(), 1);

    // Pasted on its own track, the LFO is there: the copy is modulated
    // too, by a route of its own.
    let pasted = p.insert_devices(Some(bass), None, &copied).unwrap()[0];
    assert_eq!(count(&p, bass, pasted), 1);
    assert_eq!(count(&p, bass, comp), 1);
    let ids: Vec<ModRouteId> = p
        .track(bass)
        .unwrap()
        .modulations
        .iter()
        .map(|r| r.id)
        .collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len());
    // It went in before the graph's Output.
    let track = p.track(bass).unwrap();
    let routing = track.routing.as_ref().unwrap();
    assert_eq!(routing.validate(&track.devices), Ok(()));
    let output = routing.output().unwrap();
    let feeds: Vec<NodeId> = routing
        .cables
        .iter()
        .filter(|c| c.to.node == output)
        .map(|c| c.from.node)
        .collect();
    assert_eq!(feeds, [routing.device_node(pasted).unwrap()]);

    // On another track there's no LFO: no route.
    let elsewhere = p.insert_devices(Some(other), None, &copied).unwrap()[0];
    assert_eq!(count(&p, other, elsewhere), 0);

    // Moved there, the same: its route stays behind and goes.
    p.move_devices(&[comp], Some(other), 0).unwrap();
    assert_eq!(count(&p, other, comp), 0);
    assert_eq!(count(&p, bass, comp), 0);
    let track = p.track(bass).unwrap();
    assert!(track.routing.as_ref().unwrap().device_node(comp).is_none());
    assert_eq!(
        track.routing.as_ref().unwrap().validate(&track.devices),
        Ok(())
    );
    // A route from the instrument moves with it to a MIDI track.
    let to_comp = ModRoute {
        id: p.new_mod_route_id(),
        source: ModSource::Instrument("lfo1".into()),
        target: threshold(comp),
        amount: 0.1,
        polarity: Polarity::Source,
        curve: 0.0,
        scale_by: Some(ModSource::Modulator { id: lfo, output: 0 }),
        bypassed: false,
    };
    p.track_mut(other).unwrap().modulations.push(to_comp);
    p.move_devices(&[comp], Some(bass), 0).unwrap();
    assert_eq!(count(&p, bass, comp), 1);
    assert_eq!(
        p.track(bass).unwrap().modulations.last().unwrap().scale_by,
        Some(ModSource::Modulator { id: lfo, output: 0 }),
        "the LFO is here"
    );
    // The master has no modulation.
    p.move_devices(&[comp], None, 0).unwrap();
    assert_eq!(count(&p, bass, comp), 0);
    assert!(p.master_routing.is_some(), "still a graph");
    let routing = p.master_routing.as_ref().unwrap();
    assert_eq!(routing.validate(&p.master_devices), Ok(()));
}

#[test]
fn a_new_instrument_drops_its_routes_and_a_deleted_track_its_listeners() {
    let Rich {
        mut p, bass, other, ..
    } = rich();
    p.set_instrument(bass, InstrumentRef::builtin("canopy.drums"))
        .unwrap();
    let track = p.track(bass).unwrap();
    assert!(
        track
            .modulations
            .iter()
            .all(|r| !matches!(r.source, ModSource::Instrument(_))
                && !matches!(r.target, AutomationTarget::InstrumentParam(_)))
    );
    assert_eq!(track.modulations.len(), 2);

    assert_eq!(p.track(other).unwrap().devices.len(), 1);
    let plugin = match &p.track(other).unwrap().kind {
        TrackKind::Midi { plugin, .. } => plugin.clone(),
        _ => None,
    };
    assert!(plugin.is_some());
    p.set_instrument(other, InstrumentRef::builtin("canopy.poly_synth"))
        .unwrap();
    assert!(matches!(
        &p.track(other).unwrap().kind,
        TrackKind::Midi { plugin: None, .. }
    ));

    p.delete_tracks(&[other]).unwrap();
    let track = p.track(bass).unwrap();
    assert_eq!(track.modulators[1].listen, None);
}

#[test]
fn missing_plugins_keep_their_name_settings_and_state() {
    let Rich { p, other, .. } = rich();
    let text = p.to_file_string().unwrap();
    // As a build without the plugin reads it.
    let text = text.replace("test.rich", "test.not-installed");
    let loaded = Project::from_file_string(&text).unwrap();
    let device = &loaded.track(other).unwrap().devices[0];
    assert!(device.effect.descriptor().is_none());
    assert_eq!(device.name(), "Rich Gain");
    assert_eq!(device.params.get("7"), Some(&0.75));
    assert_eq!(
        device.plugin.as_ref().unwrap().state.as_bytes(),
        [0, 1, 2, 250, 255]
    );
    assert_eq!(loaded.to_file_string().unwrap(), text);
}
