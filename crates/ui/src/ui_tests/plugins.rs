//! Plugins in the app, with the test plugins (`plugins::fixtures`) as the
//! installed ones: adding an effect and an instrument from the menus, the
//! plugin's own value text on its knobs, presets keeping its state, and a
//! project naming a plugin that isn't installed.

use ::plugins::fixtures;
use ::plugins::{PluginFormat, PluginInfo};
use egui_kittest::kittest::Queryable;
use hosting::{Owner, Status};
use instruments::Format;
use midi::{NoteEventKind, Pitch, Velocity};
use project::TrackKind;

use super::{Rig, peak};

/// The test plugins, as a scan would list them.
fn installed() -> Vec<PluginInfo> {
    let fixtures = fixtures::build().expect("the test plugins build");
    let mut infos = ::plugins::scan::read_plugin_file(PluginFormat::Clap, &fixtures.clap).unwrap();
    infos.extend(::plugins::scan::read_plugin_file(PluginFormat::Vst3, &fixtures.vst3).unwrap());
    for vst2 in [&fixtures.vst2_gain, &fixtures.vst2_sine] {
        infos.extend(::plugins::scan::read_plugin_file(PluginFormat::Vst2, vst2).unwrap());
    }
    infos
}

fn rig(name: &'static str) -> Rig {
    let mut rig = Rig::wide(name);
    rig.app_mut()
        .session
        .plugins
        .manager
        .catalog_mut()
        .set(installed());
    rig
}

/// Opens "Plugins" in the menu `menu` opens, then the vendor's submenu,
/// and picks `plugin`.
fn pick(rig: &mut Rig, menu: &str, plugin: &str) {
    rig.harness.get_by_label(menu).click();
    rig.frames(2);
    // Submenus are labelled with their arrow.
    rig.harness.get_by_label_contains("Plugins").hover();
    rig.frames(2);
    rig.harness.get_by_label_contains("canopy").hover();
    rig.frames(2);
    rig.screenshot("menu");
    rig.harness.get_by_label(plugin).click();
    rig.frames(3);
}

#[test]
fn a_plugin_effect_comes_from_the_effect_menu() {
    let mut rig = rig("plugin effect");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    pick(&mut rig, "+ Effect", "Canopy Test Gain  (CLAP)");

    let session = &rig.app().session;
    let devices = session.project.devices(Some(track)).unwrap();
    assert_eq!(devices.len(), 1);
    let device = &devices[0];
    assert_eq!(device.effect.format, Format::Clap);
    assert_eq!(device.name(), "Canopy Test Gain");
    assert!(
        device.plugin.as_ref().is_some_and(|p| !p.state.is_empty()),
        "its first state is kept, for undo"
    );
    assert_eq!(session.undo_label(), Some("Add Canopy Test Gain"));
    assert_eq!(
        session.plugins.manager.status(Owner::Effect(device.id)),
        Status::Loaded
    );
    // Its parameters are on the panel, the output-only ones left out.
    let id = device.id;
    rig.harness.get_by_label("Gain");
    rig.harness.get_by_label("Latency");
    assert!(rig.harness.query_by_label("Tempo seen").is_none());
    rig.screenshot("added");

    // Undo takes it away; redo brings back the same instance.
    rig.app_mut().session.undo();
    rig.frames(3);
    assert!(
        rig.app()
            .session
            .project
            .devices(Some(track))
            .unwrap()
            .is_empty()
    );
    rig.app_mut().session.redo();
    rig.frames(3);
    assert_eq!(
        rig.app().session.plugins.manager.status(Owner::Effect(id)),
        Status::Loaded
    );
}

#[test]
fn every_format_comes_from_the_same_menu() {
    let mut rig = rig("plugin formats");
    let track = rig.app().session.project.track_ids()[2];
    rig.click(rig.header_point(track));
    rig.frames(2);
    for format in ["VST3", "VST2"] {
        pick(
            &mut rig,
            "+ Effect",
            &format!("Canopy Test Gain  ({format})"),
        );
    }
    let formats: Vec<Format> = rig
        .app()
        .session
        .project
        .devices(Some(track))
        .unwrap()
        .iter()
        .map(|d| d.effect.format)
        .collect();
    assert_eq!(formats, [Format::Vst3, Format::Vst2]);
    let session = &rig.app().session;
    for device in session.project.devices(Some(track)).unwrap() {
        assert_eq!(
            session.plugins.manager.status(Owner::Effect(device.id)),
            Status::Loaded
        );
    }
    rig.screenshot("formats");
}

