//! The Wavetable panel's controls, driven through the UI and checked in the
//! project: the second filter and its routing and types, drawn LFO shapes
//! (steps and nodes), a matrix slot's curve, polarity and bypass, the
//! grouped destination menu (with its envelope and LFO grids), the third
//! oscillator, a user wavetable dropped on an oscillator, a modulation
//! source dragged onto a knob and its ring dragged for depth, sources
//! dropped on envelope times, LFO settings and the volume, spectral
//! morphs, envelopes, the voicing page, the preset browser and the
//! wavetable editor. Each page is also screenshotted at full width
//! (`CANOPY_SCREENSHOTS`).

use std::sync::Arc;

use eframe::egui::{self, Modifiers, vec2};
use egui_kittest::kittest::Queryable;
use instruments::builtin::wavetable as wt;
use midi::{Pitch, Tick, Velocity};
use project::{Note, TrackId};

use super::Rig;
use crate::devices::Owner;
use crate::theme::space;

/// The first MIDI track on the Wavetable, selected, with a chord playing
/// so the displays have something to show.
fn wavetable(rig: &mut Rig) -> TrackId {
    let midi = rig.app().session.project.track_ids()[0];
    rig.click(rig.header_point(midi));
    rig.frames(2);
    let reference = instruments::InstrumentRef::builtin("canopy.wavetable");
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| {
            p.set_instrument(midi, reference)?;
            let bar = p.tempo.bar_len();
            let clip = p.add_midi_clip(midi, Tick::ZERO, bar * 4)?;
            let notes: Vec<_> = [48, 55, 60, 64]
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
            p.set_notes(clip, notes)
        })
        .unwrap();
    rig.app_mut().session.toggle_playing();
    for _ in 0..20 {
        rig.render_audio(800);
    }
    midi
}

/// Wavetable parameter `index` as the project has it.
fn param(rig: &Rig, track: TrackId, index: usize) -> f32 {
    let descriptor = &wt::PARAMS[index];
    rig.app()
        .session
        .project
        .track(track)
        .unwrap()
        .instrument_param(descriptor.id)
        .unwrap_or(descriptor.default_value())
}

fn page(rig: &mut Rig, name: &str) {
    rig.harness.get_by_label(name).click();
    rig.frames(2);
}

/// The lowest of the widgets labelled `label` on screen.
fn lowest(rig: &Rig, label: &str) -> egui::Pos2 {
    rig.harness
        .get_all_by_label(label)
        .map(|n| n.rect().center())
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap_or_else(|| panic!("no {label:?}"))
}

#[test]
fn route_osc_b_into_filter_2_in_parallel() {
    let mut rig = Rig::wide("wavetable_filters");
    let track = wavetable(&mut rig);
    rig.screenshot("oscillators");

    // Filter 2's tab, then its switch.
    page(&mut rig, "Filter 2");
    rig.harness.get_by_label("On").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, wt::FILTER2_ON), 1.0);

    // B in filter 2's row (the lower of the two) sends osc B there.
    rig.harness.get_by_label("Routing").click();
    rig.frames(2);
    let b = lowest(&rig, "B");
    rig.click(b);
    assert_eq!(param(&rig, track, wt::FILTER_OSC_B), 1.0);
    assert_eq!(param(&rig, track, wt::FILTER_TARGET_OSC_B), 1.0);
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));
    // Clicked again, B goes past both filters; once more, back into 2.
    rig.click(b);
    assert_eq!(param(&rig, track, wt::FILTER_OSC_B), 0.0);
    rig.click(b);
    assert_eq!(param(&rig, track, wt::FILTER_OSC_B), 1.0);

    // Side by side rather than one into the other.
    rig.harness.get_by_label("Serial").click();
    rig.frames(2);
    rig.harness.get_by_label("Parallel").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, wt::FILTER_ROUTING), 1.0);
    for _ in 0..10 {
        rig.render_audio(800);
    }
    rig.screenshot("filter 2 in parallel");
}

#[test]
fn draw_a_custom_lfo_shape() {
    let mut rig = Rig::wide("wavetable_lfo");
    let track = wavetable(&mut rig);
    page(&mut rig, "Envelopes & LFOs");
    rig.screenshot("envelopes and lfos");
    let shape = wt::lfo_param(0, wt::LFO_SHAPE);

    // LFO 1's shape menu reads "Sine": make it custom.
    rig.harness.get_by_label("Sine").click();
    rig.frames(2);
    rig.harness.get_by_label("Custom").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, shape) as usize, wt::SHAPE_CUSTOM);
    // The custom shape's glide knob shows (the voice's glide lives on
    // the Voicing page).
    assert_eq!(rig.harness.get_all_by_label("Glide").count(), 1);

    // Draw a rising line across the editor, bottom left to top right.
    let id = egui::Id::new(("panel", Owner::Instrument(track), ("lfo shape", 0usize)));
    let rect: egui::Rect = rig
        .harness
        .ctx
        .data(|d| d.get_temp(id.with("rect")))
        .expect("the shape editor is shown");
    // In small steps from the editor's very edge, since drawing starts
    // where the pointer is once it has moved far enough to be a drag.
    let area = rect.shrink(6.0);
    let from = egui::pos2(rect.left() + 0.5, area.bottom() - 1.0);
    let to = area.right_top() + vec2(-1.0, 1.0);
    rig.event(egui::Event::PointerMoved(from));
    rig.press(from, true, egui::Modifiers::NONE);
    for i in 1..=40 {
        rig.event(egui::Event::PointerMoved(
            from + (to - from) * (i as f32 / 40.0),
        ));
    }
    rig.press(to, false, egui::Modifiers::NONE);
    rig.frames(2);
    let points: Vec<f32> = (0..wt::LFO_POINTS)
        .map(|i| param(&rig, track, wt::lfo_param(0, wt::LFO_POINT + i)))
        .collect();
    println!("drawn points: {points:.2?}");
    assert!(
        points[0] < 0.1 && points[wt::LFO_POINTS - 1] > 0.9,
        "{points:?}"
    );
    assert!(points.windows(2).all(|w| w[1] >= w[0] - 1e-6), "{points:?}");
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));
    for _ in 0..10 {
        rig.render_audio(800);
    }
    rig.screenshot("custom lfo drawn");
}

