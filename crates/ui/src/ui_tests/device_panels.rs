//! Every effect's and instrument's panel with a chord playing through it,
//! so displays and meters have something to show: run with
//! `CANOPY_SCREENSHOTS` to look at them. Each panel's controls are also
//! checked to reach the project.

use eframe::egui;
use egui_kittest::kittest::Queryable;
use midi::{Pitch, Tick, Velocity};
use project::{EffectRef, Note, TrackId};

use super::Rig;

/// A chord held over four bars on the first MIDI track, inside a group
/// (whose device view has room for wide effects), playing. Returns the
/// MIDI track and the group.
fn chord_playing(rig: &mut Rig) -> (TrackId, TrackId) {
    let midi = rig.app().session.project.track_ids()[0];
    let group = rig
        .app_mut()
        .session
        .edit("Add", |p| {
            let bar = p.tempo.bar_len();
            let clip = p.add_midi_clip(midi, Tick::ZERO, bar * 4)?;
            let notes: Vec<_> = [48, 55, 60, 64, 67]
                .iter()
                .map(|pitch| Note {
                    id: p.new_note_id(),
                    start: Tick::ZERO,
                    len: bar * 4,
                    pitch: Pitch::new(*pitch).unwrap(),
                    velocity: Velocity::from_midi1(100),
                    release_velocity: Velocity::CENTER,
                    muted: false,
                    bend: Vec::new(),
                })
                .collect();
            p.set_notes(clip, notes)?;
            p.group_tracks(&[midi])
        })
        .unwrap();
    rig.app_mut().session.toggle_playing();
    rig.frames(2);
    (midi, group)
}

/// Plays a moment of audio, a frame at a time, so the UI keeps up.
fn listen(rig: &mut Rig, seconds: f32) {
    let blocks = (seconds * 60.0) as usize;
    for _ in 0..blocks {
        rig.render_audio(800);
    }
}

fn select(rig: &mut Rig, track: TrackId) {
    let point = rig.header_point(track);
    rig.click(point);
    rig.frames(2);
}

#[test]
fn every_effect_panel() {
    let mut rig = Rig::new("effect_panels");
    let (_, group) = chord_playing(&mut rig);
    select(&mut rig, group);
    for descriptor in effects::builtins() {
        let device = rig
            .app_mut()
            .session
            .edit("Add Effect", |p| {
                p.add_device(Some(group), EffectRef::builtin(descriptor.id), None)
            })
            .unwrap();
        rig.harness
            .ctx
            .data_mut(|d| d.insert_temp(egui::Id::new("reveal_effect"), device));
        listen(&mut rig, 0.6);
        rig.screenshot(descriptor.id.trim_start_matches("canopy."));
        if descriptor.id == "canopy.analyzer" {
            // Popped out, then back.
            rig.harness.get_by_label("Pop Out").click();
            listen(&mut rig, 0.6);
            rig.screenshot("analyzer_window");
            rig.harness.get_by_label("Pop Out").click();
            rig.frames(2);
        }
        rig.app_mut()
            .session
            .edit("Remove Effect", |p| p.remove_device(device));
        rig.frames(2);
    }
}

#[test]
fn a_long_sfz_name_leaves_the_polyphony_menu_its_room() {
    let mut rig = Rig::new("sampler_long_name");
    let dir = super::temp_dir("sampler_long_name");
    super::write_sine_wav(&dir.join("tone.wav"), 1, 440.0);
    let name = format!(
        "{} Grand Piano Close Mics Pedal Down.sfz",
        "Very ".repeat(12)
    );
    std::fs::write(dir.join(&name), "<region> sample=tone.wav\n").unwrap();
    let midi = rig.app().session.project.track_ids()[0];
    select(&mut rig, midi);
    let reference = instruments::InstrumentRef::builtin("canopy.sampler");
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| p.set_instrument(midi, reference));
    rig.app_mut()
        .session
        .set_instrument_file(midi, dir.join(&name));
    rig.wait_for("the bank to load", |app| {
        matches!(
            app.session.bank(midi),
            Some(crate::banks::BankState::Ready(_))
        )
    });
    rig.frames(2);
    rig.screenshot("long name");
    // The panel's (the track's header names it too, higher up).
    let shown = rig
        .harness
        .get_all_by_label_contains("Grand Piano")
        .map(|n| n.rect())
        .max_by(|a, b| a.top().total_cmp(&b.top()))
        .unwrap();
    let polyphony = rig.harness.get_by_label("Polyphony").rect();
    assert!(
        shown.right() <= polyphony.left(),
        "the name ends at {} and Polyphony starts at {}",
        shown.right(),
        polyphony.left()
    );
}

