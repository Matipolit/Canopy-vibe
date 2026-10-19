//! File → Import FL Studio Project: the dialog, a changed replacement, and
//! the project it makes.

use std::path::{Path, PathBuf};

use eframe::egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use flp::write::{ChannelType, Item, SlotPlugin, Writer, note};
use project::{ClipRef, Track, TrackKind};

use super::{Rig, SAMPLES_PER_BAR, peak, temp_dir, write_sine_wav};
use crate::flp_import::Dialog;
use crate::session::SourceState;

fn slot(name: &str) -> SlotPlugin {
    SlotPlugin {
        index: 0,
        internal_name: name.to_owned(),
        name: None,
        state: vec![0; 8],
    }
}

/// A bass on its own insert, and a kick and a vocal clip sharing "Drums".
pub(super) fn write_song(dir: &Path) -> PathBuf {
    let vox = dir.join("vox.wav");
    write_sine_wav(&vox, 2, 330.0);
    write_sine_wav(&dir.join("kick.wav"), 1, 60.0);
    let mut w = Writer::new(96, "20.8.4.2576");
    w.tempo(140.0);
    w.pattern_notes(
        1,
        &[
            note(0, 0, 192, 48),
            note(0, 192, 192, 51),
            note(1, 0, 0, 60),
            note(1, 192, 0, 60),
        ],
    );
    w.channel(0, ChannelType::Generator, "3x Osc", "Bass")
        .route_to(1);
    // Saved on another machine; found by name next to the project.
    w.channel(1, ChannelType::Sampler, "", "Kick")
        .sample("C:\\Users\\someone\\Samples\\kick.wav")
        .route_to(2);
    w.channel(2, ChannelType::AudioClip, "", "Vox")
        .sample(vox.to_str().unwrap())
        .route_to(2);
    w.pattern_name(1, "Groove");
    w.arrangement(
        0,
        "Arrangement",
        &[
            Item::pattern(1, 0, 384),
            Item::pattern(1, 384, 384),
            Item::clip(2, 0, 384),
        ],
    );
    w.insert("", &[slot("Fruity Limiter")], &[]);
    w.insert("Bass", &[slot("Fruity Reeverb 2")], &[0]);
    w.insert("Drums", &[slot("Fruity Compressor")], &[0]);
    let path = dir.join("groove.flp");
    std::fs::write(&path, w.finish()).unwrap();
    path
}

fn find<'a>(project: &'a project::Project, name: &str) -> &'a Track {
    let mut found = None;
    project.visit(|t, _| {
        if t.name == name {
            found = Some(t);
        }
    });
    found.unwrap_or_else(|| panic!("no track {name}"))
}