#[test]
fn bend_a_slot_and_flip_its_polarity() {
    let mut rig = Rig::wide("wavetable_matrix");
    let track = wavetable(&mut rig);
    page(&mut rig, "Matrix");
    rig.screenshot("matrix");
    let curve = wt::slot_param(0, wt::SLOT_CURVE);
    let polarity = wt::slot_param(0, wt::SLOT_POLARITY);

    // Drag slot 1's curve up: it bends toward a fast start.
    let id = egui::Id::new(("panel", Owner::Instrument(track), ("curve", 0usize)));
    let rect: egui::Rect = rig
        .harness
        .ctx
        .data(|d| d.get_temp(id.with("rect")))
        .expect("the curve is shown");
    rig.drag(rect.center(), rect.center() - vec2(0.0, 25.0));
    let bent = param(&rig, track, curve);
    assert!(bent < -0.4, "{bent}");
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));
    // Ctrl+click puts it back.
    rig.click_with(rect.center(), Modifiers::COMMAND);
    assert_eq!(param(&rig, track, curve), 0.0);

    // Slot 1 reads Env 2, which is unipolar: its button shows "+". The
    // first click keeps it unipolar on purpose, the second makes it bipolar.
    let button = rig
        .harness
        .get_all_by_label("+")
        .map(|n| n.rect().center())
        .min_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap();
    rig.click(button);
    assert_eq!(param(&rig, track, polarity) as usize, wt::POLARITY_UNIPOLAR);
    rig.frames(45);
    rig.click(button);
    assert_eq!(param(&rig, track, polarity) as usize, wt::POLARITY_BIPOLAR);
    rig.harness
        .get_all_by_label("±")
        .next()
        .expect("shown as bipolar");

    // Slot 6's destination menu, in columns: B's second warp.
    let slot = 5;
    let dest = wt::slot_param(slot, wt::SLOT_DEST);
    rig.click(empty_destination(&rig, slot));
    rig.frames(2);
    for group in ["Osc B", "Osc C", "Filters", "Voice", "Envelopes", "LFOs"] {
        rig.harness.get_by_label(group);
    }
    // "Macros" heads the column at the right too.
    assert_eq!(rig.harness.get_all_by_label("Macros").count(), 2);
    rig.screenshot("destination menu");
    rig.harness.get_by_label("B Warp 2").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, dest) as usize, wt::DEST_WARP2_A + 1);
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));

    // Slot 7's: the envelopes and LFOs in grids, a row each. Env 3's
    // decay, and LFO 6's phase for slot 8.
    rig.click(empty_destination(&rig, slot + 1));
    rig.frames(2);
    rig.harness.get_by_label("Env 3 Decay").click();
    rig.frames(2);
    assert_eq!(
        param(&rig, track, wt::slot_param(slot + 1, wt::SLOT_DEST)) as usize,
        wt::env_dest(2, wt::ENV_DEST_DECAY)
    );
    rig.click(empty_destination(&rig, slot + 2));
    rig.frames(2);
    rig.harness.get_by_label("LFO 6 Phase").click();
    rig.frames(2);
    assert_eq!(
        param(&rig, track, wt::slot_param(slot + 2, wt::SLOT_DEST)) as usize,
        wt::lfo_dest(5, wt::LFO_DEST_PHASE)
    );
    // The row's menu says where it goes now.
    rig.harness.get_by_label("LFO 6 Phase");
}

#[test]
fn the_matrix_lists_only_routes_in_use() {
    let mut rig = Rig::wide("wavetable_matrix_rows");
    wavetable(&mut rig);
    page(&mut rig, "Matrix");
    // The default patch routes five slots: those, then one to start a sixth.
    for slot in 1..=6 {
        rig.harness.get_by_label(&format!("{slot}"));
    }
    assert!(rig.harness.query_by_label("7").is_none());
    rig.harness.get_by_label("+ Route");
}

#[test]
fn a_new_route_fills_the_first_free_slot() {
    let mut rig = Rig::wide("wavetable_new_route");
    let track = wavetable(&mut rig);
    page(&mut rig, "Matrix");
    // The "+ Route" row's source menu: aftertouch.
    let row = rig.harness.get_by_label("+ Route").rect().center();
    let source = rig
        .harness
        .get_all_by_label("None")
        .map(|n| n.rect().center())
        .filter(|p| (p.y - row.y).abs() < 6.0 && p.x > row.x)
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .expect("the new route's source");
    rig.click(source);
    rig.frames(2);
    rig.harness.get_by_label("Aftertouch").click();
    rig.frames(2);
    assert_eq!(
        param(&rig, track, wt::slot_param(5, wt::SLOT_SOURCE)) as usize,
        wt::SRC_AFTERTOUCH
    );
    // Its row stays, and the next empty slot offers the next route.
    rig.harness.get_by_label("7");
    rig.harness.get_by_label("+ Route");
}