#[test]
fn load_an_sfz_and_play_it() {
    let mut rig = Rig::new("sampler");
    let dir = super::temp_dir("sampler");
    super::write_sine_wav(&dir.join("A4 tone.wav"), 1, 440.0);
    std::fs::write(
        dir.join("piano.sfz"),
        "<group> ampeg_release=0.05\n<region> sample=A4 tone.wav lokey=c3 hikey=c6 pitch_keycenter=a4\n",
    )
    .unwrap();
    let midi = rig.app().session.project.track_ids()[0];
    select(&mut rig, midi);
    let reference = instruments::InstrumentRef::builtin("canopy.sampler");
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| p.set_instrument(midi, reference));
    rig.frames(2);
    rig.harness.get_by_label("Load SFZ…");
    rig.screenshot("empty");

    // What Load SFZ… does once a file is picked.
    rig.app_mut()
        .session
        .set_instrument_file(midi, dir.join("piano.sfz"));
    rig.wait_for("the bank to load", |app| {
        matches!(
            app.session.bank(midi),
            Some(crate::banks::BankState::Ready(_))
        )
    });
    rig.frames(2);
    rig.harness.get_by_label_contains("1 region ·");

    // A note through the keyboard map's audition plays the sample.
    let on = midi::NoteEventKind::On {
        pitch: Pitch::A4,
        velocity: Velocity::MAX,
    };
    rig.app_mut().session.preview_note(midi, on);
    let out = rig.render_audio(4800);
    assert!(super::peak(&out) > 0.05, "it plays: {}", super::peak(&out));
    rig.screenshot("loaded");

    // A new clip's piano roll opens on the keys the instrument plays.
    rig.double_click(rig.lane_point(midi, 1.1));
    rig.frames(2);
    rig.screenshot("piano roll");

    // Saved with the project, and loaded again from it.
    let file = rig
        .app()
        .session
        .project
        .track(midi)
        .unwrap()
        .instrument_file();
    assert_eq!(file, Some(dir.join("piano.sfz").as_path()));
}

