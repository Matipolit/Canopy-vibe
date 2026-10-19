//! Real FL Studio files, if there are any: set `CANOPY_FLP_SAMPLES` to a
//! folder of them (searched recursively). Without it these tests pass
//! without checking anything.
//!
//! Every file must read and import without panicking. Files from public
//! test suites (PyFLP, node-flp, dawtool, flpdiff) are also checked against
//! what those projects document about them.

use std::path::{Path, PathBuf};

use flp::{AutomationTarget, ChannelKind, Files, ItemTarget, Options};

fn samples() -> Vec<PathBuf> {
    let Some(dir) = std::env::var_os("CANOPY_FLP_SAMPLES") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut stack = vec![PathBuf::from(dir)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| {
                ["flp", "fst", "fsc"]
                    .iter()
                    .any(|x| e.eq_ignore_ascii_case(x))
            }) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The sample called `name`, if it's in the folder.
fn sample(name: &str) -> Option<flp::Project> {
    let path = samples()
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n == name))?;
    Some(flp::parse(&std::fs::read(path).unwrap()).expect(name))
}

fn import(path: &Path, fl: &flp::Project) -> flp::Import {
    let files = Files::find(fl, Some(path), &|_| None);
    flp::import(
        fl,
        &flp::Plan::new(fl).choices(),
        &files,
        &Options::default(),
    )
}

/// Every plugin name in a project: generators (VSTs by their own name)
/// and effects.
fn plugin_names(fl: &flp::Project) -> Vec<String> {
    let mut names: Vec<String> = fl
        .channels
        .iter()
        .filter_map(|c| c.plugin.as_ref())
        .map(|p| p.plugin_name().to_owned())
        .collect();
    for insert in &fl.inserts {
        names.extend(
            insert
                .slots
                .iter()
                .map(|s| s.plugin.plugin_name().to_owned()),
        );
    }
    names
}

#[test]
fn every_file_reads_and_imports() {
    for path in samples() {
        let bytes = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match flp::parse(&bytes) {
            Ok(fl) => {
                let result = import(&path, &fl);
                // Tracks for everything that isn't an automation clip.
                if fl.format == 0 && !fl.truncated {
                    assert!(result.summary.tempo > 0.0, "{name}");
                    let plain = fl
                        .channels
                        .iter()
                        .filter(|c| c.kind != ChannelKind::Automation)
                        .count();
                    assert!(result.summary.tracks > 0 || plain == 0, "{name}: no tracks");
                }
            }
            // Only PyFLP's deliberately broken files may fail.
            Err(err) => assert!(name.starts_with("invalid-"), "{name}: {err}"),
        }
    }
}

