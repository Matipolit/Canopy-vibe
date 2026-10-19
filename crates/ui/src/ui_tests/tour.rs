//! A screenshot tour of every main view, at a laptop's and a desktop's
//! window size, for looking at the UI as a whole: run with
//! `CANOPY_SCREENSHOTS=<dir>`. Without it, it still drives every view once.

use eframe::egui::{self, Key, Modifiers, vec2};
use egui_kittest::kittest::Queryable;
use midi::{Pitch, Tick, Velocity};
use project::{AutomationPoint, AutomationTarget, ClipId, EffectRef, LoopRegion, Note, TrackId};

use super::Rig;
use crate::timeline::Action;

/// What the tour's song is made of.
struct Song {
    synth: TrackId,
    drums: TrackId,
    audio: TrackId,
    chords: ClipId,
    beat: ClipId,
}

fn note(p: &mut project::Project, start: i64, len: i64, pitch: u8, velocity: u8) -> Note {
    Note {
        id: p.new_note_id(),
        start: Tick::new(start),
        len: Tick::new(len),
        pitch: Pitch::new(pitch).unwrap(),
        velocity: Velocity::from_midi1(velocity),
        release_velocity: Velocity::CENTER,
        muted: false,
    }
}

/// Chords and a beat, an audio clip, automation, effects, a scale and a loop.
fn song(rig: &mut Rig, dir: &std::path::Path) -> Song {
    let ids = rig.app().session.project.track_ids();
    let (synth, drums, audio) = (ids[0], ids[1], ids[2]);
    let bar = 3840;
    let (chords, beat) = rig
        .app_mut()
        .session
        .edit("Song", |p| {
            p.rename_track(synth, "Keys")?;
            p.rename_track(drums, "Beat")?;
            p.rename_track(audio, "Vox")?;
            p.set_instrument(drums, instruments::InstrumentRef::builtin("canopy.drums"))?;
            let chords = p.add_midi_clip(synth, Tick::ZERO, Tick::new(4 * bar))?;
            let mut notes = Vec::new();
            for (i, root) in [48u8, 44, 51, 46].iter().enumerate() {
                let at = i as i64 * bar;
                for (k, interval) in [0u8, 3, 7, 12].iter().enumerate() {
                    notes.push(note(p, at, bar - 240, root + interval, 70 + 12 * k as u8));
                }
                notes.push(note(p, at + 1920, 480, root + 19, 90));
            }
            p.set_notes(chords, notes)?;
            let second = p.add_midi_clip(synth, Tick::new(5 * bar), Tick::new(2 * bar))?;
            let notes = (0..8)
                .map(|i| note(p, i * 960, 480, 60 + (i % 4) as u8 * 2, 100))
                .collect();
            p.set_notes(second, notes)?;
            let beat = p.add_midi_clip(drums, Tick::ZERO, Tick::new(2 * bar))?;
            let mut notes = Vec::new();
            for step in 0..32 {
                let at = step * 240;
                if step % 8 == 0 {
                    notes.push(note(p, at, 240, 36, 120));
                }
                if step % 8 == 4 {
                    notes.push(note(p, at, 240, 38, 110));
                }
                notes.push(note(p, at, 120, 42, 60 + (step % 2) as u8 * 40));
            }
            p.set_notes(beat, notes)?;
            let points = [(0, 0.8), (2 * bar, 0.35), (4 * bar, 0.9), (6 * bar, 0.6)]
                .map(|(tick, value)| AutomationPoint {
                    tick: Tick::new(tick),
                    value,
                    curve: 0.0,
                })
                .to_vec();
            p.set_automation(synth, AutomationTarget::Volume, points)?;
            for effect in [
                "canopy.eq",
                "canopy.compressor",
                "canopy.reverb",
                "canopy.delay",
            ] {
                p.add_device(Some(synth), EffectRef::builtin(effect), None)?;
            }
            p.set_scale(Some(project::Scale::new(0, project::ScaleMode::Minor)))?;
            Ok((chords, beat))
        })
        .unwrap();
    rig.app_mut().session.set_loop(Some(LoopRegion {
        start: Tick::new(2 * bar),
        end: Tick::new(4 * bar),
        on: true,
    }));
    rig.app_mut()
        .timeline
        .automation
        .insert(synth, vec![AutomationTarget::Volume]);

    let wav = dir.join("vox take.wav");
    super::write_sine_wav(&wav, 3, 330.0);
    let app = rig.app_mut();
    let position = app.timeline.new_track_position(&app.session.project);
    app.timeline.apply(
        &mut app.session,
        Action::Import(vec![wav], Some(audio), Tick::new(bar), position),
    );
    rig.wait_for("the audio", |app| {
        let project = &app.session.project;
        !project.track(audio).unwrap().clips().is_empty()
            && project
                .sources
                .first()
                .is_some_and(|s| app.session.source_state(s.id).is_some())
    });
    rig.frames(3);
    Song {
        synth,
        drums,
        audio,
        chords,
        beat,
    }
}