#[test]
fn drop_an_sfz_for_a_sampler_track() {
    let mut rig = Rig::new("sfz_drop");
    let dir = super::temp_dir("sfz_drop");
    super::write_sine_wav(&dir.join("tone.wav"), 1, 440.0);
    let sfz = dir.join("Flute.sfz");
    std::fs::write(&sfz, "<region> sample=tone.wav pitch_keycenter=69\n").unwrap();

    // Dropped below the tracks: a new track, named after the file.
    let before = rig.app().session.project.track_ids().len();
    let last = *rig.app().session.project.track_ids().last().unwrap();
    let below = rig.lane_point(last, 2.0) + egui::vec2(0.0, 80.0);
    rig.event(egui::Event::PointerMoved(below));
    rig.harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(super::TestFile(sfz.clone())));
    rig.frames(2);
    let project = &rig.app().session.project;
    assert_eq!(project.track_ids().len(), before + 1);
    let track = project
        .track_ids()
        .into_iter()
        .find(|t| project.track(*t).unwrap().name == "Flute")
        .expect("a Flute track");
    assert_eq!(
        project.track(track).unwrap().instrument_file(),
        Some(sfz.as_path())
    );
    assert_eq!(rig.app().session.undo_label(), Some("Load SFZ"));
    rig.wait_for("the bank", |app| {
        matches!(
            app.session.bank(track),
            Some(crate::banks::BankState::Ready(_))
        )
    });

    // Dropped on a MIDI track: that track plays it.
    let midi = rig.app().session.project.track_ids()[0];
    rig.event(egui::Event::PointerMoved(rig.lane_point(midi, 2.0)));
    rig.harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(super::TestFile(sfz.clone())));
    rig.frames(2);
    let track = rig.app().session.project.track(midi).unwrap();
    assert_eq!(track.instrument_file(), Some(sfz.as_path()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn every_instrument_panel() {
    let mut rig = Rig::new("instrument_panels");
    let (midi, _) = chord_playing(&mut rig);
    select(&mut rig, midi);
    for descriptor in instruments::builtins() {
        let reference = instruments::InstrumentRef::builtin(descriptor.id);
        rig.app_mut()
            .session
            .edit("Change Instrument", |p| p.set_instrument(midi, reference));
        // Stop (back to the start) and play again, so the chord starts on
        // the new instrument.
        rig.app_mut().session.toggle_playing();
        rig.render_audio(4800);
        rig.app_mut().session.toggle_playing();
        listen(&mut rig, 0.6);
        rig.screenshot(descriptor.id.trim_start_matches("canopy."));
        if descriptor.id == "canopy.wavetable" {
            for page in ["Envelopes & LFOs", "Matrix", "Oscillators"] {
                rig.harness.get_by_label(page).click();
                listen(&mut rig, 0.2);
                rig.screenshot(&format!("wavetable {page}"));
            }
        }
    }
}

#[test]
fn route_an_lfo_to_the_cutoff_in_the_matrix() {
    use instruments::builtin::wavetable as wt;
    let mut rig = Rig::new("matrix");
    let midi = rig.app().session.project.track_ids()[0];
    select(&mut rig, midi);
    let reference = instruments::InstrumentRef::builtin("canopy.wavetable");
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| p.set_instrument(midi, reference));
    rig.frames(2);
    rig.harness.get_by_label("Matrix").click();
    rig.frames(2);
    let param = |rig: &Rig, index: usize| {
        let id = wt::PARAMS[index].id;
        rig.app()
            .session
            .project
            .track(midi)
            .unwrap()
            .instrument_param(id)
            .unwrap()
    };
    // The first unused slot's destination menu: pick Cutoff.
    let slot = (0..wt::MOD_SLOTS)
        .find(|s| param(&rig, wt::slot_param(*s, wt::SLOT_DEST)) as usize == wt::DEST_NONE)
        .expect("a free slot");
    let dest = wt::slot_param(slot, wt::SLOT_DEST);
    // The slot's row starts with its number (in the device view, below the
    // ruler's bar numbers); an unused slot's source, destination and
    // scaling all read "None", left to right.
    let number = rig
        .harness
        .get_all_by_label(&format!("{}", slot + 1))
        .map(|n| n.rect().center())
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap();
    let mut row: Vec<_> = rig
        .harness
        .get_all_by_label(wt::DESTINATIONS[wt::DEST_NONE])
        .map(|n| n.rect().center())
        .filter(|p| (p.y - number.y).abs() < 6.0 && p.x > number.x && p.x < number.x + 400.0)
        .collect();
    row.sort_by(|a, b| a.x.total_cmp(&b.x));
    let menu = row[1];
    // Other slots may aim at the cutoff already: the menu's entry is the
    // "Cutoff" that appears when it opens.
    let cutoffs = |rig: &Rig| -> Vec<eframe::egui::Pos2> {
        rig.harness
            .get_all_by_label("Cutoff")
            .map(|n| n.rect().center())
            .collect()
    };
    let before = cutoffs(&rig);
    rig.click(menu);
    rig.frames(2);
    rig.screenshot("menu");
    let entry = cutoffs(&rig)
        .into_iter()
        .find(|p| !before.contains(p))
        .expect("the menu's Cutoff");
    rig.click(entry);
    rig.frames(2);
    assert_eq!(
        param(&rig, dest) as usize,
        wt::DEST_CUTOFF,
        "the slot now aims at the cutoff"
    );
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));
}
