//! End-to-end tests: the real app, driven through simulated mouse and
//! keyboard input, with an engine the test renders by hand.
//!
//! Set `CANOPY_SCREENSHOTS=<dir>` to also save what each step looks like.

mod audio_tools;
mod automation;
mod clip_features;
mod device_panels;
mod drum_features;
mod effects;
mod export;
mod flp_import;
mod instrument_widths;
mod midi_input;
mod perf;
mod piano_tools;
mod plugins;
mod polish;
mod quality_of_life;
mod regressions;
mod slicer;
mod tour;
mod wavetable_features;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, DroppedFile, Event, Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use engine::Engine;
use project::{AutomationTarget, ClipRef, TrackKind};

use crate::app::CanopyApp;
use crate::session::Session;
use crate::timeline::HEADER_WIDTH;

const RATE: f32 = 48_000.0;
/// At 120 BPM.
const SAMPLES_PER_BAR: usize = 96_000;

pub(super) struct Rig {
    pub(super) harness: Harness<'static, CanopyApp>,
    engine: Engine,
    name: &'static str,
    shots: usize,
}

/// kittest's software-Vulkan setup without wgpu's debug object names and
/// validation. Debug builds name every buffer through the Vulkan loader,
/// which isn't safe with several devices at once: tests running in parallel
/// crashed in `vkSetDebugUtilsObjectNameEXT`, more often the more textures
/// (font sizes) a test uploaded.
fn quiet_wgpu() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::egui_wgpu::{WgpuSetup, wgpu::InstanceFlags};
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let WgpuSetup::CreateNew(new) = &mut setup {
        new.instance_descriptor
            .flags
            .remove(InstanceFlags::DEBUG | InstanceFlags::VALIDATION);
    }
    setup
}

impl Rig {
    pub(super) fn new(name: &'static str) -> Rig {
        Rig::with_size(name, vec2(1280.0, 800.0))
    }

    /// A window wide enough for a whole chain of effects.
    pub(super) fn wide(name: &'static str) -> Rig {
        Rig::with_size(name, vec2(2600.0, 800.0))
    }

    pub(super) fn with_size(name: &'static str, size: egui::Vec2) -> Rig {
        let (handle, engine) = engine::create(RATE);
        let session = Session::with_engine(handle, || {});
        let harness = Harness::builder()
            .with_size(size)
            .with_step_dt(1.0 / 60.0)
            // Room for animations to finish when a test waits for the
            // window to go idle (`Rig::settle`).
            .with_max_steps(60)
            .wgpu_setup(quiet_wgpu())
            .build_eframe(move |_cc| CanopyApp::with_session(session));
        let mut rig = Rig {
            harness,
            engine,
            name,
            shots: 0,
        };
        rig.frames(3);
        rig
    }

    pub(super) fn app(&self) -> &CanopyApp {
        self.harness.state()
    }

    pub(super) fn app_mut(&mut self) -> &mut CanopyApp {
        self.harness.state_mut()
    }

    pub(super) fn frames(&mut self, n: usize) {
        self.harness.run_steps(n);
    }

    /// Runs frames until the window stops asking for them (animations
    /// done), and returns how many that took. Panics if it never does:
    /// an idle window mustn't keep repainting.
    pub(super) fn settle(&mut self) -> u64 {
        match self.harness.try_run() {
            Ok(steps) => steps,
            Err(err) => panic!("{}: the window kept repainting: {err}", self.name),
        }
    }

    /// How long the window asked to wait before its next frame, as of the
    /// last one: `Duration::MAX` when it asked for none at all, so it
    /// sleeps until something happens.
    pub(super) fn repaint_delay(&self) -> std::time::Duration {
        self.harness
            .output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(std::time::Duration::MAX, |v| v.repaint_delay)
    }

    pub(super) fn event(&mut self, event: Event) {
        self.harness.event(event);
        self.harness.step();
    }

