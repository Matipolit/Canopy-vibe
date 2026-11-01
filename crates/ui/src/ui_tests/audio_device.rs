//! The status bar's device menu: sample rate, buffer size and output.

use egui_kittest::kittest::Queryable;
use engine::device::OutputDevice;

use super::Rig;

fn open_menu(rig: &mut Rig) {
    rig.harness.get_by_label_contains(" frames").click();
    rig.frames(2);
}

#[test]
fn the_device_menu_picks_an_output_and_a_buffer_size() {
    let mut rig = Rig::new("device_menu");
    rig.app_mut().session.test_outputs = vec![
        OutputDevice {
            name: "alsa_output.speakers".into(),
            description: "Speakers".into(),
        },
        OutputDevice {
            name: "bluez_output.headphones".into(),
            description: "Headphones".into(),
        },
    ];
    rig.frames(1);
    open_menu(&mut rig);
    rig.screenshot("menu");
    assert!(rig.harness.query_by_label("System default").is_some());
    rig.harness.get_by_label("Headphones").click();
    rig.frames(2);
    let settings = &rig.app().session.audio_settings;
    assert_eq!(settings.output.as_deref(), Some("bluez_output.headphones"));

    open_menu(&mut rig);
    rig.harness.get_by_label_contains("128 frames").click();
    rig.frames(2);
    let settings = &rig.app().session.audio_settings;
    assert_eq!(settings.buffer_frames, 128);
    assert_eq!(
        settings.output.as_deref(),
        Some("bluez_output.headphones"),
        "the rest stays"
    );

    open_menu(&mut rig);
    rig.harness.get_by_label("System default").click();
    rig.frames(2);
    assert_eq!(rig.app().session.audio_settings.output, None);
}

#[test]
fn without_pipewire_there_is_no_output_to_pick() {
    let mut rig = Rig::new("device_menu_alsa");
    open_menu(&mut rig);
    assert!(rig.harness.query_by_label("System default").is_none());
    assert!(
        rig.harness
            .query_by_label_contains("256 frames (")
            .is_some()
    );
}