/// Where to click for the destination menu of matrix slot `slot` (0-based)
/// while it is empty: the second "None" along its row.
fn empty_destination(rig: &Rig, slot: usize) -> egui::Pos2 {
    let number = lowest(rig, &format!("{}", slot + 1));
    let nones_right_of = |x: f32| {
        rig.harness
            .get_all_by_label("None")
            .map(|n| n.rect().center())
            .filter(move |p| (p.y - number.y).abs() < 6.0 && p.x > x)
            .min_by(|a, b| a.x.total_cmp(&b.x))
    };
    let source = nones_right_of(number.x).expect("the slot's source");
    nones_right_of(source.x).expect("the slot's destination")
}

#[test]
fn the_wavetable_tabs_sit_in_the_title_bar() {
    let mut rig = Rig::new("wavetable_tabs");
    wavetable(&mut rig);
    let presets = rig.harness.get_by_label("Presets").rect();
    let tab = rig.harness.get_by_label("Matrix").rect();
    assert!(
        (tab.center().y - presets.center().y).abs() < 4.0,
        "{tab:?} beside {presets:?}"
    );
    assert!(tab.left() > presets.right());
}

#[test]
fn the_new_knobs_reach_the_project() {
    let mut rig = Rig::wide("wavetable_knobs");
    let track = wavetable(&mut rig);
    // Osc A's stack menu, right of its name: an octave up.
    let stack = wt::osc_param(0, wt::OSC_STACK);
    let caption = nth(&rig, "Stack", 0);
    rig.click(caption.right_center() + vec2(space::M, 0.0));
    rig.frames(2);
    rig.harness.get_by_label("+12").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, stack), 1.0);

    // The amp envelope's hold, beside its editor.
    page(&mut rig, "Envelopes & LFOs");
    let hold = wt::env_param(0, wt::ENV_HOLD);
    let knob = rig
        .harness
        .get_all_by_label("Hold")
        .map(|n| n.rect().center())
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .unwrap();
    rig.drag(knob + vec2(0.0, 20.0), knob - vec2(0.0, 20.0));
    assert!(param(&rig, track, hold) > 0.0);
}