    pub(super) fn press(&mut self, pos: Pos2, pressed: bool, modifiers: Modifiers) {
        self.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        });
    }

    pub(super) fn click_with(&mut self, pos: Pos2, modifiers: Modifiers) {
        self.event(Event::ModifiersChanged(modifiers));
        self.event(Event::PointerMoved(pos));
        self.press(pos, true, modifiers);
        self.press(pos, false, modifiers);
        self.event(Event::ModifiersChanged(Modifiers::NONE));
        self.frames(1);
    }

    pub(super) fn click(&mut self, pos: Pos2) {
        self.click_with(pos, Modifiers::NONE);
    }

    pub(super) fn double_click(&mut self, pos: Pos2) {
        // egui reads a click within 0.6 s of the one before last as a
        // triple-click, wherever it was, so let earlier clicks age first.
        self.frames(45);
        self.event(Event::PointerMoved(pos));
        for _ in 0..2 {
            self.press(pos, true, Modifiers::NONE);
            self.press(pos, false, Modifiers::NONE);
        }
        self.frames(2);
    }

    /// A drag with whatever keys are already held.
    pub(super) fn drag(&mut self, from: Pos2, to: Pos2) {
        self.event(Event::PointerMoved(from));
        self.press(from, true, Modifiers::NONE);
        for i in 1..=8 {
            let t = i as f32 / 8.0;
            self.event(Event::PointerMoved(from + (to - from) * t));
        }
        self.press(to, false, Modifiers::NONE);
        self.frames(2);
    }

    /// A drag with `modifiers` held from start to end, like Shift+drag.
    pub(super) fn drag_with(&mut self, from: Pos2, to: Pos2, modifiers: Modifiers) {
        self.event(Event::ModifiersChanged(modifiers));
        self.drag(from, to);
        self.event(Event::ModifiersChanged(Modifiers::NONE));
    }

    pub(super) fn right_click(&mut self, pos: Pos2) {
        self.event(Event::PointerMoved(pos));
        for pressed in [true, false] {
            self.event(Event::PointerButton {
                pos,
                button: PointerButton::Secondary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        self.frames(2);
    }

    pub(super) fn key(&mut self, modifiers: Modifiers, key: Key) {
        self.harness.key_press_modifiers(modifiers, key);
        self.frames(2);
    }

    /// Renders `frames` of audio and returns the left channel.
    pub(super) fn render_audio(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for chunk in out.chunks_mut(512 * 2) {
            self.engine.process_interleaved(chunk, 2);
        }
        self.frames(1);
        out.chunks(2).map(|f| f[0]).collect()
    }

    /// Saves a screenshot if `CANOPY_SCREENSHOTS` is set.
    pub(super) fn screenshot(&mut self, label: &str) {
        let Some(dir) = std::env::var_os("CANOPY_SCREENSHOTS") else {
            return;
        };
        self.shots += 1;
        let image = self.harness.render().expect("render");
        let path = Path::new(&dir).join(format!("{}_{:02}_{label}.png", self.name, self.shots));
        image.save(&path).expect("save screenshot");
    }

    /// Steps until `done` holds, for work that finishes on another thread.
    pub(super) fn wait_for(&mut self, what: &str, done: impl Fn(&CanopyApp) -> bool) {
        let start = Instant::now();
        while !done(self.app()) {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
            self.frames(1);
        }
    }

    /// A point in a track's lane, `bars` bars from the start (at the default zoom).
    pub(super) fn lane_point(&self, track: project::TrackId, bars: f32) -> Pos2 {
        let row = self
            .app()
            .timeline
            .row_rect(track)
            .expect("track is on screen");
        let viewport = self.app().timeline.viewport;
        pos2(viewport.x(bars as f64 * 3840.0), row.center().y)
    }

    pub(super) fn header_point(&self, track: project::TrackId) -> Pos2 {
        let row = self
            .app()
            .timeline
            .row_rect(track)
            .expect("track is on screen");
        pos2(row.left() + HEADER_WIDTH * 0.6, row.top() + 8.0)
    }

    /// A MIDI clip's notes as `(start, pitch)`.
    pub(super) fn notes(&self, clip: project::ClipId) -> Vec<(i64, u8)> {
        match self.app().session.project.clip(clip).unwrap().1 {
            ClipRef::Midi(m) => m
                .notes
                .iter()
                .map(|n| (n.start.get(), n.pitch.get()))
                .collect(),
            _ => unreachable!(),
        }
    }
}

pub(super) fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn draw_a_clip_add_notes_move_it_and_hear_it() {
    let mut rig = Rig::new("midi");
    rig.screenshot("start");
    let track = rig.app().session.project.tracks[0].id;

    // Double-clicking an empty MIDI lane makes a one-bar clip there and
    // opens it in the piano roll.
    rig.double_click(rig.lane_point(track, 1.1));
    let clip = {
        let project = &rig.app().session.project;
        let clips = project.track(track).unwrap().clips();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].start().get(), 3840, "snapped to bar 2");
        clips[0].id()
    };
    assert!(rig.app().piano_roll_shown());
    assert_eq!(rig.app().timeline.selection.open_clip, Some(clip));
    rig.frames(2);

    // Double-click in the piano roll adds notes.
    for (beat, pitch) in [(0.0, 60), (1.0, 64), (2.0, 67)] {
        let point = rig
            .app()
            .piano_roll
            .point(3840.0 + beat * 960.0 + 30.0, pitch);
        rig.double_click(point);
    }
    let pitches: Vec<u8> = match rig.app().session.project.clip(clip).unwrap().1 {
        ClipRef::Midi(midi) => midi.notes.iter().map(|n| n.pitch.get()).collect(),
        _ => unreachable!(),
    };
    assert_eq!(pitches, [60, 64, 67]);
    rig.screenshot("notes");

    // Drag the clip two bars right.
    let from = rig.lane_point(track, 1.5);
    rig.drag(from, from + vec2(240.0, 0.0));
    let start = rig.app().session.project.clip(clip).unwrap().1.start();
    assert_eq!(start.get(), 3 * 3840, "moved to bar 4");
    rig.screenshot("moved");

    // Undo puts it back, redo moves it again.
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(
        rig.app()
            .session
            .project
            .clip(clip)
            .unwrap()
            .1
            .start()
            .get(),
        3840
    );
    rig.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    assert_eq!(
        rig.app()
            .session
            .project
            .clip(clip)
            .unwrap()
            .1
            .start()
            .get(),
        3 * 3840
    );

    // Space plays: silence for three bars, then the chord. (Clicking a lane
    // moved the playhead; Home puts it back at the start.)
    rig.click(pos2(640.0, 400.0)); // focus the timeline
    rig.key(Modifiers::NONE, Key::Home);
    rig.key(Modifiers::NONE, Key::Space);
    assert!(rig.app().session.is_playing());
    let before = rig.render_audio(3 * SAMPLES_PER_BAR);
    assert_eq!(peak(&before), 0.0);
    let chord = rig.render_audio(SAMPLES_PER_BAR / 4);
    assert!(peak(&chord) > 0.05, "the notes play");
    rig.screenshot("playing");
    rig.key(Modifiers::NONE, Key::Space);
    assert!(!rig.app().session.is_playing());
}