#[test]
fn import_an_fl_studio_project() {
    let mut rig = Rig::new("flp_import");
    let dir = temp_dir("flp-import");
    let path = write_song(&dir);
    let sfz_dir = dir.join("sfz");

    // Cancel leaves everything as it was.
    // Its grids take a few frames to settle their widths.
    rig.app_mut().flp_import =
        Some(Dialog::open(&path, Some(sfz_dir.clone()), Default::default()).unwrap());
    rig.frames(5);
    rig.harness.get_by_label("Cancel").click();
    rig.frames(2);
    assert!(rig.app().flp_import.is_none());

    // Unsaved work, so importing will ask first.
    let first = rig.app().session.project.tracks[0].id;
    rig.app_mut()
        .session
        .edit("Rename", |p| p.rename_track(first, "Mine"));
    assert!(rig.app().session.is_dirty());

    rig.app_mut().flp_import =
        Some(Dialog::open(&path, Some(sfz_dir.clone()), Default::default()).unwrap());
    rig.frames(5);
    rig.harness.get_by_label("Import FL Studio Project");
    rig.harness.get_by_label("140.00 BPM");
    rig.harness.get_by_label("Fruity Reeverb 2");
    rig.screenshot("dialog");

    // Shortcuts wait while it's open.
    rig.key(Modifiers::NONE, Key::Space);
    assert!(!rig.app().session.is_playing());

    // 3x Osc would be a Poly Synth; make it a Wavetable. (Track headers
    // name their instruments too: the dialog's is the button.)
    rig.harness
        .get_by_role_and_label(eframe::egui::accesskit::Role::Button, "Poly Synth")
        .click();
    rig.frames(2);
    rig.screenshot("menu");
    rig.harness.get_by_label("Wavetable").click();
    rig.frames(2);
    let preview = rig.app().flp_import.as_ref().unwrap().preview();
    let TrackKind::Midi { instrument, .. } = &find(&preview.project, "Bass").kind else {
        panic!("Bass should be a MIDI track");
    };
    assert_eq!(instrument.id, "canopy.wavetable");
    rig.screenshot("changed");

    rig.harness.get_by_label("Import").click();
    rig.frames(2);
    rig.harness.get_by_label("Don't Save").click();
    rig.frames(3);
    rig.screenshot("imported");
    assert!(rig.app().flp_import.is_none());

    // A new project with no file and no undo history, counted as unsaved.
    let session = &rig.app().session;
    assert_eq!(session.path, None);
    assert!(session.is_dirty());
    assert_eq!(session.undo_label(), None);
    assert_eq!(
        session.message.as_ref().map(|m| m.text.as_str()),
        Some("Imported groove")
    );
    let p = &session.project;
    let mut names = Vec::new();
    p.visit(|t, depth| names.push(format!("{}{}", "-".repeat(depth), t.name)));
    assert_eq!(names, ["Bass", "Drums", "-Kick", "-Vox"]);
    assert!((p.tempo.tempo.to_bpm() - 140.0).abs() < 0.01);
    let masters: Vec<&str> = p.master_devices.iter().map(|d| d.name()).collect();
    assert_eq!(masters, ["Limiter"]);

    let bass = find(p, "Bass");
    assert_eq!(
        bass.devices.iter().map(|d| d.name()).collect::<Vec<_>>(),
        ["Reverb"]
    );
    let TrackKind::Midi {
        instrument, clips, ..
    } = &bass.kind
    else {
        panic!("Bass should be a MIDI track");
    };
    assert_eq!(instrument.id, "canopy.wavetable");
    let starts: Vec<i64> = clips.iter().map(|c| c.start.get()).collect();
    assert_eq!(starts, [0, 3840]);
    assert_eq!(clips[0].notes.len(), 2);
    assert_eq!(clips[0].name, "Groove");

    let drums = find(p, "Drums");
    assert!(drums.is_group());
    assert_eq!(drums.devices[0].name(), "Compressor");

    // The kick plays its sample through an SFZ written for it.
    let kick = find(p, "Kick");
    let sfz = kick.instrument_file().expect("an SFZ").to_owned();
    let text = std::fs::read_to_string(&sfz).unwrap();
    assert!(text.contains(&format!("sample={}", dir.join("kick.wav").display())));

    let vox = find(p, "Vox");
    let source = match vox.clips()[0] {
        ClipRef::Audio(clip) => clip.source,
        ClipRef::Midi(_) => panic!("Vox should hold an audio clip"),
    };
    rig.wait_for("the vocal", |app| {
        matches!(
            app.session.source_state(source),
            Some(SourceState::Ready(_))
        )
    });

    // And it plays.
    rig.app_mut().session.seek(midi::Tick::ZERO);
    rig.key(Modifiers::NONE, Key::Space);
    assert!(rig.app().session.is_playing());
    let audio = rig.render_audio(SAMPLES_PER_BAR / 4);
    assert!(peak(&audio) > 0.05, "silent: {}", peak(&audio));
    rig.key(Modifiers::NONE, Key::Space);
    let _ = std::fs::remove_dir_all(dir);
}

/// With `CANOPY_FLP_SAMPLES` set to a folder of real projects: the
/// biggest one through the dialog. Passes quietly without it.
#[test]
fn import_a_real_project() {
    let Some(dir) = std::env::var_os("CANOPY_FLP_SAMPLES") else {
        return;
    };
    let mut biggest: Option<(u64, PathBuf)> = None;
    let mut stack = vec![PathBuf::from(dir)];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let size = entry.metadata().map_or(0, |m| m.len());
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "flp")
                && biggest.as_ref().is_none_or(|(s, _)| size > *s)
            {
                biggest = Some((size, path));
            }
        }
    }
    let Some((_, path)) = biggest else {
        return;
    };
    let mut rig = Rig::new("flp_import_real");
    let sfz_dir = temp_dir("flp-real").join("sfz");
    rig.app_mut().flp_import =
        Some(Dialog::open(&path, Some(sfz_dir), Default::default()).unwrap());
    rig.frames(5);
    rig.screenshot("dialog");
    rig.harness.get_by_label("Import").click();
    rig.frames(3);
    rig.screenshot("imported");
    assert!(rig.app().flp_import.is_none());
    assert!(!rig.app().session.project.tracks.is_empty());
}