/// Magnitude at `hz` over `signal` (at 48 kHz), Hann-windowed (Goertzel).
fn magnitude(signal: &[f32], hz: f64) -> f64 {
    use std::f64::consts::TAU;
    let n = signal.len();
    let coeff = 2.0 * (TAU * hz / 48_000.0).cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for (i, x) in signal.iter().enumerate() {
        let window = 0.5 - 0.5 * (TAU * i as f64 / (n - 1) as f64).cos();
        let s = *x as f64 * window + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / n as f64 * 4.0
}

#[test]
fn drop_a_wavetable_on_osc_c() {
    let mut rig = Rig::wide("wavetable_user_table");
    let dir = super::temp_dir("wavetable_user_table");
    // Two frames of a sine three times over, as a 24-bit stereo WAV: its
    // first channel is the table (the second is ignored). Played, it
    // sounds an octave and a fifth above the note, with nothing at the
    // note itself.
    let frame: Vec<f32> = (0..2048)
        .map(|i| (std::f32::consts::TAU * 3.0 * i as f32 / 2048.0).sin() * 0.8)
        .collect();
    let left: Vec<f32> = frame.iter().chain(&frame).copied().collect();
    let right: Vec<f32> = (0..left.len())
        .map(|i| ((i * 7919) % 200) as f32 / 400.0)
        .collect();
    let path = dir.join("osc c.wav");
    let audio = media::AudioBuffer::new(44_100, left, right);
    media::write_wav_as(&path, &audio, media::WavFormat::Pcm24).unwrap();

    // The Wavetable with A and B off.
    let midi = rig.app().session.project.track_ids()[0];
    rig.click(rig.header_point(midi));
    rig.frames(2);
    let reference = instruments::InstrumentRef::builtin("canopy.wavetable");
    rig.app_mut()
        .session
        .edit("Change Instrument", |p| {
            p.set_instrument(midi, reference)?;
            for osc in [0, 1] {
                let on = wt::PARAMS[wt::osc_param(osc, wt::OSC_ON)].id;
                p.set_instrument_param(midi, on, 0.0)?;
            }
            Ok(())
        })
        .unwrap();
    rig.frames(2);
    let tracks = rig.app().session.project.track_ids().len();

    // Osc C's switch, beside A's and B's.
    rig.harness.get_by_label("Osc C").click();
    rig.frames(2);
    assert_eq!(param(&rig, midi, wt::osc_param(2, wt::OSC_ON)), 1.0);
    rig.harness
        .get_by_label_contains("Osc C wave: Basic Shapes");
    // Each oscillator's menu of its table's files and options.
    assert_eq!(rig.harness.get_all_by_label("…").count(), 3);
    rig.screenshot("osc c on");

    // The file dropped on C's display: C's own table, as one undo step,
    // rather than a new audio track.
    let id = egui::Id::new(("panel", Owner::Instrument(midi), ("wave", 2usize)));
    let rect: egui::Rect = rig
        .harness
        .ctx
        .data(|d| d.get_temp(id.with("rect")))
        .expect("osc C's display");
    rig.event(egui::Event::PointerMoved(rect.center()));
    rig.harness
        .input_mut()
        .dropped_files
        .push(Arc::new(super::TestFile(path.clone())));
    rig.frames(2);
    let track = rig.app().session.project.track(midi).unwrap();
    assert_eq!(track.wavetable_file(2), Some(path.as_path()));
    assert_eq!(track.wavetable_file(0), None);
    assert_eq!(
        param(&rig, midi, wt::osc_param(2, wt::OSC_TABLE)) as usize,
        wt::USER_TABLE
    );
    assert_eq!(rig.app().session.undo_label(), Some("Load Wavetable"));
    assert_eq!(rig.app().session.project.track_ids().len(), tracks);
    rig.wait_for("the table", |app| {
        matches!(
            app.session.wavetable(midi, 2),
            Some(crate::banks::TableState::Ready(_))
        )
    });
    rig.frames(3);
    // The display draws it, named.
    rig.harness
        .get_by_label_contains("Osc C wave: osc c.wav · 2 frames");
    rig.screenshot("loaded");

    // A3 (220 Hz) through C plays the table: 660 Hz, nothing at 220.
    let a3 = midi::NoteEventKind::On {
        pitch: Pitch::new(57).unwrap(),
        velocity: Velocity::MAX,
    };
    rig.app_mut().session.preview_note(midi, a3);
    rig.render_audio(4800);
    let out = rig.render_audio(24_000);
    let (fifth, note) = (magnitude(&out, 660.0), magnitude(&out, 220.0));
    println!("osc C's table at A3: {fifth:.4} at 660 Hz, {note:.5} at 220 Hz");
    assert!(fifth > 0.02 && fifth > 30.0 * note, "{fifth} {note}");
    rig.app_mut()
        .session
        .preview_note(midi, midi::NoteEventKind::AllNotesOff);

    // Saved with the project, and read back.
    let text = rig.app().session.project.to_file_string().unwrap();
    let loaded = project::Project::from_file_string(&text).unwrap();
    let saved = loaded.track(midi).unwrap();
    assert_eq!(saved.wavetable_file(2), Some(path.as_path()));

    // What Load… does once a file is picked, for osc A: it shares the
    // loaded file.
    rig.app_mut().session.load_wavetable(midi, 0, path.clone());
    rig.frames(2);
    assert_eq!(
        param(&rig, midi, wt::osc_param(0, wt::OSC_TABLE)) as usize,
        wt::USER_TABLE
    );
    rig.wait_for("A's table", |app| {
        matches!(
            app.session.wavetable(midi, 0),
            Some(crate::banks::TableState::Ready(_))
        )
    });

    // Undone, A goes back to its built-in table and loses the file.
    rig.app_mut().session.undo();
    rig.frames(2);
    let track = rig.app().session.project.track(midi).unwrap();
    assert_eq!(track.wavetable_file(0), None);
    assert_eq!(track.wavetable_file(2), Some(path.as_path()));
    assert_eq!(param(&rig, midi, wt::osc_param(0, wt::OSC_TABLE)), 0.0);

    // Removed (C's display's menu), "User" plays the first built-in table.
    rig.app_mut().session.remove_wavetable(midi, 2);
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Remove Wavetable"));
    assert!(
        rig.app()
            .session
            .project
            .track(midi)
            .unwrap()
            .wavetable_files()
            .is_empty()
    );
    rig.harness
        .get_by_label_contains("Osc C wave: User: no file, plays Basic Shapes");
    rig.screenshot("removed");
    let _ = std::fs::remove_dir_all(dir);
}

/// Plays `notes` on the track's synth (as keys held) and lets them sound.
fn hold(rig: &mut Rig, track: TrackId, notes: &[u8]) {
    for note in notes {
        let on = midi::NoteEventKind::On {
            pitch: Pitch::new(*note).unwrap(),
            velocity: Velocity::from_midi1(100),
        };
        rig.app_mut().session.preview_note(track, on);
    }
    for _ in 0..12 {
        rig.render_audio(800);
    }
}

/// The slot routing `source` to `dest`, if one does.
fn route(rig: &Rig, track: TrackId, source: usize, dest: usize) -> Option<usize> {
    (0..wt::MOD_SLOTS).find(|slot| {
        param(rig, track, wt::slot_param(*slot, wt::SLOT_SOURCE)) as usize == source
            && param(rig, track, wt::slot_param(*slot, wt::SLOT_DEST)) as usize == dest
    })
}

/// The `n`th (0-based, left to right) widget labelled `label`.
fn nth(rig: &Rig, label: &str, n: usize) -> egui::Rect {
    let mut rects: Vec<egui::Rect> = rig
        .harness
        .get_all_by_label(label)
        .map(|w| w.rect())
        .collect();
    rects.sort_by(|a, b| a.left().total_cmp(&b.left()));
    rects[n]
}

#[test]
fn drag_an_lfo_onto_a_knob_and_set_its_depth() {
    let mut rig = Rig::wide("wavetable_drag_modulation");
    let track = wavetable(&mut rig);
    hold(&mut rig, track, &[48, 60]);
    let dest = wt::osc_dest(1) + wt::DEST_POSITION;
    assert_eq!(route(&rig, track, wt::lfo_source(2), dest), None);

    // LFO 3's tile, onto Osc B's position knob.
    let tile = rig.harness.get_by_label("LFO 3").rect().center();
    let knob = nth(&rig, "Position", 1);
    let drop_at = knob.center();
    rig.event(egui::Event::PointerMoved(tile));
    rig.press(tile, true, Modifiers::NONE);
    for i in 1..=12 {
        let t = i as f32 / 12.0;
        rig.event(egui::Event::PointerMoved(tile + (drop_at - tile) * t));
    }
    rig.screenshot("dragging");
    rig.press(drop_at, false, Modifiers::NONE);
    rig.frames(2);
    let slot = route(&rig, track, wt::lfo_source(2), dest).expect("a slot routes LFO 3 there");
    let amount = wt::slot_param(slot, wt::SLOT_AMOUNT);
    assert_eq!(param(&rig, track, amount), 0.25);
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));
    // Dropped again, nothing doubles.
    for _ in 0..3 {
        rig.render_audio(800);
    }
    rig.screenshot("routed");

    // The ring, just outside the knob's dial: dragged up, it deepens; the
    // knob's own value stays.
    let position = param(&rig, track, wt::osc_param(1, wt::OSC_POSITION));
    let radius = 14.0;
    let center = egui::pos2(knob.center().x, knob.top() + 11.0 + 4.0 + radius);
    let ring = center - vec2(0.0, radius + 2.0);
    rig.drag(ring, ring - vec2(0.0, 44.0));
    let deeper = param(&rig, track, amount);
    assert!(deeper > 0.4, "{deeper}");
    assert_eq!(
        param(&rig, track, wt::osc_param(1, wt::OSC_POSITION)),
        position
    );
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));
    for _ in 0..6 {
        rig.render_audio(800);
    }
    rig.screenshot("ring dragged");

    // Its menu, from a right-click on the knob: bypass, then remove.
    rig.right_click(knob.center());
    rig.harness
        .get_by_label_contains(&format!("Mod {}: LFO 3", slot + 1))
        .click();
    rig.frames(2);
    rig.harness.get_by_label("Bypass").click();
    rig.frames(2);
    assert_eq!(
        param(&rig, track, wt::slot_param(slot, wt::SLOT_BYPASS)),
        1.0
    );
    assert_eq!(rig.app().session.undo_label(), Some("Change Modulation"));
    rig.right_click(knob.center());
    rig.harness
        .get_by_label_contains(&format!("Mod {}: LFO 3", slot + 1))
        .click();
    rig.frames(2);
    rig.harness.get_by_label("Remove").click();
    rig.frames(2);
    assert_eq!(route(&rig, track, wt::lfo_source(2), dest), None);
}