#[test]
fn pyflp_project() {
    let Some(fl) = sample("FL 20.8.4.flp") else {
        return;
    };
    assert_eq!(fl.version.as_deref(), Some("20.8.4.2576"));
    assert_eq!((fl.ppq, fl.channel_count, fl.channels.len()), (96, 19, 19));
    assert_eq!(fl.tempo, Some(69.42));
    assert_eq!(fl.title.as_deref(), Some("PyFLP Test FLP"));
    let mut names: Vec<String> = fl.patterns.iter().filter_map(|p| p.name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        ["Colored", "Default", "MIDI", "Selected", "Timemarkers"]
    );
    let channel = |name: &str| {
        fl.channels
            .iter()
            .find(|c| c.name.as_deref() == Some(name))
            .unwrap()
    };
    assert_eq!(
        channel("VST2")
            .plugin
            .as_ref()
            .unwrap()
            .vst
            .as_ref()
            .unwrap()
            .name,
        "Sylenth1"
    );
    assert_eq!(channel("Zero Volume").volume, Some(0));
    assert_eq!(channel("Full Volume").volume, Some(12_800));
    assert_eq!(channel("100% L").pan, Some(0));
    assert_eq!(channel("100% R").pan, Some(12_800));
    assert!(!channel("Disabled").enabled);
    assert_eq!(channel("Instrument track").insert, Some(2));
    assert_eq!(channel("Automation Clip").kind, ChannelKind::Automation);
    assert_eq!(channel("Layer").kind, ChannelKind::Layer);
    assert!(
        channel("22in Kick")
            .sample_path
            .as_deref()
            .unwrap()
            .ends_with("22in Kick.wav")
    );
    // PyFLP's automation-points preset has the same clip.
    let points: Vec<f64> = channel("Automation Clip")
        .automation
        .as_ref()
        .unwrap()
        .points
        .iter()
        .map(|p| p.position.round())
        .collect();
    assert_eq!(points[..4], [0.0, 8.0, 8.0, 16.0]);
    let tracks = fl
        .arrangements
        .iter()
        .find(|a| a.name.as_deref() == Some("Just tracks"))
        .unwrap();
    assert_eq!(tracks.items.len(), 22);
    let markers = fl
        .arrangements
        .iter()
        .find(|a| a.name.as_deref() == Some("Just timemarkers"))
        .unwrap();
    assert_eq!(markers.markers.len(), 11);
    let plugin_test = fl
        .inserts
        .iter()
        .find(|i| i.name.as_deref() == Some("Plugin Test"))
        .unwrap();
    assert_eq!(plugin_test.slots.len(), 7);
    assert_eq!(plugin_test.slots[6].plugin.plugin_name(), "OTT");
    assert_eq!(fl.insert(4).unwrap().volume, Some(0), "Zero Volume insert");
    assert_eq!(fl.insert(6).unwrap().pan, Some(-6400), "100% L insert");
    assert!(
        !fl.inserts
            .iter()
            .find(|i| i.name.as_deref() == Some("Disabled"))
            .unwrap()
            .enabled()
    );
}

#[test]
fn pyflp_presets() {
    let keys = |fl: flp::Project| -> Vec<(u32, u16, u8, u8)> {
        fl.patterns[0]
            .notes
            .iter()
            .map(|n| (n.position, n.key, n.velocity, n.fine_pitch))
            .collect()
    };
    if let Some(fl) = sample("c-major-scale.fsc") {
        let notes = keys(fl);
        let expected: Vec<u16> = vec![60, 62, 64, 65, 67, 69, 71, 72];
        assert_eq!(notes.iter().map(|n| n.1).collect::<Vec<_>>(), expected);
        assert_eq!(
            notes.iter().map(|n| n.0).collect::<Vec<_>>(),
            (0..8).map(|x| x * 384).collect::<Vec<u32>>()
        );
    }
    if let Some(fl) = sample("velocity-min-max.fsc") {
        assert_eq!(keys(fl).iter().map(|n| n.2).collect::<Vec<_>>(), [0, 128]);
    }
    if let Some(fl) = sample("fine-pitch-min-max.fsc") {
        assert_eq!(keys(fl).iter().map(|n| n.3).collect::<Vec<_>>(), [0, 240]);
    }
    if let Some(fl) = sample("automation-points.fst") {
        let points: Vec<f64> = fl.channels[0]
            .automation
            .as_ref()
            .unwrap()
            .points
            .iter()
            .map(|p| p.position.round())
            .collect();
        assert_eq!(points, [0.0, 8.0, 8.0, 16.0, 24.0, 32.0]);
    }
    if let Some(fl) = sample("routed.fst") {
        assert_eq!(fl.channels[0].insert, Some(125));
    }
    if let Some(fl) = sample("multi-channel.flp") {
        let channels: std::collections::BTreeSet<u16> = fl
            .patterns
            .iter()
            .flat_map(|p| p.notes.iter().map(|n| n.channel))
            .collect();
        assert_eq!(channels.into_iter().collect::<Vec<_>>(), [0, 1]);
    }
}

#[test]
fn pyflp_broken_files() {
    for name in [
        "invalid-data-magic.flp",
        "invalid-format.flp",
        "invalid-header-magic.flp",
        "invalid-header-size.flp",
        "invalid-ppq.flp",
    ] {
        if let Some(path) = samples().into_iter().find(|p| p.ends_with(name)) {
            assert!(flp::parse(&std::fs::read(path).unwrap()).is_err(), "{name}");
        }
    }
    // A data chunk claiming one byte, with none there: an empty import.
    if let Some(fl) = sample("invalid-event-size.flp") {
        assert!(fl.truncated);
        assert!(fl.channels.is_empty());
    }
}