#[test]
fn files_that_arent_fl_projects_are_refused() {
    let dir = temp_dir("flp-refused");
    let path = dir.join("song.flp");
    std::fs::write(&path, b"RIFF....WAVEfmt ").unwrap();
    let err = Dialog::open(&path, None, Default::default())
        .err()
        .expect("refused");
    assert!(err.contains("not an FL Studio project"), "{err}");
    assert!(Dialog::open(&dir.join("missing.flp"), None, Default::default()).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

/// The plugins in `CANOPY_REAL_PLUGINS` (plugin files, as for
/// `hosting`'s real-plugin test), as a scan lists them.
fn real_plugins() -> Vec<::plugins::PluginInfo> {
    let files = std::env::var_os("CANOPY_REAL_PLUGINS").expect("CANOPY_REAL_PLUGINS");
    std::env::split_paths(&files)
        .flat_map(|path| {
            let format = match path.extension().and_then(|e| e.to_str()) {
                Some("clap") => ::plugins::PluginFormat::Clap,
                Some("vst3") => ::plugins::PluginFormat::Vst3,
                _ => ::plugins::PluginFormat::Vst2,
            };
            ::plugins::scan::read_plugin_file(format, &path).unwrap_or_default()
        })
        .collect()
}

/// A real FL project (`CANOPY_FLP_WITH_PLUGINS`) whose third-party plugins
/// are installed here (`CANOPY_REAL_PLUGINS`): they come in as themselves,
/// with the settings FL saved, and play.
#[test]
#[ignore = "needs CANOPY_FLP_WITH_PLUGINS and CANOPY_REAL_PLUGINS"]
fn installed_plugins_come_in_as_themselves() {
    let flp = PathBuf::from(std::env::var_os("CANOPY_FLP_WITH_PLUGINS").expect("a project"));
    let mut rig = Rig::wide("flp plugins");
    rig.app_mut()
        .session
        .plugins
        .manager
        .catalog_mut()
        .set(real_plugins());
    let installed = crate::flp_import::installed(rig.app().session.plugins.manager.catalog());
    let dir = temp_dir("flp-plugins");
    let dialog = Dialog::open(&flp, Some(dir.join("sfz")), installed).unwrap();
    let hosted: Vec<String> = dialog
        .plan()
        .items
        .iter()
        .filter(|i| dialog.choice(i) == flp::Choice::Plugin)
        .map(|i| i.name.clone())
        .collect();
    println!("as themselves: {hosted:?}");
    for line in &dialog.preview().report {
        println!("  {line}");
    }
    assert!(!hosted.is_empty(), "something installed is used");
    rig.app_mut().flp_import = Some(dialog);
    rig.frames(3);
    let dialog = rig.app_mut().flp_import.take().unwrap();
    dialog.import_into(&mut rig.app_mut().session);
    rig.frames(5);

    let session = &rig.app().session;
    let mut plugin_tracks = Vec::new();
    session.project.visit(|track, _| {
        if let TrackKind::Midi {
            instrument, params, ..
        } = &track.kind
            && !instrument.format.is_builtin()
        {
            plugin_tracks.push((track.id, track.name.clone(), params.len()));
        }
    });
    assert!(!plugin_tracks.is_empty(), "a plugin instrument came in");
    // FL's own patches, not the plugin's defaults: they differ from those,
    // and from one another.
    let patch = |track: project::TrackId| -> Vec<f32> {
        session.project.track(track).unwrap().instrument_values()
    };
    let defaults: Vec<f32> = match &session.project.track(plugin_tracks[0].0).unwrap().kind {
        TrackKind::Midi { instrument, .. } => instrument
            .descriptor()
            .unwrap()
            .params
            .iter()
            .map(|p| p.default_value())
            .collect(),
        _ => unreachable!(),
    };
    let patches: Vec<Vec<f32>> = plugin_tracks.iter().map(|(t, _, _)| patch(*t)).collect();
    for (patch, (_, name, _)) in patches.iter().zip(&plugin_tracks) {
        let changed = patch
            .iter()
            .zip(&defaults)
            .filter(|(a, b)| (*a - *b).abs() > 1e-4)
            .count();
        println!("{name}: {changed} values away from Vital's defaults");
        assert!(changed > 0, "{name} has FL's settings");
    }
    let distinct: std::collections::HashSet<Vec<u32>> = patches
        .iter()
        .map(|p| p.iter().map(|v| v.to_bits()).collect())
        .collect();
    assert!(distinct.len() > 1, "different channels, different patches");
    for (track, name, params) in &plugin_tracks {
        let status = session
            .plugins
            .manager
            .status(hosting::Owner::Instrument(*track));
        println!("{name}: {status:?}, {params} values filled in");
        assert_eq!(status, hosting::Status::Loaded);
        assert!(*params > 0, "the plugin's values are in the project");
    }
    let (track, _, _) = plugin_tracks[0];
    let on = midi::NoteEventKind::On {
        pitch: midi::Pitch::A4,
        velocity: midi::Velocity::MAX,
    };
    rig.app_mut().session.preview_note(track, on);
    rig.frames(1);
    let audio = rig.render_audio(48_000);
    println!("level {}", peak(&audio));
    assert!(peak(&audio) > 0.01, "it plays");
    let _ = std::fs::remove_dir_all(dir);
}