/// Drags a modulation source's tile from `from` and drops it at `to`.
fn drop_source(rig: &mut Rig, from: egui::Pos2, to: egui::Pos2) {
    rig.event(egui::Event::PointerMoved(from));
    rig.press(from, true, Modifiers::NONE);
    for i in 1..=12 {
        let t = i as f32 / 12.0;
        rig.event(egui::Event::PointerMoved(from + (to - from) * t));
    }
    rig.press(to, false, Modifiers::NONE);
    rig.frames(2);
}

#[test]
fn modulate_envelope_times_lfo_settings_and_the_volume() {
    let mut rig = Rig::wide("wavetable_more_destinations");
    let track = wavetable(&mut rig);
    page(&mut rig, "Envelopes & LFOs");
    hold(&mut rig, track, &[48, 60]);

    // Velocity onto the amp envelope's decay: harder notes ring longer.
    let velocity = rig.harness.get_by_label("Vel").rect().center();
    let decay = rig.harness.get_by_label("Decay").rect().center();
    drop_source(&mut rig, velocity, decay);
    let amp_decay = wt::env_dest(0, wt::ENV_DEST_DECAY);
    route(&rig, track, wt::SRC_VELOCITY, amp_decay).expect("velocity moves the decay");

    // The per-note random onto LFO 1's phase, and Macro 1 onto its rise.
    let random = rig.harness.get_by_label("Rnd 1").rect().center();
    let phase = rig.harness.get_by_label("Phase").rect().center();
    drop_source(&mut rig, random, phase);
    let lfo_phase = wt::lfo_dest(0, wt::LFO_DEST_PHASE);
    route(&rig, track, wt::SRC_RANDOM, lfo_phase).expect("the random moves the phase");
    let macro1 = rig.harness.get_by_label("M1").rect().center();
    let rise = rig.harness.get_by_label("Rise").rect().center();
    drop_source(&mut rig, macro1, rise);
    let lfo_rise = wt::lfo_dest(0, wt::LFO_DEST_RISE);
    route(&rig, track, wt::macro_source(0), lfo_rise).expect("macro 1 moves the rise");

    // The mod wheel onto the volume, at the right of every page.
    let wheel = rig.harness.get_by_label("Wheel").rect().center();
    // (The lowest "Volume": the track's own is higher up.)
    let volume = lowest(&rig, "Volume");
    drop_source(&mut rig, wheel, volume);
    let slot =
        route(&rig, track, wt::SRC_MOD_WHEEL, wt::DEST_VOLUME).expect("the wheel moves the volume");
    assert_eq!(
        param(&rig, track, wt::slot_param(slot, wt::SLOT_AMOUNT)),
        0.25
    );
    // The knob's menu lists the slot.
    rig.right_click(volume);
    rig.harness
        .get_by_label_contains(&format!("Mod {}: Mod Wheel", slot + 1));
    rig.key(Modifiers::NONE, egui::Key::Escape);
    rig.frames(2);

    hold(&mut rig, track, &[48, 60]);
    rig.screenshot("routed");
}