/// node-flp's test suite: FL 9 and 11 files, with their tempos and plugins.
#[test]
fn node_flp_projects() {
    let cases: &[(&str, f64, &[&str])] = &[
        ("1-blank.flp", 140.0, &[]),
        ("2-named.flp", 140.0, &[]),
        ("3-3.3-time-sig.flp", 140.0, &[]),
        ("4-130-bpm.flp", 130.0, &[]),
        (
            "4front+mjcompressor.flp",
            140.0,
            &["4Front Piano", "MjMultibandCompressor"],
        ),
        ("4frontpiano.flp", 140.0, &["4Front Piano"]),
        ("5-replace-sampler-with-3xosc.flp", 130.0, &["3x Osc"]),
        ("6-turn-osc3-volume-down.flp", 130.0, &["3x Osc"]),
        ("ambience.flp", 140.0, &["Ambience"]),
        ("audio-clip.flp", 140.0, &[]),
        (
            "effects.flp",
            140.0,
            &[
                "Ambience",
                "Edison",
                "Gross Beat",
                "Hardcore",
                "Maximus",
                "MjMultibandCompressor",
                "Soundgoodizer",
                "Vocodex",
            ],
        ),
        ("native-plugins.flp", 140.0, &[]),
        (
            "TheCastle_19.flp",
            135.0,
            &["Synth1 VST", "DirectWave", "Sytrus"],
        ),
        (
            "listen-to-my-synthesizer.flp",
            140.0,
            &["Nexus", "Synth1 VST"],
        ),
        (
            "mdl.flp",
            140.0,
            &[
                "Decimort",
                "Altiverb 6",
                "FabFilter Timeless 2",
                "FabFilter Pro-L",
            ],
        ),
        (
            "nucleon-orbit.flp",
            132.0,
            &["Harmless", "Sytrus", "Slicex", "Maximus"],
        ),
    ];
    for (name, tempo, plugins) in cases {
        let Some(fl) = sample(name) else {
            continue;
        };
        assert_eq!(fl.tempo, Some(*tempo), "{name}");
        let found = plugin_names(&fl);
        for plugin in *plugins {
            assert!(
                found.iter().any(|f| f == plugin),
                "{name}: no {plugin} in {found:?}"
            );
        }
    }
    if let Some(fl) = sample("3-3.3-time-sig.flp") {
        assert_eq!(fl.time_signature.map(|t| t.0), Some(3));
    }
    // FL 9 places patterns as blocks; the song has notes to import.
    if let Some(path) = samples()
        .into_iter()
        .find(|p| p.ends_with("TheCastle_19.flp"))
    {
        let fl = flp::parse(&std::fs::read(&path).unwrap()).unwrap();
        let result = import(&path, &fl);
        assert!(result.summary.midi_clips > 100, "{:?}", result.summary);
        assert!(result.summary.notes > 1000, "{:?}", result.summary);
    }
    if let Some(fl) = sample("audio-clip.flp") {
        let items = &fl.main_arrangement().unwrap().items;
        assert!(
            items
                .iter()
                .any(|i| i.target == Some(ItemTarget::Channel(4)))
        );
    }
}