/// Lets fades and slides finish, then saves a screenshot: the tour shows
/// how things look at rest, not halfway through an animation.
fn shot(rig: &mut Rig, label: &str) {
    rig.frames(20);
    rig.screenshot(label);
}

fn select_track(rig: &mut Rig, track: TrackId) {
    let point = rig.header_point(track);
    rig.click(point);
    rig.frames(2);
}

fn open_clip(rig: &mut Rig, clip: ClipId) {
    let rect = rig.app().timeline.clip_rect(clip).expect("clip on screen");
    rig.double_click(rect.center() + vec2(0.0, 6.0));
    rig.frames(3);
}

fn open_menu(rig: &mut Rig, name: &str, label: &str) {
    rig.harness.get_by_label(name).click();
    shot(rig, label);
    rig.key(Modifiers::NONE, Key::Escape);
    rig.frames(2);
}

fn tour(name: &'static str, size: egui::Vec2) {
    let mut rig = Rig::with_size(name, size);
    let dir = super::temp_dir(name);
    let song = song(&mut rig, &dir);

    // The arrangement with the synth's device view.
    select_track(&mut rig, song.synth);
    shot(&mut rig, "device_synth");

    // A knob with the keyboard's focus (as Tab would give it); Escape lets
    // go of it.
    rig.harness.get_by_label("Cutoff").focus();
    shot(&mut rig, "knob_focus");
    rig.key(Modifiers::NONE, Key::Escape);

    // An effect halfway through a drag along its chain: where it would
    // land, and what's moving. Let go where it started.
    let eq = rig
        .harness
        .get_all_by_label("EQ")
        .map(|n| n.rect().center())
        .find(|p| p.y > size.y / 2.0)
        .expect("the EQ's title bar");
    // Over the compressor's left half, if it's in view: the line goes
    // between the two. Otherwise just off to the side.
    let target = rig
        .harness
        .get_all_by_label("Compressor")
        .map(|n| n.rect().left_center() + vec2(4.0, 40.0))
        .find(|p| p.y > size.y / 2.0 && p.x < size.x - 20.0)
        .unwrap_or(eq + vec2(-160.0, 40.0));
    rig.event(egui::Event::PointerMoved(eq));
    rig.press(eq, true, Modifiers::NONE);
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        rig.event(egui::Event::PointerMoved(eq + (target - eq) * t));
    }
    shot(&mut rig, "effect_dragging");
    // Let go over the room under the tracks, where nothing takes it.
    let last = *rig.app().session.project.track_ids().last().unwrap();
    let nowhere = rig.app().timeline.row_rect(last).unwrap().left_bottom() + vec2(600.0, 40.0);
    rig.event(egui::Event::PointerMoved(nowhere));
    rig.press(nowhere, false, Modifiers::NONE);
    rig.frames(2);

    // A track halfway through a drag down the list; Escape puts it back.
    let header = rig.header_point(song.synth);
    let to = rig.header_point(song.audio);
    rig.event(egui::Event::PointerMoved(header));
    rig.press(header, true, Modifiers::NONE);
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        rig.event(egui::Event::PointerMoved(header + (to - header) * t));
    }
    shot(&mut rig, "track_dragging");
    rig.key(Modifiers::NONE, Key::Escape);
    rig.press(to, false, Modifiers::NONE);
    rig.frames(2);

    // The piano roll, on the chords (with some notes selected) and on the
    // beat.
    open_clip(&mut rig, song.chords);
    shot(&mut rig, "piano_roll");
    // A click in it gives it the keys; Ctrl+A then selects the notes.
    rig.click(rig.app().piano_roll.point(4800.0, 62));
    rig.key(Modifiers::COMMAND, Key::A);
    shot(&mut rig, "piano_roll_selected");
    // Its right-click menu, on empty space.
    let empty = rig.app().piano_roll.point(4800.0, 62);
    rig.right_click(empty);
    shot(&mut rig, "piano_roll_menu");
    rig.key(Modifiers::NONE, Key::Escape);
    rig.frames(2);
    // Its Tools menu, and the Strum Pattern window previewing on the
    // selected chords (Escape cancels it).
    open_menu(&mut rig, "Tools", "piano_roll_tools");
    rig.click(rig.app().piano_roll.point(4800.0, 62));
    rig.key(Modifiers::COMMAND, Key::A);
    rig.key(Modifiers::ALT, Key::P);
    shot(&mut rig, "strum_pattern");
    rig.key(Modifiers::NONE, Key::Escape);
    rig.frames(2);
    open_clip(&mut rig, song.beat);
    shot(&mut rig, "piano_roll_drums");

    // The drum kit's panel.
    rig.key(Modifiers::SHIFT, Key::Tab);
    shot(&mut rig, "device_drums");

    // An audio clip's settings.
    let clip = rig.app().session.project.track(song.audio).unwrap().clips()[0].id();
    open_clip(&mut rig, clip);
    shot(&mut rig, "clip_panel");

    // A clip's context menu, and a clip halfway through a drag.
    let rect = rig.app().timeline.clip_rect(song.chords).unwrap();
    rig.right_click(rect.center() + vec2(0.0, 6.0));
    shot(&mut rig, "clip_menu");
    rig.key(Modifiers::NONE, Key::Escape);
    let from = rect.center() + vec2(0.0, 6.0);
    rig.event(egui::Event::PointerMoved(from));
    rig.press(from, true, Modifiers::NONE);
    for i in 1..=8 {
        rig.event(egui::Event::PointerMoved(from + vec2(16.0 * i as f32, 0.0)));
    }
    shot(&mut rig, "clip_dragging");
    rig.key(Modifiers::NONE, Key::Escape);
    rig.press(from + vec2(128.0, 0.0), false, Modifiers::NONE);
    rig.frames(2);

    // The sampler and the guitar, on the beat's track.
    rig.key(Modifiers::SHIFT, Key::Tab);
    select_track(&mut rig, song.drums);
    for (id, label) in [
        ("canopy.sampler", "device_sampler"),
        ("canopy.guitar", "device_guitar"),
    ] {
        rig.app_mut().session.edit("Instrument", |p| {
            p.set_instrument(song.drums, instruments::InstrumentRef::builtin(id))
        });
        shot(&mut rig, label);
    }

    // The Wavetable's pages, on the same track.
    rig.app_mut().session.edit("Instrument", |p| {
        p.set_instrument(
            song.drums,
            instruments::InstrumentRef::builtin("canopy.wavetable"),
        )
    });
    shot(&mut rig, "device_wavetable");
    for (page, label) in [
        ("Envelopes & LFOs", "device_wavetable_lfos"),
        ("Matrix", "device_wavetable_matrix"),
        ("Voicing", "device_wavetable_voicing"),
    ] {
        rig.harness.get_by_label(page).click();
        shot(&mut rig, label);
    }

    // A track with no effects, and the master's chain.
    select_track(&mut rig, song.audio);
    shot(&mut rig, "device_empty");

    // The amp's and the creative effects' panels, two at a time.
    for (pair, label) in [
        (["canopy.amp", "canopy.waveshaper"], "device_amp"),
        (["canopy.shaper", "canopy.vocoder"], "device_shaper"),
    ] {
        let added = rig
            .app_mut()
            .session
            .edit("Effects", |p| {
                pair.iter()
                    .map(|id| p.add_device(Some(song.audio), EffectRef::builtin(id), None))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        shot(&mut rig, label);
        rig.app_mut().session.edit("Remove", |p| {
            added.iter().try_for_each(|device| p.remove_device(*device))
        });
    }
    rig.harness.get_by_label("Master").click();
    shot(&mut rig, "device_master");

    // The menus.
    for menu in ["File", "Edit", "View", "Track", "MIDI", "Help"] {
        open_menu(&mut rig, menu, &format!("menu_{}", menu.to_lowercase()));
    }

    // Where the keyboard is, after a few Tabs.
    for _ in 0..3 {
        rig.key(Modifiers::NONE, Key::Tab);
    }
    shot(&mut rig, "keyboard_focus");
    // A click on the room under the tracks takes the focus away again.
    let last = *rig.app().session.project.track_ids().last().unwrap();
    let below = rig.app().timeline.row_rect(last).unwrap().left_bottom() + vec2(600.0, 40.0);
    rig.click(below);
    rig.frames(2);

    // Keyboard shortcuts.
    rig.key(Modifiers::NONE, Key::F1);
    shot(&mut rig, "shortcuts");
    rig.key(Modifiers::NONE, Key::F1);

    // The Export dialog.
    rig.key(Modifiers::COMMAND, Key::R);
    shot(&mut rig, "export");
    rig.key(Modifiers::NONE, Key::Escape);
    assert!(rig.app().export_dialog().is_none(), "Escape closes it");

    // Short rows: Alt+wheel over the tracks.
    let over = rig.lane_point(song.synth, 7.0);
    rig.event(egui::Event::PointerMoved(over));
    rig.event(egui::Event::ModifiersChanged(Modifiers::ALT));
    for _ in 0..4 {
        rig.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -60.0),
            modifiers: Modifiers::ALT,
            phase: egui::TouchPhase::Move,
        });
    }
    rig.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    shot(&mut rig, "compact_rows");

    // The Clip tab with nothing open.
    rig.app_mut().timeline.selection.open_clip = None;
    rig.key(Modifiers::SHIFT, Key::Tab);
    shot(&mut rig, "clip_none");

    // The FL Studio import dialog.
    let flp = super::flp_import::write_song(&dir);
    let dialog =
        crate::flp_import::Dialog::open(&flp, Some(dir.join("sfz")), Default::default()).unwrap();
    rig.app_mut().flp_import = Some(dialog);
    shot(&mut rig, "flp_import");
    rig.app_mut().flp_import = None;
    rig.frames(2);

    // A new, empty project.
    rig.app_mut().session.edit("Empty", |p| {
        let ids = p.track_ids();
        p.delete_tracks(&ids)
    });
    shot(&mut rig, "empty_project");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn screenshot_tour_laptop() {
    tour("tour_1280", vec2(1280.0, 800.0));
}

#[test]
fn screenshot_tour_desktop() {
    tour("tour_1920", vec2(1920.0, 1080.0));
}