#[test]
fn edit_an_lfos_nodes() {
    let mut rig = Rig::wide("wavetable_nodes");
    let track = wavetable(&mut rig);
    page(&mut rig, "Envelopes & LFOs");
    // LFO 5 starts on the Points shape: a triangle of three nodes. Its tab
    // is under its tile in the strip.
    let tab = lowest(&rig, "LFO 5");
    rig.click(tab);
    hold(&mut rig, track, &[57]);
    rig.screenshot("lfo 5");
    let p = |offset: usize| wt::lfo_param(4, offset);
    assert_eq!(
        param(&rig, track, p(wt::LFO_SHAPE)) as usize,
        wt::SHAPE_NODES
    );
    let id = egui::Id::new(("panel", Owner::Instrument(track), ("lfo nodes", 4usize)));
    let rect: egui::Rect = rig
        .harness
        .ctx
        .data(|d| d.get_temp(id.with("rect")))
        .expect("the node editor is shown");
    let area = rect.shrink(6.0);
    let at = |x: f32, y: f32| {
        egui::pos2(
            area.left() + x * area.width(),
            area.bottom() - y * area.height(),
        )
    };

    // The peak dragged down and left lands on the grid: a quarter in, three
    // quarters up.
    rig.drag(at(0.5, 1.0), at(0.26, 0.73));
    assert_eq!(param(&rig, track, p(wt::LFO_NODE_X + 1)), 0.25);
    assert_eq!(param(&rig, track, p(wt::LFO_NODE_Y + 1)), 0.75);
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));

    // A double-click on empty space adds a node there.
    rig.double_click(at(0.75, 0.25));
    assert_eq!(param(&rig, track, p(wt::LFO_NODES)) as usize + 2, 4);
    assert_eq!(param(&rig, track, p(wt::LFO_NODE_X + 2)), 0.75);

    // Painting steps across the grid's eight cells, near the top: they
    // snap to it.
    rig.harness.get_by_label("Steps").click();
    rig.frames(2);
    rig.drag(at(0.0, 0.9), at(0.95, 0.9));
    let count = param(&rig, track, p(wt::LFO_NODES)) as usize + 2;
    assert_eq!(count, 16, "eight steps of two nodes");
    for node in 0..count {
        assert_eq!(
            param(&rig, track, p(wt::LFO_NODE_Y + node)),
            1.0,
            "node {node}"
        );
    }
    for _ in 0..6 {
        rig.render_audio(800);
    }
    rig.screenshot("painted");
}

#[test]
fn morph_and_filter_menus_and_the_envelopes() {
    let mut rig = Rig::wide("wavetable_morph");
    let track = wavetable(&mut rig);
    hold(&mut rig, track, &[45, 57]);
    // Osc A's morph menu: Harmonic Stretch.
    let morph = nth(&rig, "No Morph", 0).center();
    rig.click(morph);
    rig.frames(2);
    rig.harness.get_by_label("Harmonic Stretch").click();
    rig.frames(2);
    assert_eq!(
        param(&rig, track, wt::osc_param(0, wt::OSC_MORPH_MODE)) as usize,
        wt::MORPH_HARMONIC_STRETCH
    );
    // Its amount, dragged up.
    let knob = nth(&rig, "Morph", 0).center();
    rig.drag(knob + vec2(0.0, 20.0), knob - vec2(0.0, 40.0));
    assert!(param(&rig, track, wt::osc_param(0, wt::OSC_MORPH)) > 0.1);

    // The filter's type menu, by family: a phaser, whose blend spreads it.
    rig.harness.get_by_label("LP 24").click();
    rig.frames(2);
    rig.harness.get_by_label("Effects");
    rig.screenshot("filter types");
    rig.harness.get_by_label("Phaser").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, wt::FILTER_TYPE) as usize, wt::PHASER);
    rig.harness.get_by_label("Spread");
    for _ in 0..6 {
        rig.render_audio(800);
    }
    rig.screenshot("morph and phaser");

    // Spectrum view of osc A.
    let id = egui::Id::new(("panel", Owner::Instrument(track), ("wave", 0usize)));
    let wave: egui::Rect = rig
        .harness
        .ctx
        .data(|d| d.get_temp(id.with("rect")))
        .unwrap();
    let sp = egui::pos2(wave.left() + 34.0, wave.bottom() - 8.0);
    rig.click(sp);
    rig.screenshot("spectrum");

    // Env 4, mono.
    page(&mut rig, "Envelopes & LFOs");
    let tab = lowest(&rig, "Env 4");
    rig.click(tab);
    // The envelope's Mono, left of the LFO's.
    let mono = nth(&rig, "Mono", 0).center();
    rig.click(mono);
    rig.frames(2);
    assert_eq!(param(&rig, track, wt::env_mono_param(3)), 1.0);
    rig.screenshot("env 4");
}