/// dawtool's tests: automation clips, playlist items and markers in FL 20.
#[test]
fn dawtool_projects() {
    if let Some(fl) = sample("auto-basic.flp") {
        let names: Vec<&str> = fl
            .channels
            .iter()
            .filter_map(|c| c.name.as_deref())
            .collect();
        assert_eq!(
            names,
            [
                "Kick", "Clap", "Hat", "Snare", "coin 4", "Tempo 5", "Tempo 6"
            ]
        );
        let items: Vec<(u32, Option<ItemTarget>, u32, u16)> = fl
            .main_arrangement()
            .unwrap()
            .items
            .iter()
            .map(|i| (i.position, i.target, i.length, i.track))
            .collect();
        assert_eq!(
            items,
            [
                (0, Some(ItemTarget::Channel(5)), 456, 0),
                (0, Some(ItemTarget::Channel(4)), 192, 2),
                (752, Some(ItemTarget::Channel(6)), 456, 1),
                (1376, Some(ItemTarget::Channel(5)), 456, 0),
            ]
        );
        let points: Vec<(f64, f64)> = fl.channels[6]
            .automation
            .as_ref()
            .unwrap()
            .points
            .iter()
            .map(|p| (p.position, (p.value * 1e4).round() / 1e4))
            .collect();
        assert_eq!(points, [(0.0, 0.5833), (4.75, 0.3077)]);
        for link in &fl.automation_links {
            assert_eq!(link.target, AutomationTarget::Global { param: 5 }, "tempo");
        }
        let result = flp::import(
            &fl,
            &flp::Plan::new(&fl).choices(),
            &Files::default(),
            &Options::default(),
        );
        assert!(
            result
                .report
                .iter()
                .any(|l| l.starts_with("Tempo automation"))
        );
    }
    if let Some(fl) = sample("fl-markers.flp") {
        let markers: Vec<(u32, &str, u8)> = fl
            .main_arrangement()
            .unwrap()
            .markers
            .iter()
            .map(|m| (m.position, m.name.as_deref().unwrap_or(""), m.action))
            .collect();
        assert_eq!(
            markers,
            [
                (1220, "Auto", 0),
                (1356, "start", 5),
                (1932, "time sig", 8),
                (2624, "loop", 4),
                (3168, "marker loop", 1),
                (3988, "marker skip", 2),
                (4896, "marker pause", 3),
                (5640, "punch in", 9),
                (6516, "punch out", 10),
            ]
        );
    }
    // Clips named after what they drive, as FL names them.
    if let Some(fl) = sample("complex.flp") {
        let target = |name: &str| {
            let clip = fl
                .channels
                .iter()
                .find(|c| c.name.as_deref() == Some(name))
                .unwrap();
            fl.automation_links
                .iter()
                .find(|l| l.channel == clip.iid)
                .unwrap()
                .target
        };
        assert!(matches!(
            target("coin 4 - Channel volume"),
            AutomationTarget::Channel {
                param: 0,
                plugin: false,
                ..
            }
        ));
        assert!(matches!(
            target("coin 4 - Channel panning"),
            AutomationTarget::Channel {
                param: 1,
                plugin: false,
                ..
            }
        ));
        assert!(matches!(
            target("coin 4 - Volume"),
            AutomationTarget::Insert {
                insert: 1,
                param: 0x1FC0,
                ..
            }
        ));
        assert!(matches!(
            target("Master - Panning"),
            AutomationTarget::Insert {
                insert: 0,
                param: 0x1FC1,
                ..
            }
        ));
        assert_eq!(
            target("Master volume"),
            AutomationTarget::Global { param: 0 }
        );
        assert_eq!(target("Tempo"), AutomationTarget::Global { param: 5 });
    }
}

/// flpdiff's FL 25 files.
#[test]
fn flpdiff_fl25_projects() {
    if let Some(fl) = sample("base_one_serum.flp") {
        let vst = fl.channels[1]
            .plugin
            .as_ref()
            .unwrap()
            .vst
            .as_ref()
            .unwrap();
        assert_eq!(
            (vst.name.as_str(), vst.vendor.as_str()),
            ("Serum", "Xfer Records")
        );
        assert_eq!(flp::Plan::new(&fl).items[1].name, "Serum");
    }
    if let Some(fl) = sample("base_one_pattern.flp") {
        let notes = &fl.patterns[0].notes;
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0].channel, notes[0].length, notes[0].key),
            (1, 48, 63)
        );
        assert_eq!(fl.tempo, Some(120.0));
    }
    if let Some(fl) = sample("base_one_insert.flp") {
        let drums = fl
            .inserts
            .iter()
            .find(|i| i.name.as_deref() == Some("Drums"))
            .unwrap();
        assert_eq!(
            drums.slots[0].plugin.internal_name,
            "Fruity Parametric EQ 2"
        );
    }
}