#[test]
fn a_plugin_instrument_plays_from_the_instrument_picker() {
    let mut rig = rig("plugin instrument");
    let track = rig.app().session.project.track_ids()[0];
    rig.click(rig.header_point(track));
    rig.frames(2);
    pick(&mut rig, "Plugins", "Canopy Test Sine  (CLAP)");
    let session = &rig.app().session;
    let TrackKind::Midi {
        instrument, plugin, ..
    } = &session.project.track(track).unwrap().kind
    else {
        panic!("a MIDI track");
    };
    assert_eq!(instrument.format, Format::Clap);
    assert_eq!(plugin.as_ref().unwrap().name, "Canopy Test Sine");

    // A note through it.
    rig.frames(2);
    let on = NoteEventKind::On {
        pitch: Pitch::A4,
        velocity: Velocity::MAX,
    };
    rig.app_mut().session.preview_note(track, on);
    rig.frames(1);
    let audio = rig.render_audio(4800);
    assert!(peak(&audio) > 0.1, "it sounds: {}", peak(&audio));
    rig.screenshot("instrument");

    // Back to a built-in, then undone back to the plugin.
    rig.app_mut().session.edit("Built-in", |p| {
        p.set_instrument(track, instruments::InstrumentRef::default())
    });
    rig.frames(2);
    rig.app_mut().session.undo();
    rig.frames(3);
    assert_eq!(
        rig.app()
            .session
            .plugins
            .manager
            .status(Owner::Instrument(track)),
        Status::Loaded
    );
}

#[test]
fn a_plugin_that_isnt_installed_says_so_and_can_be_removed() {
    let mut rig = rig("missing plugin");
    let track = rig.app().session.project.track_ids()[2];
    // A project from elsewhere, naming a plugin this computer doesn't have.
    let mut elsewhere = rig.app().session.project.clone();
    let info = &installed()[0];
    let loaded = rig.app_mut().session.plugins.manager.load(info).unwrap();
    let device = elsewhere
        .add_device(Some(track), loaded.effect_ref(), None)
        .unwrap();
    elsewhere
        .set_device_plugin(device, Some(loaded.data.clone()))
        .unwrap();
    let text = elsewhere
        .to_file_string()
        .unwrap()
        .replace(&info.id, "com.example.missing")
        .replace(&info.name, "Missing Thing");
    let elsewhere = project::Project::from_file_string(&text).unwrap();
    rig.app_mut().session.edit("Open", |p| {
        *p = elsewhere;
        Ok(())
    });
    rig.click(rig.header_point(track));
    rig.frames(3);
    rig.harness
        .get_by_label("Missing Thing (canopy, CLAP) isn't installed, so the sound passes through.");
    rig.screenshot("missing");
    rig.harness.get_by_label("Remove").click();
    rig.frames(2);
    assert!(
        rig.app()
            .session
            .project
            .devices(Some(track))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_plugin_preset_keeps_its_state() {
    let mut rig = rig("plugin preset");
    let dir = std::env::temp_dir().join(format!("canopy-plugin-presets-{}", std::process::id()));
    rig.app_mut().session.presets_dir = Some(dir.clone());
    let track = rig.app().session.project.track_ids()[2];
    let info = installed()
        .into_iter()
        .find(|i| i.id == fixtures::CLAP_GAIN)
        .unwrap();
    let device =
        crate::plugins::add_effect(&mut rig.app_mut().session, Some(track), &info).unwrap();
    rig.frames(2);
    let target = crate::session::PresetTarget::Device(device);
    rig.app_mut().session.save_preset(target, "Quiet");
    let presets = rig.app().session.presets(target);
    assert_eq!(presets.len(), 1);
    let saved = std::fs::read_to_string(&presets[0].path).unwrap();
    assert!(
        saved.contains("state:"),
        "the plugin's state is in the preset"
    );
    rig.app_mut()
        .session
        .edit("Louder", |p| p.set_device_param(device, "0", 6.0));
    rig.app_mut().session.load_preset(target, &presets[0]);
    assert_eq!(rig.app().session.undo_label(), Some("Load Preset"));
    let gain = rig.app().session.project.device(device).unwrap().param("0");
    assert_eq!(gain, Some(0.0), "the preset's value is back");
    let _ = std::fs::remove_dir_all(dir);
}