#[test]
fn the_voicing_page_and_the_browser() {
    let mut rig = Rig::wide("wavetable_voicing");
    let track = wavetable(&mut rig);
    page(&mut rig, "Voicing");
    rig.harness.get_by_label("High").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, wt::QUALITY), 1.0);
    rig.harness.get_by_label("Scaled").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, wt::GLIDE_SCALED), 1.0);
    hold(&mut rig, track, &[50]);
    rig.screenshot("voicing");

    // The FX page: the track's effects (none yet).
    page(&mut rig, "FX");
    rig.harness.get_by_label("No effects on this track.");
    rig.screenshot("fx");

    // The browser: a factory preset loads as one undo step.
    let dir = super::temp_dir("wavetable_browser");
    rig.app_mut().session.presets_dir = Some(dir.clone());
    page(&mut rig, "Browser");
    rig.screenshot("browser");
    let factory = rig
        .app()
        .session
        .factory_presets(crate::session::PresetTarget::Instrument(track));
    let (name, values) = factory
        .iter()
        .find(|(n, _)| n.starts_with("Bass: "))
        .expect("a bass preset");
    let short = name.trim_start_matches("Bass: ");
    rig.harness
        .get_by_label_contains(&format!("{short} · Bass"))
        .click();
    rig.frames(2);
    assert_eq!(rig.app().session.undo_label(), Some("Load Preset"));
    for (id, value) in values {
        let index = wt::PARAMS.iter().position(|p| p.id == id).unwrap();
        assert_eq!(param(&rig, track, index), *value, "{id}");
    }
    // A category, then the search.
    rig.harness.get_by_label("Pad").click();
    rig.frames(2);
    rig.screenshot("pads");

    // Saved under a name, it lists as the user's own.
    let label = rig.harness.get_by_label("Save as").rect();
    rig.click(egui::pos2(label.right() + 60.0, label.center().y));
    rig.harness.event(egui::Event::Text("My Bass".to_owned()));
    rig.frames(2);
    rig.harness.get_by_label("Save").click();
    rig.frames(2);
    assert!(dir.join("canopy.wavetable").join("My Bass.ron").exists());
    rig.harness.get_by_label("User").click();
    rig.frames(2);
    rig.harness.get_by_label("My Bass");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn edit_and_save_a_wavetable() {
    let mut rig = Rig::wide("wavetable_editor");
    let track = wavetable(&mut rig);
    // Osc A's menu (the leftmost), then Edit… in it.
    rig.click(nth(&rig, "…", 0).center());
    rig.frames(2);
    rig.harness.get_by_label("Edit…").click();
    rig.frames(3);
    rig.screenshot("editor");
    // A formula over the frames, then the frames' count.
    rig.harness.get_by_label("All Frames").click();
    rig.frames(2);
    rig.screenshot("formula");
    // What Save & Use does once the file is written (as the editor's own
    // tests check it writes): the oscillator plays it.
    let dir = super::temp_dir("wavetable_editor");
    let path = dir.join("drawn.wav");
    let samples: Vec<f32> = (0..4 * 2048)
        .map(|i| ((i % 2048) as f32 / 2048.0 * (i / 2048 + 1) as f32 * std::f32::consts::TAU).sin())
        .collect();
    let audio = media::AudioBuffer::new(48_000, samples.clone(), samples);
    media::write_wav(&path, &audio).unwrap();
    rig.app_mut().session.load_wavetable(track, 0, path.clone());
    rig.wait_for("the table", |app| {
        matches!(
            app.session.wavetable(track, 0),
            Some(crate::banks::TableState::Ready(_))
        )
    });
    rig.frames(3);
    rig.harness
        .get_by_label_contains("Osc A wave: drawn.wav · 4 frames");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn remap_an_oscillators_cycle() {
    let mut rig = Rig::wide("wavetable_remap");
    let track = wavetable(&mut rig);
    hold(&mut rig, track, &[45, 57]);
    let p = |offset: usize| wt::osc_param(0, offset);
    // Osc A's first warp menu (the leftmost showing its warp): Remap.
    let warp = wt::WARPS_A[param(&rig, track, p(wt::OSC_WARP_MODE)) as usize];
    rig.click(nth(&rig, warp, 0).center());
    rig.frames(2);
    rig.harness.get_by_label("Remap").click();
    rig.frames(2);
    assert_eq!(
        param(&rig, track, p(wt::OSC_WARP_MODE)) as usize,
        wt::WARP_REMAP
    );
    // Its amount up, so the curve is heard.
    let knob = nth(&rig, "Warp", 0).center();
    rig.drag(knob + vec2(0.0, 20.0), knob - vec2(0.0, 60.0));
    let amount = param(&rig, track, p(wt::OSC_WARP));
    assert!(amount > 0.3, "{amount}");

    // The curve, opened beside the warp menus: the default S-curve's
    // middle node dragged up and left lands on the grid.
    rig.harness.get_by_label("Curve").click();
    rig.frames(2);
    let id = egui::Id::new(("panel", Owner::Instrument(track), ("remap", 0usize)));
    let rect: egui::Rect = rig
        .harness
        .ctx
        .data(|d| d.get_temp(id.with("rect")))
        .expect("the remap editor is shown");
    let area = rect.shrink(6.0);
    let at = |x: f32, y: f32| {
        egui::pos2(
            area.left() + x * area.width(),
            area.bottom() - y * area.height(),
        )
    };
    rig.drag(at(0.5, 0.5), at(0.26, 0.74));
    assert_eq!(param(&rig, track, p(wt::OSC_REMAP_X + 1)), 0.25);
    assert_eq!(param(&rig, track, p(wt::OSC_REMAP_Y + 1)), 0.75);
    assert_eq!(rig.app().session.undo_label(), Some("Change Parameter"));
    for _ in 0..6 {
        rig.render_audio(800);
    }
    rig.screenshot("remap curve");

    // A ready-made curve from its Shapes menu, under the editor: the cycle
    // twice over. (The tables named "… Shapes" are elsewhere.)
    let shapes = rig
        .harness
        .get_all_by_label_contains("Shapes")
        .map(|n| n.rect().center())
        .min_by(|a, b| {
            let off = |p: &egui::Pos2| p.distance(rect.center_bottom());
            off(a).total_cmp(&off(b))
        })
        .unwrap();
    rig.click(shapes);
    rig.frames(2);
    rig.screenshot("remap shapes");
    rig.harness.get_by_label("Twice (Octave Up)").click();
    rig.frames(2);
    assert_eq!(param(&rig, track, p(wt::OSC_REMAP_NODES)) as usize + 2, 4);
    assert_eq!(param(&rig, track, p(wt::OSC_REMAP_Y + 1)), 1.0);
    assert_eq!(param(&rig, track, p(wt::OSC_REMAP_X + 2)), 0.5);
    for _ in 0..6 {
        rig.render_audio(800);
    }
    rig.screenshot("remapped twice");
}

/// How a factory preset sounds over its first two seconds: a held note's
/// and a chord's loudness (the loudest 100 ms, as RMS in dBFS, so plucks
/// and drums count as loud as they sound), the peak, the note's DC offset,
/// and how much louder one side is than the other.
#[derive(Debug)]
struct PresetSound {
    note_rms: f32,
    chord_rms: f32,
    peak: f32,
    dc: f32,
    side_db: f32,
}

fn preset_sound(values: &std::collections::BTreeMap<String, f32>) -> PresetSound {
    use instruments::Instrument;
    use midi::{NoteEvent, NoteEventKind};
    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 256;
    let play = |notes: &[u8]| -> (Vec<f32>, Vec<f32>) {
        let mut synth = wt::Wavetable::new();
        synth.prepare(RATE, BLOCK);
        for (id, value) in values {
            let index = wt::PARAMS.iter().position(|p| p.id == id).unwrap();
            synth.set_param(index, *value);
        }
        synth.reset();
        let events: Vec<NoteEvent> = notes
            .iter()
            .map(|&n| NoteEvent {
                frame: 0,
                kind: NoteEventKind::On {
                    pitch: Pitch::new(n).unwrap(),
                    velocity: Velocity::from_midi1(100),
                },
            })
            .collect();
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for block in 0..(2.0 * RATE) as usize / BLOCK {
            synth.process(if block == 0 { &events } else { &[] }, &mut l, &mut r);
            left.extend_from_slice(&l);
            right.extend_from_slice(&r);
        }
        (left, right)
    };
    let db = |x: f32| 20.0 * x.max(1e-9).log10();
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    // The loudest 100 ms of both sides, stepping 50 ms.
    let loudest = |l: &[f32], r: &[f32]| {
        let window = (RATE / 10.0) as usize;
        (0..l.len().saturating_sub(window))
            .step_by(window / 2)
            .map(|i| {
                let (wl, wr) = (&l[i..i + window], &r[i..i + window]);
                ((rms(wl).powi(2) + rms(wr).powi(2)) / 2.0).sqrt()
            })
            .fold(0.0f32, f32::max)
    };
    let (l, r) = play(&[48]);
    let (cl, cr) = play(&[48, 55, 60, 64]);
    let peak = l
        .iter()
        .chain(&r)
        .chain(&cl)
        .chain(&cr)
        .fold(0.0f32, |m, x| m.max(x.abs()));
    PresetSound {
        note_rms: db(loudest(&l, &r)),
        chord_rms: db(loudest(&cl, &cr)),
        peak: db(peak),
        dc: (l.iter().chain(&r).sum::<f32>() / (2 * l.len()) as f32).abs(),
        side_db: (db(rms(&l)) - db(rms(&r))).abs(),
    }
}

/// Every Wavetable factory preset is in a sensible window of loudness,
/// leaves headroom, carries no DC and sits near the middle of the stereo
/// field, so stepping through the browser never jumps out of the mix.
#[test]
fn the_factory_presets_sit_together() {
    let presets = crate::presets::factory("canopy.wavetable");
    assert!(presets.len() >= 60, "{} presets", presets.len());
    let mut problems = Vec::new();
    for (name, values) in &presets {
        let sound = preset_sound(values);
        println!(
            "{name:32} note {:6.1} dB  chord {:6.1} dB  peak {:6.1} dB  dc {:.4}  side {:.1} dB",
            sound.note_rms, sound.chord_rms, sound.peak, sound.dc, sound.side_db
        );
        if !(-24.0..=-8.0).contains(&sound.note_rms) {
            problems.push(format!("{name}: note at {:.1} dB", sound.note_rms));
        }
        if sound.chord_rms > -5.0 {
            problems.push(format!("{name}: chord at {:.1} dB", sound.chord_rms));
        }
        if sound.peak > 1.5 {
            problems.push(format!("{name}: peaks at {:.1} dB", sound.peak));
        }
        // Asymmetric warps (Asym, Squeeze, Drive) leave some DC, as in
        // Serum; a lot would thump at every note.
        if sound.dc > 0.02 {
            problems.push(format!("{name}: DC {:.4}", sound.dc));
        }
        if sound.side_db > 3.0 {
            problems.push(format!("{name}: one side {:.1} dB louder", sound.side_db));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