#[derive(Debug)]
pub(super) struct TestFile(pub PathBuf);

impl DroppedFile for TestFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// A second of a 220 Hz sine as a 16-bit mono WAV.
fn write_wav(path: &Path) {
    write_sine_wav(path, 1, 220.0);
}

/// `seconds` of a sine at `freq` as a 16-bit mono WAV.
pub(super) fn write_sine_wav(path: &Path, seconds: u32, freq: f32) {
    let rate = 44_100u32;
    let samples: Vec<i16> = (0..rate * seconds)
        .map(|i| ((i as f32 * freq * std::f32::consts::TAU / rate as f32).sin() * 20_000.0) as i16)
        .collect();
    let data_len = samples.len() as u32 * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

pub(super) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("canopy-ui-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn drop_audio_group_tracks_and_save() {
    let mut rig = Rig::new("audio");
    let dir = temp_dir("audio");
    let wav = dir.join("sine.wav");
    write_wav(&wav);
    let tracks = rig.app().session.project.track_ids();
    let audio_track = tracks[2];

    // Drop a file onto the first audio track, at bar 2.
    let target = rig.lane_point(audio_track, 1.0);
    rig.event(Event::PointerMoved(target));
    rig.harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(TestFile(wav.clone())));
    rig.frames(1);
    rig.wait_for("the import", |app| {
        !app.session
            .project
            .track(audio_track)
            .unwrap()
            .clips()
            .is_empty()
            && app
                .session
                .source_state(app.session.project.sources[0].id)
                .is_some()
    });
    let (start, frames) = match rig
        .app()
        .session
        .project
        .track(audio_track)
        .unwrap()
        .clips()[0]
    {
        ClipRef::Audio(clip) => (clip.start.get(), clip.source_len),
        _ => unreachable!(),
    };
    assert_eq!((start, frames), (3840, 44_100));
    rig.frames(2);
    rig.screenshot("dropped");

    // It plays at bar 2.
    rig.key(Modifiers::NONE, Key::Space);
    let before = rig.render_audio(SAMPLES_PER_BAR);
    assert_eq!(peak(&before), 0.0);
    let audio = rig.render_audio(SAMPLES_PER_BAR / 4);
    assert!(peak(&audio) > 0.3, "the clip plays: {}", peak(&audio));
    rig.key(Modifiers::NONE, Key::Space);

    // Select both audio tracks and group them.
    rig.click(rig.header_point(tracks[2]));
    rig.click_with(rig.header_point(tracks[3]), Modifiers::COMMAND);
    assert_eq!(rig.app().timeline.selection.tracks, [tracks[2], tracks[3]]);
    rig.key(Modifiers::COMMAND, Key::G);
    let project = &rig.app().session.project;
    assert_eq!(project.tracks.len(), 3);
    let group = project.tracks[2].id;
    assert!(project.tracks[2].is_group());
    assert_eq!(project.parent(tracks[2]), Some(group));
    assert_eq!(project.parent(tracks[3]), Some(group));
    rig.screenshot("grouped");

    // Muting the group silences its children.
    let mut mixer = rig.app().session.project.track(group).unwrap().mixer;
    mixer.mute = true;
    rig.app_mut()
        .session
        .edit("Mute", |p| p.set_mixer(group, mixer));
    rig.frames(2);
    rig.app_mut().session.seek(midi::Tick::ZERO);
    rig.key(Modifiers::NONE, Key::Space);
    let muted = rig.render_audio(SAMPLES_PER_BAR + SAMPLES_PER_BAR / 4);
    assert_eq!(
        peak(&muted[SAMPLES_PER_BAR + 2000..]),
        0.0,
        "group mute works"
    );
    rig.key(Modifiers::NONE, Key::Space);

    // Fold it from its header arrow.
    let row = rig.app().timeline.row_rect(group).unwrap();
    rig.click(pos2(row.left() + 16.0, row.top() + 12.0));
    assert!(rig.app().session.project.track(group).unwrap().is_folded());
    rig.screenshot("folded");

    // Save, start over, and open it again.
    let file = dir.join("song.canopy");
    rig.app_mut().session.save_as(&file);
    assert!(!rig.app().session.is_dirty());
    let saved = rig.app().session.project.clone();
    rig.app_mut().session.new_project();
    rig.frames(2);
    assert_ne!(rig.app().session.project, saved);
    rig.app_mut().session.open(&file);
    rig.wait_for("the reload", |app| {
        matches!(
            app.session.source_state(app.session.project.sources[0].id),
            Some(crate::session::SourceState::Ready(_))
        )
    });
    assert_eq!(rig.app().session.project, saved);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn automation_lane_points() {
    let mut rig = Rig::new("automation");
    let track = rig.app().session.project.tracks[0].id;

    // The "A" button in the header shows the volume lane.
    rig.harness
        .get_all_by_label("A")
        .next()
        .expect("an A button")
        .click();
    rig.frames(2);
    assert_eq!(
        rig.app().timeline.automation.get(&track).map(Vec::as_slice),
        Some(&[AutomationTarget::Volume][..])
    );
    rig.frames(1);
    let lane = rig
        .app()
        .timeline
        .automation_rect(track)
        .expect("lane is shown");

    let viewport = rig.app().timeline.viewport;
    for (bar, height) in [(0.0, 0.9), (2.0, 0.1), (4.0, 0.9)] {
        let x = viewport.x(bar * 3840.0);
        rig.double_click(pos2(x + 1.0, lane.bottom() - lane.height() * height));
    }
    let points = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .automation_lane(&AutomationTarget::Volume)
        .map(|l| l.points.to_vec())
        .unwrap_or_default();
    let ticks: Vec<i64> = points.iter().map(|p| p.tick.get()).collect();
    assert_eq!(ticks, [0, 7680, 15360]);
    assert!(points[1].value < 0.3 && points[0].value > 0.7);
    rig.screenshot("automation");

    // Dragging the middle point up raises it.
    let x = viewport.x(2.0 * 3840.0);
    let from = pos2(x, lane.bottom() - lane.height() * 0.1);
    rig.drag(from, from - vec2(0.0, lane.height() * 0.4));
    let value = rig
        .app()
        .session
        .project
        .track(track)
        .unwrap()
        .automation_lane(&AutomationTarget::Volume)
        .unwrap()
        .points[1]
        .value;
    assert!(value > 0.35, "dragged up to {value}");

    // Switching the track's instrument from the device view.
    let TrackKind::Midi { instrument, .. } = &rig.app().session.project.tracks[0].kind else {
        unreachable!()
    };
    assert_eq!(instrument.id, instruments::DEFAULT_INSTRUMENT);
    let _ = egui::Id::NULL;
}

#[test]
fn trim_split_delete_reorder_and_fader_undo() {
    let mut rig = Rig::new("edit");
    let tracks = rig.app().session.project.track_ids();
    let midi = tracks[0];
    rig.double_click(rig.lane_point(midi, 0.1));
    let clip = rig.app().session.project.track(midi).unwrap().clips()[0].id();
    let bounds = |rig: &Rig| {
        let project = &rig.app().session.project;
        project
            .track(midi)
            .unwrap()
            .clips()
            .iter()
            .map(|c| (c.start().get(), project.clip_end(*c).get()))
            .collect::<Vec<_>>()
    };
    assert_eq!(bounds(&rig), [(0, 3840)]);

    // Drag the right edge out to the end of bar 2.
    let edge = rig.lane_point(midi, 1.0) - vec2(2.0, 0.0);
    rig.drag(edge, rig.lane_point(midi, 2.0));
    assert_eq!(bounds(&rig), [(0, 7680)]);

    // Click in the clip at bar 2 to set the marker there, then split.
    rig.frames(45);
    rig.click(rig.lane_point(midi, 1.0) + vec2(1.0, 0.0));
    assert_eq!(rig.app().timeline.marker, Some(midi::Tick::new(3840)));
    rig.key(Modifiers::COMMAND, Key::E);
    assert_eq!(bounds(&rig), [(0, 3840), (3840, 7680)]);
    rig.screenshot("split");

    // Select just the right half and delete it.
    rig.frames(45);
    rig.click(rig.lane_point(midi, 1.5));
    rig.key(Modifiers::NONE, Key::Delete);
    assert_eq!(bounds(&rig), [(0, 3840)]);
    assert!(
        rig.app().session.project.clip(clip).is_some(),
        "the left half kept its id"
    );

    // Drag the last track's header above the first.
    let from = rig.header_point(tracks[3]);
    let first = rig.app().timeline.row_rect(tracks[0]).unwrap();
    rig.drag(from, pos2(from.x, first.top() + 4.0));
    assert_eq!(
        rig.app().session.project.track_ids(),
        [tracks[3], tracks[0], tracks[1], tracks[2]]
    );
    rig.screenshot("reordered");

    // A fader drag is one undo step.
    let undo_before = rig.app().session.undo_label().map(str::to_owned);
    // The volume field is the third control on the header's second line.
    let row = rig.app().timeline.row_rect(tracks[3]).unwrap();
    let volume = pos2(row.left() + 75.0, row.top() + 33.0);
    rig.drag(volume, volume + vec2(-60.0, 0.0));
    let db = rig
        .app()
        .session
        .project
        .track(tracks[3])
        .unwrap()
        .mixer
        .volume_db;
    assert!(db < -1.0, "dragged down to {db}");
    assert_eq!(rig.app().session.undo_label(), Some("Volume"));
    rig.click(pos2(700.0, 420.0)); // focus the timeline, away from the field
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(
        rig.app()
            .session
            .project
            .track(tracks[3])
            .unwrap()
            .mixer
            .volume_db,
        0.0
    );
    assert_eq!(
        rig.app().session.undo_label().map(str::to_owned),
        undo_before
    );
}

#[test]
fn envelope_editor_shapes_the_synth() {
    let mut rig = Rig::new("envelope");
    let track = rig.app().session.project.tracks[0].id;
    rig.frames(2);
    rig.screenshot("device");
    let id = egui::Id::new(("envelope", track.0, 0usize)).with("rect");
    let rect: egui::Rect = rig
        .harness
        .ctx
        .data(|d| d.get_temp(id))
        .expect("the editor is shown");
    let area = rect.shrink(crate::widgets::envelope::PAD);
    let attack = |rig: &Rig| {
        rig.app().session.project.tracks[0]
            .instrument_param("attack")
            .unwrap()
    };
    let curve = |rig: &Rig| {
        rig.app().session.project.tracks[0]
            .instrument_param("attack_curve")
            .unwrap()
    };
    let before = attack(&rig);

    // The attack peak sits at the top, its distance from the left edge set
    // by the attack time on a log scale.
    let param = instruments::find_builtin(instruments::DEFAULT_INSTRUMENT)
        .unwrap()
        .params
        .iter()
        .find(|p| p.id == "attack")
        .unwrap();
    let n = param.to_normalized(before);
    let peak = pos2(
        area.left() + area.width() * crate::widgets::envelope::STAGE_SHARE * (0.04 + 0.96 * n),
        area.top(),
    );
    rig.drag(peak, peak + vec2(40.0, 0.0));
    assert!(
        attack(&rig) > before * 2.0,
        "longer attack: {} -> {}",
        before,
        attack(&rig)
    );
    assert_eq!(rig.app().session.undo_label(), Some("Change Envelope"));

    // Bend the attack: drag its middle dot down to start slow.
    let curve_before = curve(&rig);
    let n = param.to_normalized(attack(&rig));
    let peak_x =
        area.left() + area.width() * crate::widgets::envelope::STAGE_SHARE * (0.04 + 0.96 * n);
    let mid_x = (area.left() + peak_x) / 2.0;
    let mid_y = area.bottom() - dsp::stage_shape(0.5, curve_before) * area.height();
    rig.frames(2);
    rig.drag(pos2(mid_x, mid_y), pos2(mid_x, mid_y + 40.0));
    assert!(
        curve(&rig) > curve_before,
        "bent: {} -> {}",
        curve_before,
        curve(&rig)
    );
    rig.screenshot("bent");

    rig.click(pos2(700.0, 300.0));
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(curve(&rig), curve_before);
    rig.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(attack(&rig), before);
}
