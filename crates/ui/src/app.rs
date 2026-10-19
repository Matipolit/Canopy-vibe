use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand};
use midi::Tick;
use project::{ClipRef, NewTrack};

use crate::clip_panel::ClipPanel;
use crate::device_view;
use crate::export;
use crate::piano_roll::PianoRoll;
use crate::session::{Audio, Session};
use crate::theme::{self, color, size, space};
use crate::timeline::{Action, GridSize, TimelineView, control_menu, volume_drag};
use crate::widgets::StereoMeter;
use crate::widgets::chrome::{self, menu_item, menu_item_enabled};

/// The Wayland file-drop listener, started in `main` (Linux only).
#[cfg(target_os = "linux")]
pub type Drops = Option<crate::wayland_drop::WaylandDrops>;
#[cfg(not(target_os = "linux"))]
pub type Drops = ();

/// Sample rates offered in the status bar.
const SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];
const DEFAULT_SAMPLE_RATE: u32 = 44_100;
/// Key for the chosen sample rate in eframe's saved app state.
const SAMPLE_RATE_KEY: &str = "sample_rate";
/// Key for the recent projects list, one path per line.
const RECENT_KEY: &str = "recent_projects";
/// MIDI devices switched off, one name per line.
const MIDI_OFF_KEY: &str = "midi_disabled";
const MAX_RECENT: usize = 10;
/// How long a status message stays up.
const MESSAGE_TIME: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// The open clip: the piano roll, or an audio clip's settings.
    Clip,
    Device,
    /// The selected chain's effect graph (see `patcher`), once offered.
    Patcher,
}

/// Something that would throw away unsaved changes, waiting for the user
/// to say what to do about them.
#[derive(Debug, Clone)]
enum Pending {
    New,
    Open(PathBuf),
    /// The Import FL Studio Project dialog's project.
    ImportFlp,
    Quit,
}

pub struct CanopyApp {
    pub(crate) session: Session,
    pub(crate) timeline: TimelineView,
    pub(crate) piano_roll: PianoRoll,
    clip_panel: ClipPanel,
    export_dialog: Option<export::Dialog>,
    /// The Export dialog's format and tail settings, kept between uses.
    export_settings: export::Settings,
    pub(crate) flp_import: Option<crate::flp_import::Dialog>,
    detail: Detail,
    pub(crate) detail_open: bool,
    /// The device view was clicked last: Delete, Ctrl+D, copy and paste act
    /// on its effects rather than on clips and tracks.
    devices_focused: bool,
    /// Where the device view was drawn last frame.
    device_rect: egui::Rect,
    meter: StereoMeter,
    /// The audio thread's load as shown: its peak, falling back slowly.
    dsp_load: f32,
    pending: Option<Pending>,
    quitting: bool,
    shortcuts_open: bool,
    /// Copied clips with the tracks they came from.
    clipboard: Vec<(project::TrackId, project::ClipData)>,
    /// Recently opened or saved projects, newest first.
    recent: Vec<PathBuf>,
    /// The project path last frame, to notice opens and saves.
    last_path: Option<PathBuf>,
    /// File drops on Wayland, which winit doesn't deliver.
    #[cfg(target_os = "linux")]
    drops: Option<crate::wayland_drop::WaylandDrops>,
    title: String,
    /// Whether the theme has been applied to egui's style yet.
    styled: bool,
}

impl CanopyApp {
    pub fn new(cc: &eframe::CreationContext<'_>, drops: Drops) -> Self {
        let sample_rate = cc
            .storage
            .and_then(|storage| storage.get_string(SAMPLE_RATE_KEY))
            .and_then(|rate| rate.parse().ok())
            .unwrap_or(DEFAULT_SAMPLE_RATE);
        let ctx = cc.egui_ctx.clone();
        let session = Session::new(sample_rate, move || ctx.request_repaint());
        let mut app = Self::with_session(session);
        #[cfg(target_os = "linux")]
        {
            if let Some(drops) = &drops {
                drops.set_context(cc.egui_ctx.clone());
            }
            app.drops = drops;
        }
        #[cfg(not(target_os = "linux"))]
        let () = drops;
        app.recent = cc
            .storage
            .and_then(|storage| storage.get_string(RECENT_KEY))
            .map(|list| list.lines().map(PathBuf::from).collect())
            .unwrap_or_default();
        app.session.midi.disabled = cc
            .storage
            .and_then(|storage| storage.get_string(MIDI_OFF_KEY))
            .map(|list| list.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        app
    }

    pub fn with_session(session: Session) -> Self {
        let mut timeline = TimelineView::default();
        timeline.selection.tracks = session
            .project
            .tracks
            .first()
            .map(|t| t.id)
            .into_iter()
            .collect();
        Self {
            session,
            timeline,
            piano_roll: PianoRoll::default(),
            clip_panel: ClipPanel::default(),
            export_dialog: None,
            export_settings: export::Settings {
                format: media::WavFormat::Pcm24,
                tail: true,
                loop_only: false,
            },
            flp_import: None,
            detail: Detail::Device,
            detail_open: true,
            devices_focused: false,
            device_rect: egui::Rect::NOTHING,
            meter: StereoMeter::default(),
            dsp_load: 0.0,
            pending: None,
            quitting: false,
            shortcuts_open: false,
            clipboard: Vec::new(),
            recent: Vec::new(),
            last_path: None,
            #[cfg(target_os = "linux")]
            drops: None,
            title: String::new(),
            styled: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn piano_roll_rect(&self) -> egui::Rect {
        self.piano_roll.rect()
    }

    /// Opens a project as File → Open does.
    #[cfg(test)]
    pub(crate) fn open_project(&mut self, path: &std::path::Path) {
        self.run(Pending::Open(path.to_owned()));
    }

    #[cfg(test)]
    pub(crate) fn export_dialog(&self) -> Option<&export::Dialog> {
        self.export_dialog.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn piano_roll_shown(&self) -> bool {
        self.detail_open && self.detail == Detail::Clip && !self.audio_clip_open()
    }

    /// True if the open clip is an audio clip, so the Clip tab shows its
    /// settings rather than the piano roll.
    fn audio_clip_open(&self) -> bool {
        let open = self.timeline.selection.open_clip;
        matches!(
            open.and_then(|id| self.session.project.clip(id)),
            Some((_, ClipRef::Audio(_)))
        )
    }

    /// True if the detail view shows effects (the rack or the Patcher), so
    /// the keyboard can act on them.
    fn shows_devices(&self) -> bool {
        matches!(self.detail, Detail::Device | Detail::Patcher)
    }

    fn act(&mut self, action: Action) {
        self.timeline.apply(&mut self.session, action);
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // A knob with the keyboard focus turns with the arrows, Home and
        // End: shortcuts on them wait.
        let shortcut = |modifiers, key| {
            !crate::widgets::knob::takes_key(ctx, key)
                && ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(modifiers, key)))
        };
        // F1 types nothing, so it works from the shortcuts' own filter too.
        if shortcut(Modifiers::NONE, Key::F1) {
            self.shortcuts_open = !self.shortcuts_open;
        }
        // Leave the rest alone while text is being typed. Not
        // `egui_wants_keyboard_input`: that's also true when Tab has merely
        // moved focus to a button, which would switch every shortcut off.
        if ctx.text_edit_focused() {
            return;
        }
        let ctrl = Modifiers::COMMAND;
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;

        // Consuming the key keeps a focused button from also reacting to Space.
        if shortcut(Modifiers::NONE, Key::Space) {
            self.session.toggle_playing();
        }
        if shortcut(Modifiers::NONE, Key::Home) {
            self.session.seek(Tick::ZERO);
        }
        if shortcut(ctrl, Key::M) {
            let on = !self.session.metronome();
            self.session.set_metronome(on);
        }
        // Check Ctrl+Shift combinations before the plain Ctrl ones, which
        // would otherwise match them too.
        if shortcut(ctrl_shift, Key::Z) || shortcut(ctrl, Key::Y) {
            self.session.redo();
        }
        if shortcut(ctrl, Key::Z) {
            self.session.undo();
        }
        if shortcut(ctrl_shift, Key::S) {
            self.save_as();
        }
        if shortcut(ctrl, Key::S) {
            self.save();
        }
        if shortcut(ctrl, Key::O) {
            self.open_dialog();
        }
        if shortcut(ctrl, Key::N) {
            self.guard(Pending::New);
        }
        if shortcut(ctrl, Key::I) {
            self.import_dialog();
        }
        if shortcut(ctrl, Key::R) {
            self.open_export(export::What::Mix, self.timeline.selection.tracks.clone());
        }
        // egui ignores extra Alt and Shift when matching, so check: Alt+R is
        // the piano roll's Naturalize.
        let plain = ctx.input(|i| !i.modifiers.alt && !i.modifiers.shift);
        if plain && shortcut(Modifiers::NONE, Key::R) {
            let on = !self.session.is_recording();
            self.session.set_recording(on);
        }
        if shortcut(Modifiers::SHIFT, Key::L) {
            self.loop_selection();
        }
        if plain && shortcut(Modifiers::NONE, Key::L) {
            // In the piano roll, L loops the clip being edited.
            if self.piano_roll_has_keys() {
                self.piano_roll.toggle_loop(&mut self.session);
            } else {
                self.session.toggle_loop();
            }
        }
        if shortcut(ctrl_shift, Key::T) {
            self.add_track(NewTrack::Midi);
        }
        if shortcut(ctrl, Key::T) {
            self.add_track(NewTrack::Audio);
        }
        if shortcut(ctrl_shift, Key::G) {
            self.ungroup();
        }
        if shortcut(ctrl, Key::G) {
            self.group();
        }
        if shortcut(ctrl, Key::Num4) {
            self.timeline.grid.snap = !self.timeline.grid.snap;
        }
        if shortcut(ctrl, Key::Num1) {
            self.step_grid(1);
        }
        if shortcut(ctrl, Key::Num2) {
            self.step_grid(-1);
        }
        if shortcut(Modifiers::SHIFT, Key::Tab) {
            // egui would also move the keyboard focus back a control later
            // this frame (and ring it): Shift+Tab is ours, so cancel that
            // and let go of the focus.
            ctx.memory_mut(|m| {
                m.move_focus(egui::FocusDirection::None);
                if let Some(focused) = m.focused() {
                    m.surrender_focus(focused);
                }
            });
            self.detail_open = true;
            self.detail = match self.detail {
                Detail::Clip => Detail::Device,
                Detail::Device if crate::patcher::READY => Detail::Patcher,
                Detail::Device | Detail::Patcher => Detail::Clip,
            };
        }

        // The rest act on whichever view was clicked last; the piano roll
        // handles its own.
        if self.piano_roll_has_keys() {
            return;
        }
        if self.devices_focused && self.detail_open && self.shows_devices() {
            crate::effect_rack::shortcuts(ctx, &mut self.session, &mut self.timeline);
            return;
        }
        if shortcut(Modifiers::NONE, Key::Delete) || shortcut(Modifiers::NONE, Key::Backspace) {
            self.delete_selection();
        }
        if shortcut(ctrl, Key::D) {
            let clips = self.timeline.selection.clips.clone();
            if !clips.is_empty() {
                self.act(Action::DuplicateClips(clips));
            }
        }
        if shortcut(ctrl, Key::E) {
            self.split();
        }
        if shortcut(Modifiers::NONE, Key::B) {
            self.timeline.automation_draw = !self.timeline.automation_draw;
        }
        // Shift first: the plain one would match it too.
        if shortcut(Modifiers::SHIFT, Key::Z) {
            self.timeline.zoom_to_fit(ctx, &self.session.project, true);
        }
        if shortcut(Modifiers::NONE, Key::Z) {
            self.timeline.zoom_to_fit(ctx, &self.session.project, false);
        }
        if shortcut(Modifiers::NONE, Key::Num0) {
            let clips = self.timeline.selection.clips.clone();
            let project = &self.session.project;
            let muted = clips
                .iter()
                .all(|id| project.clip(*id).is_some_and(|(_, c)| c.muted()));
            if !clips.is_empty() {
                self.act(Action::MuteClips(clips, !muted));
            }
        }
        let (copy, cut, paste) = clipboard_events(ctx);
        if copy || cut {
            self.copy_clips(ctx, cut);
        }
        if paste {
            self.paste_clips();
        }
        for (key, direction) in [(Key::ArrowLeft, -1), (Key::ArrowRight, 1)] {
            if shortcut(Modifiers::NONE, key) {
                self.nudge_clips(direction);
            }
        }
        if shortcut(ctrl, Key::A) {
            let mut clips = Vec::new();
            self.session
                .project
                .visit(|t, _| clips.extend(t.clips().iter().map(|c| c.id())));
            self.timeline.selection.clips = clips;
        }
        // Not consumed: the timeline also reads it, to cancel a drag.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.timeline.selection.clips.clear();
        }
    }

    /// Whether the piano roll has the keyboard: it was clicked last and
    /// is showing.
    fn piano_roll_has_keys(&self) -> bool {
        self.piano_roll.focused
            && self.detail_open
            && self.detail == Detail::Clip
            && !self.audio_clip_open()
    }

    /// Shift+L: loops what's selected, the notes in the piano roll if it
    /// has the keyboard, or else the clips, and starts the playhead in it.
    fn loop_selection(&mut self) {
        if self.piano_roll_has_keys() {
            self.piano_roll.loop_selection(&mut self.session);
            return;
        }
        let project = &self.session.project;
        let span = self
            .timeline
            .selection
            .clips
            .iter()
            .filter_map(|id| project.clip(*id))
            .map(|(_, clip)| (clip.start(), project.clip_end(clip)))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
        match span {
            Some((start, end)) => self.session.loop_span(start, end),
            None => self
                .session
                .info("Select clips to loop (Shift+L), or Shift+drag the ruler"),
        }
    }

    fn copy_clips(&mut self, ctx: &egui::Context, cut: bool) {
        let clips = self.timeline.selection.clips.clone();
        if clips.is_empty() {
            return;
        }
        self.clipboard = self.session.project.copy_clips(&clips);
        // Ctrl+V only reaches us as a paste event when the system clipboard
        // holds text, so leave some there.
        ctx.copy_text("canopy clips".to_owned());
        if cut {
            self.act(Action::DeleteClips(clips));
        }
    }

    /// Pastes at the marker (or the playhead) onto the selected track.
    fn paste_clips(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let at = self
            .timeline
            .marker
            .unwrap_or_else(|| Tick::new(self.session.playhead().round() as i64));
        let onto = self.timeline.selection.primary_track();
        let clips = self.clipboard.clone();
        if let Some(ids) = self
            .session
            .edit("Paste", |p| p.paste_clips(&clips, at, onto))
        {
            self.timeline.selection.clips = ids;
        }
    }

    /// Moves the selected clips one grid step left or right.
    fn nudge_clips(&mut self, direction: i64) {
        let project = &self.session.project;
        let step = self
            .timeline
            .grid
            .step(&self.timeline.viewport, &project.tempo)
            * direction;
        let moves: Vec<_> = self
            .timeline
            .selection
            .clips
            .iter()
            .filter_map(|id| project.clip(*id))
            .map(|(track, clip)| project::ClipMove {
                clip: clip.id(),
                track,
                start: clip.start() + step,
            })
            .collect();
        if moves.is_empty() || moves.iter().any(|m| m.start < Tick::ZERO) {
            return;
        }
        self.act(Action::MoveClips(moves));
    }

    fn step_grid(&mut self, finer: isize) {
        let sizes = GridSize::ALL;
        let index = sizes
            .iter()
            .position(|s| *s == self.timeline.grid.size)
            .unwrap_or(0) as isize;
        let next = (index + finer).clamp(0, sizes.len() as isize - 1);
        self.timeline.grid.size = sizes[next as usize];
    }

    fn add_track(&mut self, kind: NewTrack) {
        let position = self.timeline.new_track_position(&self.session.project);
        self.act(Action::AddTrack(kind, position));
    }

    fn group(&mut self) {
        let tracks = self.timeline.selection.tracks.clone();
        if tracks.is_empty() {
            self.session
                .info("Select tracks to group (click their headers, Ctrl-click for more)");
        } else {
            self.act(Action::GroupTracks(tracks));
        }
    }

    fn ungroup(&mut self) {
        let groups: Vec<_> = self
            .timeline
            .selection
            .tracks
            .iter()
            .copied()
            .filter(|id| {
                self.session
                    .project
                    .track(*id)
                    .is_some_and(|t| t.is_group())
            })
            .collect();
        for group in groups {
            self.act(Action::Ungroup(group));
        }
    }

    fn delete_selection(&mut self) {
        if let Some(points) = &self.timeline.automation_selection
            && !points.indices.is_empty()
        {
            let (track, target, kept) = (points.track, points.target.clone(), points.without());
            self.act(Action::SetAutomation {
                track,
                target,
                points: kept,
            });
            return;
        }
        let selection = &self.timeline.selection;
        if !selection.clips.is_empty() {
            let clips = selection.clips.clone();
            self.act(Action::DeleteClips(clips));
        } else if !selection.tracks.is_empty() {
            let tracks = selection.tracks.clone();
            self.act(Action::DeleteTracks(tracks));
        }
    }

    /// Splits the selected clips at the marker, or at the playhead if no
    /// marker is set.
    fn split(&mut self) {
        let at = self
            .timeline
            .marker
            .unwrap_or_else(|| Tick::new(self.session.playhead().round() as i64));
        let clips = self.timeline.selection.clips.clone();
        if clips.is_empty() {
            self.session
                .info("Select clips to split (Ctrl+E splits at the marker)");
        } else {
            self.act(Action::SplitClips(clips, at));
        }
    }

    // Files.

    /// Runs `pending` now, or asks first if it would lose unsaved changes.
    fn guard(&mut self, pending: Pending) {
        if self.session.is_dirty() {
            self.pending = Some(pending);
        } else {
            self.run(pending);
        }
    }

    fn run(&mut self, pending: Pending) {
        match pending {
            Pending::New => {
                self.session.new_project();
                self.reset_views();
            }
            Pending::Open(path) => {
                self.session.open(&path);
                self.reset_views();
            }
            Pending::ImportFlp => {
                if let Some(dialog) = self.flp_import.take() {
                    dialog.import_into(&mut self.session);
                    self.reset_views();
                }
            }
            Pending::Quit => self.quitting = true,
        }
    }

    fn reset_views(&mut self) {
        self.timeline = TimelineView::default();
        // The arrangement as it was when the project was saved.
        if let Some(view) = self.session.project.arrangement_view {
            self.timeline.restore_view(&view);
        }
        self.piano_roll = PianoRoll::default();
        self.timeline.selection.tracks = self
            .session
            .project
            .tracks
            .first()
            .map(|t| t.id)
            .into_iter()
            .collect();
    }

    fn open_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Open project")
            .add_filter("canopy project", &[project::FILE_EXTENSION])
            .pick_file();
        if let Some(path) = picked {
            self.guard(Pending::Open(path));
        }
    }

    /// Returns true only if the project was saved: false if the user
    /// cancelled or the write failed.
    fn save(&mut self) -> bool {
        match self.session.save() {
            Some(saved) => saved,
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        let name = format!(
            "{}.{}",
            self.session.display_name(),
            project::FILE_EXTENSION
        );
        let picked = rfd::FileDialog::new()
            .set_title("Save project")
            .set_file_name(name)
            .add_filter("canopy project", &[project::FILE_EXTENSION])
            .save_file();
        match picked {
            Some(path) => self.session.save_as(&path),
            None => false,
        }
    }

    fn open_export(&mut self, what: export::What, tracks: Vec<project::TrackId>) {
        self.export_dialog = Some(export::Dialog::new(what, tracks, self.export_settings));
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        if let Some(tracks) = self.timeline.export_requested.take() {
            self.open_export(export::What::Selected, tracks);
        }
        let Some(dialog) = &mut self.export_dialog else {
            return;
        };
        let name = self.session.display_name();
        let folder = self.session.path.as_ref().and_then(|p| p.parent());
        match dialog.show(ctx, &self.session.project, &name, folder) {
            export::Outcome::Open => {}
            export::Outcome::Cancel => {
                self.export_settings = dialog.settings;
                self.export_dialog = None;
            }
            export::Outcome::Export(jobs, settings) => {
                self.export_settings = settings;
                self.export_dialog = None;
                self.session.export(jobs, settings);
            }
        }
    }

    fn open_flp_import(&mut self) {
        let Some(path) = crate::flp_import::Dialog::pick() else {
            return;
        };
        let sfz_dir = crate::flp_import::Dialog::default_sfz_dir(&path);
        let installed = crate::flp_import::installed(self.session.plugins.manager.catalog());
        match crate::flp_import::Dialog::open(&path, sfz_dir, installed) {
            Ok(dialog) => self.flp_import = Some(dialog),
            Err(err) => self.session.error(err),
        }
    }

    fn flp_import_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.flp_import else {
            return;
        };
        match dialog.show(ctx) {
            crate::flp_import::Outcome::Open => {}
            crate::flp_import::Outcome::Cancel => self.flp_import = None,
            crate::flp_import::Outcome::Import => self.guard(Pending::ImportFlp),
        }
    }

    fn import_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Import audio")
            .add_filter(
                "Audio",
                &[
                    "wav", "wave", "flac", "mp3", "ogg", "oga", "aif", "aiff", "aifc",
                ],
            )
            .pick_files();
        if let Some(paths) = picked {
            let track = self.timeline.selection.primary_track();
            let at = self.timeline.marker.unwrap_or(Tick::ZERO);
            let position = self.timeline.new_track_position(&self.session.project);
            self.act(Action::Import(paths, track, at, position));
        }
    }

    // Panels.

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| self.file_menu(ui));
            ui.menu_button("Edit", |ui| self.edit_menu(ui));
            ui.menu_button("View", |ui| self.view_menu(ui));
            ui.menu_button("Track", |ui| {
                if menu_item(ui, "New MIDI Track", "Ctrl+Shift+T").clicked() {
                    self.add_track(NewTrack::Midi);
                }
                if menu_item(ui, "New Audio Track", "Ctrl+T").clicked() {
                    self.add_track(NewTrack::Audio);
                }
                if menu_item(ui, "New Group", "").clicked() {
                    self.add_track(NewTrack::Group);
                }
                ui.separator();
                if menu_item(ui, "Group Selected", "Ctrl+G").clicked() {
                    self.group();
                }
                if menu_item(ui, "Ungroup", "Ctrl+Shift+G").clicked() {
                    self.ungroup();
                }
            });
            ui.menu_button("MIDI", |ui| self.midi_menu(ui));
            ui.menu_button("Help", |ui| {
                if menu_item(ui, "Keyboard Shortcuts", "F1").clicked() {
                    self.shortcuts_open = true;
                }
            });
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        if menu_item(ui, "New", "Ctrl+N").clicked() {
            self.guard(Pending::New);
        }
        if menu_item(ui, "Open…", "Ctrl+O").clicked() {
            self.open_dialog();
        }
        ui.add_enabled_ui(!self.recent.is_empty(), |ui| {
            ui.menu_button("Open Recent", |ui| {
                for path in self.recent.clone() {
                    let name = path.file_stem().map_or_else(
                        || path.display().to_string(),
                        |s| s.to_string_lossy().into_owned(),
                    );
                    if ui
                        .button(name)
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        self.guard(Pending::Open(path));
                    }
                }
                ui.separator();
                if ui.button("Clear").clicked() {
                    self.recent.clear();
                }
            });
        });
        if menu_item(ui, "Save", "Ctrl+S").clicked() {
            self.save();
        }
        if menu_item(ui, "Save As…", "Ctrl+Shift+S").clicked() {
            self.save_as();
        }
        ui.separator();
        if menu_item(ui, "Import Audio…", "Ctrl+I").clicked() {
            self.import_dialog();
        }
        if menu_item(ui, "Import FL Studio Project…", "").clicked() {
            self.open_flp_import();
        }
        if menu_item(ui, "Export Audio…", "Ctrl+R").clicked() {
            let tracks = self.timeline.selection.tracks.clone();
            self.open_export(export::What::Mix, tracks);
        }
        ui.separator();
        if menu_item(ui, "Quit", "").clicked() {
            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        let undo = self.session.undo_label().map(|l| format!("Undo {l}"));
        if menu_item_enabled(
            ui,
            undo.is_some(),
            undo.as_deref().unwrap_or("Undo"),
            "Ctrl+Z",
        )
        .clicked()
        {
            self.session.undo();
        }
        let redo = self.session.redo_label().map(|l| format!("Redo {l}"));
        if menu_item_enabled(
            ui,
            redo.is_some(),
            redo.as_deref().unwrap_or("Redo"),
            "Ctrl+Shift+Z",
        )
        .clicked()
        {
            self.session.redo();
        }
        ui.separator();
        let has_clips = !self.timeline.selection.clips.is_empty();
        if menu_item_enabled(ui, has_clips, "Duplicate", "Ctrl+D").clicked() {
            let clips = self.timeline.selection.clips.clone();
            self.act(Action::DuplicateClips(clips));
        }
        if menu_item_enabled(ui, has_clips, "Split", "Ctrl+E").clicked() {
            self.split();
        }
        if menu_item(ui, "Delete", "Del").clicked() {
            self.delete_selection();
        }
        ui.separator();
        if menu_item_enabled(ui, has_clips, "Loop Selection", "Shift+L").clicked() {
            self.loop_selection();
        }
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.detail_open, "Detail View");
        ui.checkbox(&mut self.timeline.follow, "Follow Playhead");
        ui.separator();
        if menu_item(ui, "Zoom to Selection", "Z").clicked() {
            let project = &self.session.project;
            self.timeline.zoom_to_fit(ui.ctx(), project, false);
        }
        if menu_item(ui, "Zoom to Song", "Shift+Z").clicked() {
            let project = &self.session.project;
            self.timeline.zoom_to_fit(ui.ctx(), project, true);
        }
        ui.separator();
        if menu_item(ui, "Plugin Manager…", "").clicked() {
            self.session.plugins.window = true;
        }
    }

    /// The song's key: a root and a scale, or none. A menu rather than a
    /// combo box, whose popup scrolls instead of fitting this.
    fn scale_picker(&mut self, ui: &mut egui::Ui) {
        let current = self.session.project.scale;
        let mut picked = current;
        let text = current.map_or_else(|| "No scale".to_owned(), project::Scale::name);
        ui.menu_button(text, |ui| {
            let (root, mode) = current.map_or((0, project::ScaleMode::Major), |s| (s.root, s.mode));
            if ui.selectable_label(current.is_none(), "No scale").clicked() {
                picked = None;
            }
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.set_max_width(6.0 * 34.0);
                for (class, name) in project::NOTE_NAMES.iter().enumerate() {
                    let class = class as u8;
                    let on = current.is_some_and(|s| s.root == class);
                    if ui
                        .add_sized([28.0, 20.0], egui::Button::selectable(on, *name))
                        .clicked()
                    {
                        picked = Some(project::Scale::new(class, mode));
                    }
                }
            });
            ui.separator();
            egui::Grid::new("scale_modes")
                .num_columns(2)
                .show(ui, |ui| {
                    for pair in project::ScaleMode::ALL.chunks(2) {
                        for &option in pair {
                            let on = current.is_some_and(|s| s.mode == option);
                            if ui.selectable_label(on, option.name()).clicked() {
                                picked = Some(project::Scale::new(root, option));
                            }
                        }
                        ui.end_row();
                    }
                });
            if picked != current {
                ui.close();
            }
        })
        .response
        .on_hover_text("The song's key, shown in the piano roll");
        if picked != current {
            self.session.edit("Set Scale", |p| p.set_scale(picked));
        }
    }

    fn midi_menu(&mut self, ui: &mut egui::Ui) {
        let ports: Vec<(String, bool)> = self
            .session
            .midi
            .ports()
            .map(|(name, open)| (name.to_owned(), open))
            .collect();
        if ports.is_empty() {
            ui.weak("No MIDI devices");
        }
        for (name, _) in ports {
            let mut on = !self.session.midi.disabled.contains(&name);
            if ui.checkbox(&mut on, &name).changed() {
                self.session.midi.set_enabled(&name, on);
            }
        }
        if let Some(error) = &self.session.midi.error {
            ui.separator();
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.separator();
        let target = self
            .session
            .live_target()
            .and_then(|t| self.session.project.track(t))
            .map_or_else(|| "nothing".to_owned(), |t| t.name.clone());
        ui.weak(format!("Keyboards play {target}"));
    }

    /// A MIDI keyboard plays the selected MIDI track, or the one it played
    /// last if the selection isn't one.
    fn follow_selection_with_keyboard(&mut self) {
        let project = &self.session.project;
        let is_midi = |id: project::TrackId| {
            matches!(
                project.track(id).map(|t| &t.kind),
                Some(project::TrackKind::Midi { .. })
            )
        };
        let target = match self.timeline.selection.primary_track() {
            Some(track) if is_midi(track) => Some(track),
            _ => self.session.live_target().filter(|t| is_midi(*t)),
        };
        self.session.set_live_target(target);
    }

    fn transport_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            // Playing and recording.
            let playing = self.session.is_playing();
            let label = if playing { "⏹ Stop" } else { "▶ Play" };
            // As wide either way, so nothing after it jumps.
            let play = ui.add(
                egui::Button::new(label)
                    .selected(playing)
                    .min_size(egui::vec2(64.0, size::CONTROL)),
            );
            let play = chrome::shortcut_tip(
                play,
                "Play or stop (stop again to go back to the start)",
                "Space",
            );
            if play.clicked() {
                self.session.toggle_playing();
            }
            let recording = self.session.is_recording();
            let record = chrome::shortcut_tip(
                record_button(ui, recording),
                "Record from a MIDI keyboard",
                "R",
            );
            if record.clicked() {
                self.session.set_recording(!recording);
            }
            let metronome = self.session.metronome();
            if chrome::switch(ui, metronome, "Metronome", "Click along", "Ctrl+M").clicked() {
                self.session.set_metronome(!metronome);
            }
            let looping = self.session.project.loop_region.is_some_and(|r| r.on);
            if chrome::switch(
                ui,
                looping,
                "Loop",
                "Play the loop round and round (Shift+drag the ruler, or drag along its top, to set where)",
                "L",
            )
            .clicked()
            {
                self.session.toggle_loop();
            }
            group_gap(ui);

            // Where the song is.
            self.position_display(ui);
            group_gap(ui);

            // Tempo, meter and key.
            let mut bpm = self.session.project.tempo.tempo.to_bpm();
            let response = ui
                .add(
                    egui::DragValue::new(&mut bpm)
                        .range(project::Tempo::MIN_BPM..=project::Tempo::MAX_BPM)
                        .speed(0.1)
                        // A typed tempo applies on Enter, not per keystroke.
                        .update_while_editing(false)
                        .fixed_decimals(2)
                        .suffix(" BPM"),
                )
                .on_hover_text("Tempo: drag, or double-click to type");
            if response.changed() {
                self.session.live_edit(|p| {
                    p.set_tempo_bpm(bpm);
                    Ok(())
                });
            }
            if response.drag_stopped()
                || (response.changed() && !response.dragged())
                || response.lost_focus()
            {
                self.session.end_live("Tempo");
            }
            let signature = self.session.project.tempo.time_signature;
            ui.weak(format!("{}/{}", signature.numerator, signature.denominator))
                .on_hover_text("Time signature");
            self.scale_picker(ui);
            group_gap(ui);

            // The arrangement's grid.
            ui.label("Grid");
            egui::ComboBox::from_id_salt("grid")
                .selected_text(self.timeline.grid.size.label())
                .width(80.0)
                .show_ui(ui, |ui| {
                    for size in GridSize::ALL {
                        ui.selectable_value(&mut self.timeline.grid.size, size, size.label());
                    }
                })
                .response
                .on_hover_text("Ctrl+1 for finer, Ctrl+2 for coarser");
            let snap = self.timeline.grid.snap;
            if chrome::switch(ui, snap, "Snap", "Snap to the grid", "Ctrl+4").clicked() {
                self.timeline.grid.snap = !snap;
            }
            let follow = self.timeline.follow;
            if chrome::switch(ui, follow, "Follow", "Keep the playhead in view while playing", "")
                .clicked()
            {
                self.timeline.follow = !follow;
            }
            let fit = chrome::icon_button(ui, chrome::Icon::Fit, "Zoom to fit arrangement", "");
            let fit = chrome::shortcut_tip(
                fit,
                "Zoom to the selected clips, or the whole song (Shift: always the song)",
                "Z / Shift+Z",
            );
            if fit.clicked() {
                let song = ui.input(|i| i.modifiers.shift);
                self.timeline.zoom_to_fit(ui.ctx(), &self.session.project, song);
            }

            // The detail view's switch, at the far end.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let detail = self.detail_open;
                if chrome::switch(
                    ui,
                    detail,
                    "Detail",
                    "Show the clip and device views (Shift+Tab switches between them)",
                    "",
                )
                .clicked()
                {
                    self.detail_open = !detail;
                }
            });
        });
    }

    /// The playhead as bar.beat.sixteenth and as time, on a dark ground.
    fn position_display(&self, ui: &mut egui::Ui) {
        let playhead = self.session.playhead();
        let position = self
            .session
            .project
            .tempo
            .bar_beat(Tick::new(playhead.floor() as i64));
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(egui::CornerRadius::same(theme::radius::CONTROL))
            .inner_margin(egui::Margin::symmetric(space::M as i8, 0))
            .show(ui, |ui| {
                ui.set_min_height(size::CONTROL);
                ui.monospace(RichText::new(format!("{position:>8}")).strong())
                    .on_hover_text("bar.beat.sixteenth");
                ui.add_space(space::S);
                ui.label(
                    RichText::new(format_time(self.session.playhead_seconds()))
                        .monospace()
                        .color(ui.visuals().weak_text_color()),
                );
            });
    }

    fn detail_panel(&mut self, ui: &mut egui::Ui) {
        let audio = self.audio_clip_open();
        // The tabs and the open view's controls, on a bar across the top.
        let bar = egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::symmetric(space::M as i8, space::XS as i8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.set_min_height(size::BAR - 2.0 * space::XS);
                    let mut tabs = vec![
                        (
                            Detail::Clip,
                            "Clip",
                            "The open clip: its notes, or an audio clip's settings",
                        ),
                        (
                            Detail::Device,
                            "Device",
                            "The selected track's instrument and effects",
                        ),
                    ];
                    if crate::patcher::READY {
                        tabs.push((
                            Detail::Patcher,
                            "Patcher",
                            "The selected track's effect graph",
                        ));
                    }
                    let mut picked = None;
                    for (detail, label, what) in tabs {
                        let tab = ui.add(egui::Button::selectable(self.detail == detail, label));
                        let tab = chrome::shortcut_tip(tab, what, "Shift+Tab");
                        if tab.clicked() {
                            self.detail = detail;
                        }
                        if self.detail == detail {
                            picked = Some(tab.rect);
                        }
                    }
                    if self.detail == Detail::Clip
                        && !audio
                        && let Some((name, color)) = self.open_clip_name()
                    {
                        group_gap(ui);
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, color);
                        ui.strong(name);
                        group_gap(ui);
                        self.piano_roll.toolbar(ui, &mut self.session);
                    }
                    picked
                })
                .inner
            });
        // The selected tab's underline slides to it.
        if let Some(tab) = bar.inner {
            let ctx = ui.ctx();
            let id = egui::Id::new("detail tab underline");
            let left = ctx.animate_value_with_time(id.with(0), tab.left(), theme::anim::NORMAL);
            let right = ctx.animate_value_with_time(id.with(1), tab.right(), theme::anim::NORMAL);
            let y = bar.response.rect.bottom() - 1.0;
            ui.painter().hline(
                left + 2.0..=right - 2.0,
                y,
                egui::Stroke::new(theme::stroke::INDICATOR, theme::accent(ui.visuals())),
            );
        }
        let content = egui::Frame::new().inner_margin(egui::Margin {
            left: space::M as i8,
            right: space::M as i8,
            top: space::S as i8 + 2,
            bottom: space::S as i8,
        });
        content.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            match (self.detail, self.timeline.selection.open_clip) {
                (Detail::Clip, Some(clip)) if audio => {
                    let step = self.timeline.grid_step(&self.session);
                    self.clip_panel
                        .show(ui, &mut self.session, &mut self.timeline, clip, step);
                }
                (Detail::Clip, clip) => {
                    let keys = self.pending.is_none();
                    self.piano_roll.show(ui, &mut self.session, clip, keys);
                }
                (Detail::Device, _) => {
                    self.device_rect = ui.available_rect_before_wrap();
                    device_view::show(ui, &mut self.session, &mut self.timeline);
                }
                (Detail::Patcher, _) => {
                    self.device_rect = ui.available_rect_before_wrap();
                    crate::patcher::show(ui, &mut self.session, &mut self.timeline);
                }
            }
        });
    }

    /// The open MIDI clip's name (its track's, if it has none of its own)
    /// and its track's color.
    fn open_clip_name(&self) -> Option<(String, egui::Color32)> {
        let (track, clip) = self
            .session
            .project
            .clip(self.timeline.selection.open_clip?)?;
        let track = self.session.project.track(track)?;
        let color = crate::timeline::color32(track.color);
        match clip {
            ClipRef::Midi(midi) if !midi.name.is_empty() => Some((midi.name.clone(), color)),
            ClipRef::Midi(_) => Some((track.name.clone(), color)),
            ClipRef::Audio(_) => None,
        }
    }

    fn master_panel(&mut self, ui: &mut egui::Ui) {
        self.drop_effects_on_master(ui);
        ui.vertical_centered(|ui| {
            // Click for the master's effects in the device view.
            let selected = self.timeline.selection.master;
            let width = ui.available_width();
            if ui
                .add(
                    egui::Button::new("Master")
                        .selected(selected)
                        .min_size(egui::vec2(width, size::CONTROL)),
                )
                .on_hover_text("The master's effects, in the device view")
                .clicked()
            {
                self.timeline.selection.master = true;
                self.detail = Detail::Device;
                self.detail_open = true;
            }
            let mut mixer = self.session.project.master;
            let response = ui.add(volume_drag(&mut mixer.volume_db));
            control_menu(
                &mut self.timeline,
                &response,
                None,
                project::AutomationTarget::Volume,
            );
            if response.changed() {
                self.act(Action::LiveMixer { track: None, mixer });
            }
            if response.drag_stopped()
                || (response.changed() && !response.dragged())
                || response.lost_focus()
            {
                self.session.end_live("Master Volume");
            }
        });
        self.meter.show(ui);
    }

    /// Effects dragged from the device view onto the master's strip go to
    /// the end of its chain (with Ctrl, copies of them).
    fn drop_effects_on_master(&mut self, ui: &egui::Ui) {
        let Some(drag) = egui::DragAndDrop::payload::<crate::effect_rack::DeviceDrag>(ui.ctx())
        else {
            return;
        };
        let area = ui.max_rect();
        if !ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| area.contains(p))
        {
            return;
        }
        ui.painter().rect_stroke(
            area.shrink(1.0),
            theme::radius::CONTROL,
            egui::Stroke::new(theme::stroke::INDICATOR, theme::accent(ui.visuals())),
            egui::StrokeKind::Inside,
        );
        let copy = ui.input(|i| i.modifiers.command);
        if let Some(pointer) = ui.ctx().pointer_hover_pos() {
            let verb = if copy { "Copy" } else { "Move" };
            chrome::drop_label(ui.ctx(), pointer, &format!("{verb} to the master"));
        }
        if ui.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(ui.ctx());
            self.act(Action::DropDevices {
                ids: drag.ids.clone(),
                to: None,
                copy,
            });
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.sample_rate_picker(ui);
            ui.separator();
            match &self.session.audio {
                Ok(Audio { output: None, .. }) => {
                    ui.weak("engine running without a device");
                }
                Ok(Audio {
                    output: Some(output),
                    ..
                }) => {
                    let buffer = match output.buffer_frames {
                        Some(frames) => {
                            let ms = frames as f32 / output.sample_rate as f32 * 1000.0;
                            format!("{frames} frames ({ms:.1} ms)")
                        }
                        None => "device buffer size".to_owned(),
                    };
                    ui.weak(format!(
                        "{} · {} · {buffer}",
                        output.host_name, output.device_name
                    ));
                    if output.sample_rate != self.session.requested_rate {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "running at {} Hz: device doesn't support {} Hz",
                                output.sample_rate, self.session.requested_rate
                            ),
                        );
                    }
                }
                Err(err) => {
                    ui.colored_label(ui.visuals().error_fg_color, format!("No audio: {err}"));
                }
            }
            if self.session.audio.is_ok() {
                ui.separator();
                let load = self.dsp_load;
                let color = if load >= 0.8 {
                    ui.visuals().error_fg_color
                } else if load >= 0.5 {
                    ui.visuals().warn_fg_color
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.label(
                    RichText::new(format!("DSP {:>3.0}%", load * 100.0))
                        .monospace()
                        .color(color),
                )
                .on_hover_text(
                    "How much of its time the audio thread needs (at its busiest lately); \
                     at 100% the sound breaks up",
                );
            }
            if self.session.midi.active() {
                ui.separator();
                let size = egui::vec2(10.0, 10.0);
                let (dot, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color::ACTIVE);
                ui.label("MIDI");
                ctx_repaint_soon(ui);
            }
            if let Some(progress) = self.session.export_progress() {
                ui.separator();
                ui.add(
                    egui::ProgressBar::new(progress)
                        .desired_width(140.0)
                        .text(format!("Exporting {:.0}%", progress * 100.0)),
                );
            }
            if let Some(message) = &self.session.message
                && message.at.elapsed() < MESSAGE_TIME
            {
                // It fades out over its last half second, and is gone
                // without waiting for something else to repaint.
                let left = (MESSAGE_TIME - message.at.elapsed()).as_secs_f32();
                let fade = (left / 0.5).min(1.0);
                if fade < 1.0 {
                    ui.ctx().request_repaint();
                } else {
                    ui.ctx()
                        .request_repaint_after(Duration::from_secs_f32(left - 0.5));
                }
                ui.separator();
                let color = if message.error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                ui.colored_label(color.gamma_multiply(fade), &message.text);
            }
        });
    }

    fn sample_rate_picker(&mut self, ui: &mut egui::Ui) {
        let before = self.session.requested_rate;
        egui::ComboBox::from_id_salt("sample_rate")
            .selected_text(format!("{} Hz", self.session.requested_rate))
            .show_ui(ui, |ui| {
                for rate in SAMPLE_RATES {
                    ui.selectable_value(
                        &mut self.session.requested_rate,
                        rate,
                        format!("{rate} Hz"),
                    );
                }
            })
            .response
            .on_hover_text("Sample rate to ask the audio device for");
        if self.session.requested_rate != before {
            self.session.restart_audio();
        }
    }

    /// Moves the current project to the top of File → Open Recent whenever
    /// it's opened or saved under a new name.
    fn remember_recent(&mut self) {
        if self.session.path == self.last_path {
            return;
        }
        self.last_path = self.session.path.clone();
        if let Some(path) = &self.session.path {
            self.recent.retain(|p| p != path);
            self.recent.insert(0, path.clone());
            self.recent.truncate(MAX_RECENT);
        }
    }

    /// Every shortcut and mouse gesture (Help → Keyboard Shortcuts, F1):
    /// an overlay in the middle of the window, in two columns when there's
    /// room, with a filter for finding one.
    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        let filter_id = egui::Id::new("shortcuts filter");
        let screen = ctx.content_rect();
        let columns = if screen.width() >= 1100.0 { 2 } else { 1 };
        let width = if columns == 2 {
            (screen.width() - 160.0).min(980.0)
        } else {
            (screen.width() - 2.0 * space::XL).min(560.0)
        };
        let height = (screen.height() - 160.0).max(240.0);
        // The filter takes the keys when it opens; Escape (or F1) closes it.
        let seen_id = filter_id.with("seen");
        if !self.shortcuts_open {
            ctx.data_mut(|d| d.remove::<bool>(seen_id));
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.shortcuts_open = false;
            return;
        }
        let seen = ctx.data(|d| d.get_temp::<bool>(seen_id).unwrap_or(false));
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut self.shortcuts_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([width, height])
            .show(ctx, |ui| {
                let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut filter)
                            .hint_text("Filter: a key or what it does…")
                            .desired_width(260.0),
                    );
                    // Ready to type into as soon as it opens.
                    if !seen {
                        field.request_focus();
                    }
                    if !filter.is_empty() && ui.small_button("Clear").clicked() {
                        filter.clear();
                    }
                });
                ui.data_mut(|d| {
                    d.insert_temp(filter_id, filter.clone());
                    d.insert_temp(seen_id, true);
                });
                let needle = filter.to_lowercase();
                let sections: Vec<ShortcutSection<'_>> = SHORTCUTS
                    .iter()
                    .map(|(section, rows)| {
                        let rows = rows
                            .iter()
                            .filter(|(keys, action)| {
                                needle.is_empty()
                                    || keys.to_lowercase().contains(&needle)
                                    || action.to_lowercase().contains(&needle)
                                    || section.to_lowercase().contains(&needle)
                            })
                            .collect();
                        (*section, rows)
                    })
                    .filter(|(_, rows): &(&str, Vec<_>)| !rows.is_empty())
                    .collect();
                ui.add_space(space::S);
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if sections.is_empty() {
                            ui.add_space(space::XL);
                            ui.vertical_centered(|ui| ui.weak("Nothing matches"));
                            return;
                        }
                        // Sections go to whichever column is shorter so far.
                        let mut picks = vec![Vec::new(); columns];
                        let mut lengths = vec![0usize; columns];
                        for section in &sections {
                            let column = (0..columns).min_by_key(|c| lengths[*c]).unwrap_or(0);
                            lengths[column] += section.1.len() + 2;
                            picks[column].push(section);
                        }
                        ui.columns(columns, |uis| {
                            for (n, (ui, picked)) in uis.iter_mut().zip(&picks).enumerate() {
                                shortcut_column(ui, n, picked);
                            }
                        });
                    });
            });
    }

    fn unsaved_changes_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending.clone() else {
            return;
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("unsaved")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("Save changes?");
            ui.label(format!(
                "“{}” has unsaved changes.",
                self.session.display_name()
            ));
            ui.add_space(space::M);
            let buttons = chrome::dialog_buttons(ui, "Save", true, Some("Don't Save"), "Cancel");
            if buttons.cancel.clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                choice = Some(Choice::Cancel);
            }
            if buttons.secondary.is_some_and(|b| b.clicked()) {
                choice = Some(Choice::Discard);
            }
            if buttons.primary.clicked() {
                choice = Some(Choice::Save);
            }
        });
        match choice {
            Some(Choice::Save) => {
                self.pending = None;
                if self.save() {
                    self.run(pending);
                }
            }
            Some(Choice::Discard) => {
                self.pending = None;
                self.run(pending);
            }
            Some(Choice::Cancel) => self.pending = None,
            None => {}
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let dirty = if self.session.is_dirty() { "•" } else { "" };
        let title = format!("{dirty}{} — canopy", self.session.display_name());
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "linux")]
        if let Some(drops) = &self.drops {
            for event in drops.poll().collect::<Vec<_>>() {
                use crate::wayland_drop::DropEvent;
                if debug_drops() {
                    eprintln!("canopy: {event:?}");
                }
                match event {
                    DropEvent::Hover(pos) => self.timeline.external_hover = Some(pos),
                    DropEvent::Leave => self.timeline.external_hover = None,
                    DropEvent::Dropped { pos, paths } => {
                        self.timeline.external_hover = None;
                        // On an oscillator's display: its own wavetable.
                        if !crate::devices::wavetable_drop(
                            ctx,
                            &mut self.session,
                            Some(pos),
                            &paths,
                        ) {
                            self.timeline.drop_files(paths, Some(pos), &self.session);
                        }
                    }
                }
            }
        }
        // winit doesn't say where files are on Windows; ask Windows.
        #[cfg(windows)]
        crate::windows_drop::update_hover(ctx, &mut self.timeline.external_hover);
        let (files, pos) = ctx.input(|i| (i.raw.dropped_files.clone(), i.pointer.hover_pos()));
        #[cfg(windows)]
        let pos = crate::windows_drop::cursor_pos(ctx).or(pos);
        let paths: Vec<PathBuf> = files.iter().map(|f| f.path().to_owned()).collect();
        if paths.is_empty() {
            return;
        }
        if crate::devices::wavetable_drop(ctx, &mut self.session, pos, &paths) {
            return;
        }
        self.timeline.drop_files(paths, pos, &self.session);
    }

    /// Clicks decide where keyboard shortcuts go: the piano roll or the timeline.
    fn track_focus(&mut self, ctx: &egui::Context) {
        if let Some(pos) =
            ctx.input(|i| i.pointer.press_origin().filter(|_| i.pointer.any_pressed()))
        {
            // A menu or window over the panels keeps the focus where it was:
            // picking Copy from an effect's menu leaves Ctrl+V to the effects.
            if ctx
                .layer_id_at(pos)
                .is_some_and(|layer| layer.order != egui::Order::Background)
            {
                return;
            }
            self.piano_roll.focused = self.detail_open
                && self.detail == Detail::Clip
                && !self.audio_clip_open()
                && self.piano_roll.contains(pos);
            self.devices_focused =
                self.detail_open && self.shows_devices() && self.device_rect.contains(pos);
        }
    }
}

/// The space between groups of controls in a bar, with a faint divider.
fn group_gap(ui: &mut egui::Ui) {
    ui.add_space(space::S);
    ui.separator();
    ui.add_space(space::S);
}

/// A round record button, red when armed. Painted, since the UI font has
/// no circle glyph.
fn record_button(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(size::CONTROL + 10.0, size::CONTROL),
        egui::Sense::click(),
    );
    let hover = theme::hover(ui, &response);
    let armed = ui
        .ctx()
        .animate_bool_with_time(response.id.with("armed"), on, theme::anim::NORMAL);
    let visuals = ui.visuals();
    let fill = visuals
        .widgets
        .inactive
        .weak_bg_fill
        .lerp_to_gamma(visuals.widgets.hovered.weak_bg_fill, hover);
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(theme::radius::CONTROL), fill);
    let dot = visuals
        .weak_text_color()
        .lerp_to_gamma(color::RECORD, armed);
    painter.circle_filled(rect.center(), 5.0, dot);
    if armed > 0.0 {
        painter.circle_stroke(
            rect.center(),
            7.0,
            egui::Stroke::new(1.0, color::RECORD.gamma_multiply(0.5 * armed)),
        );
    }
    chrome::focus_ring(ui, &response, rect);
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, "Record"));
    response
}

/// The MIDI light needs a frame after it goes out, too.
fn ctx_repaint_soon(ui: &egui::Ui) {
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(160));
}

/// One column of the Keyboard Shortcuts window: each section's name, then
/// its shortcuts (in key caps) beside what they do. One grid for the whole
/// column, so what they do lines up from section to section.
fn shortcut_column(ui: &mut egui::Ui, column: usize, sections: &[&ShortcutSection<'_>]) {
    ui.add_space(space::S);
    egui::Grid::new(("shortcuts", column))
        .num_columns(2)
        .min_col_width(140.0)
        .spacing([space::L, space::S])
        .show(ui, |ui| {
            for (n, (section, rows)) in sections.iter().enumerate() {
                if n > 0 {
                    // A short blank row between sections.
                    ui.allocate_exact_size(egui::vec2(1.0, space::S), egui::Sense::hover());
                    ui.end_row();
                }
                ui.label(
                    RichText::new(*section)
                        .strong()
                        .color(theme::accent(ui.visuals())),
                );
                ui.end_row();
                for (keys, action) in rows {
                    ui.horizontal(|ui| {
                        if is_key_combo(keys) {
                            chrome::keycaps(ui, keys);
                        } else {
                            ui.label(RichText::new(*keys).strong());
                        }
                    });
                    ui.add(egui::Label::new(*action).wrap());
                    ui.end_row();
                }
            }
        });
    ui.add_space(space::M);
}

/// A section of the Keyboard Shortcuts window: its name and the rows the
/// filter left.
type ShortcutSection<'a> = (&'a str, Vec<&'a (&'a str, &'a str)>);

/// Whether a shortcut window entry is keys alone ("Ctrl+Shift+Z",
/// "Left / Right"), drawn as key caps, rather than a gesture.
fn is_key_combo(keys: &str) -> bool {
    keys.split(" / ")
        .flat_map(|alternative| alternative.split('+'))
        .all(|key| {
            !key.is_empty()
                && !key.contains(' ')
                && key
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_uppercase() || c.is_ascii_digit())
        })
}

/// What the Keyboard Shortcuts window lists, by section.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Transport",
        &[
            ("Space", "Play / stop (stop twice: back to the start)"),
            ("R", "Record from a MIDI keyboard"),
            ("Ctrl+M", "Metronome"),
            ("Home", "Playhead to the start"),
            ("Click ruler", "Move the playhead"),
            ("L", "Loop on / off"),
            (
                "Shift+drag ruler",
                "Set the loop (drag its ends to change it)",
            ),
            (
                "Drag the ruler's top",
                "Set the loop; drag the loop to move it",
            ),
            ("Shift+L", "Loop the selected clips (or notes)"),
        ],
    ),
    (
        "Arrangement",
        &[
            ("Double-click lane", "New MIDI clip"),
            ("Double-click clip", "Open it in the Clip tab"),
            (
                "Drag clip / edge",
                "Move / trim (Alt while dragging: no snap)",
            ),
            ("Ctrl+C / Ctrl+V", "Copy / paste clips at the marker"),
            ("Ctrl+D", "Duplicate"),
            ("Ctrl+E", "Split at the marker"),
            ("0", "Mute / unmute clips"),
            ("Right-click MIDI clip", "Freeze to audio"),
            ("Left / Right", "Nudge by the grid"),
            ("Delete", "Delete clips, or tracks if no clips are selected"),
            ("Ctrl+A / Esc", "Select all / none"),
        ],
    ),
    (
        "MIDI",
        &[
            ("Select a MIDI track", "Your keyboard plays it"),
            (
                "Right-click a control",
                "Link to Controller (move a knob to link)",
            ),
            ("MIDI menu", "Switch devices on and off"),
        ],
    ),
    (
        "Audio clips",
        &[
            ("Drag top corners", "Fade in / out (middle dot: bend)"),
            ("Drag top bar", "Clip volume"),
            ("Shift+drag edge", "Stretch (Resample: pitch follows)"),
            ("Drag end past the file", "Loop"),
            ("Alt+drag", "Slip the audio inside the clip"),
            ("Drop over a clip's edge", "Crossfade"),
            ("Right-click", "Reverse, normalize, chop, consolidate…"),
        ],
    ),
    (
        "Tracks",
        &[
            ("Ctrl+T / Ctrl+Shift+T", "New audio / MIDI track"),
            ("Ctrl+G / Ctrl+Shift+G", "Group / ungroup"),
            ("Drag header", "Reorder, or drop onto a group"),
            ("Double-click name", "Rename"),
        ],
    ),
    (
        "Piano roll",
        &[
            ("Double-click", "Add or remove a note"),
            ("Right-click", "Delete a note"),
            ("Drag note / end", "Move / resize"),
            ("Up / Down", "Transpose (Ctrl or Shift: octave)"),
            ("Ctrl+C / Ctrl+V", "Copy / paste notes"),
            ("Chord box", "Double-click writes a chord in the scale"),
            ("Ctrl+Q / Alt+Q", "Quantize: now / with settings"),
            ("Alt+R / Alt+U", "Naturalize / chop"),
            ("Alt+A / Alt+S", "Arpeggiate / strum"),
            ("Alt+P", "Strum pattern: held chords in a rhythm"),
            ("Ctrl+L / Alt+Y", "Legato / flip"),
            ("L", "Loop the clip (or the loop over it)"),
            ("Z", "Zoom to the selected notes, or the clip"),
            ("Ruler", "The same ruler and loop as the arrangement"),
        ],
    ),
    (
        "Devices",
        &[
            ("Knob: drag / wheel", "Turn it (Shift or Ctrl: fine)"),
            (
                "Focused knob: arrows / Home / End",
                "Turn it (Shift: fine) / to either end",
            ),
            ("Knob: double-click", "Type a value"),
            ("Knob: Ctrl+click", "Reset it"),
            ("Knob: right-click", "Automate it, link a controller"),
            ("+ Effect (device view)", "Add one to the track"),
            ("Master (above its meter)", "The master's effects"),
            (
                "Click an effect's title bar",
                "Select it (Ctrl, Shift: more)",
            ),
            ("Drag the title bar", "Move it in the chain"),
            (
                "… onto a track's header / Master",
                "Move it there (Ctrl: copy it)",
            ),
            ("Double-click it, or its arrow", "Fold it to its bar"),
            (
                "Right-click it, or its menu",
                "Presets, duplicate, move to…",
            ),
            ("Delete", "Remove the selected effects"),
            ("Ctrl+D", "Duplicate them"),
            ("Ctrl+C / X / V", "Copy, cut, paste (after them)"),
            ("Left / Right", "Select the next effect"),
            ("Ctrl+Left / Right", "Move them along the chain"),
            ("Presets (by the instrument)", "Save, load, defaults"),
            (
                "Sidechain (beside the name)",
                "Pick the track it listens to",
            ),
            ("EQ: double-click the curve", "Add a band there"),
            ("EQ handle: drag / wheel", "Frequency and gain / Q"),
            ("EQ handle: right-click", "Type, dynamic, remove"),
            ("Displays: drag", "Threshold, decay, echoes, shapes…"),
            ("Shaper: right-click the shape", "Ready-made shapes"),
            ("Analyzer: Pop Out", "Its own window"),
            ("Drum pads, sampler keys", "Click to play"),
        ],
    ),
    (
        "Automation",
        &[
            ("A, or right-click a control", "Show a lane / Automate it"),
            (
                "Double-click lane",
                "Add a point (double-click it to remove)",
            ),
            ("Drag a segment's dot", "Bend it (double-click: straighten)"),
            ("B, or Ctrl+drag", "Draw points"),
            ("Click / drag a box", "Select points (Shift, Ctrl: add)"),
            ("Drag a selected point", "Move them all (Del deletes)"),
            (
                "Right-click lane",
                "Generate shapes or random, invert, reverse",
            ),
        ],
    ),
    (
        "View",
        &[
            ("Ctrl+wheel", "Zoom time"),
            ("Alt+wheel", "Zoom track or key height"),
            ("Shift+wheel", "Scroll sideways"),
            ("Z / Shift+Z", "Zoom to the selected clips / the song"),
            ("Ctrl+4", "Snap on / off"),
            ("Ctrl+1 / Ctrl+2", "Finer / coarser grid"),
            ("Shift+Tab", "Clip / device view"),
            ("Tab", "Move the keyboard to the next control"),
            ("F1", "This window"),
        ],
    ),
    (
        "Files",
        &[
            ("Ctrl+N / Ctrl+O", "New / open"),
            ("Ctrl+R", "Export audio (right-click a track: export it)"),
            ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
            ("Ctrl+I", "Import audio (or drop files onto a lane)"),
            ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
        ],
    ),
];

#[derive(Clone, Copy)]
enum Choice {
    Save,
    Discard,
    Cancel,
}

/// `CANOPY_DEBUG_DROPS=1` logs drag-and-drop events and status messages.
pub fn debug_drops() -> bool {
    std::env::var_os("CANOPY_DEBUG_DROPS").is_some()
}

impl eframe::App for CanopyApp {
    fn on_exit(&mut self) {
        // Stop listening for drops while the window's connection still exists.
        #[cfg(target_os = "linux")]
        {
            self.drops = None;
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(SAMPLE_RATE_KEY, self.session.requested_rate.to_string());
        let recent: Vec<_> = self
            .recent
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        storage.set_string(RECENT_KEY, recent.join("\n"));
        let mut off: Vec<_> = self.session.midi.disabled.iter().cloned().collect();
        off.sort();
        storage.set_string(MIDI_OFF_KEY, off.join("\n"));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.styled {
            theme::apply(&ctx);
            self.styled = true;
        }
        crate::plugins::pump(&ctx, &mut self.session);
        self.session.update();
        self.timeline.prune(&self.session.project);
        self.timeline.controls = crate::timeline::Controls {
            links: self.session.project.controller_links.clone(),
            learning: self.session.learning().cloned(),
        };
        self.follow_selection_with_keyboard();
        if self.timeline.open_requested {
            self.timeline.open_requested = false;
            self.detail = Detail::Clip;
            self.detail_open = true;
        }
        self.track_focus(&ctx);
        self.handle_dropped_files(&ctx);
        if self.pending.is_none() && self.export_dialog.is_none() && self.flp_import.is_none() {
            self.handle_shortcuts(&ctx);
        }

        let peaks = self.session.take_master_peaks();
        let dt = ui.input(|i| i.stable_dt);
        self.meter.update(peaks, dt);
        // Falls 50% a second, so a spike stays readable.
        self.dsp_load = self
            .session
            .take_load()
            .max(self.dsp_load - 0.5 * dt)
            .max(0.0);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        let bar = |ui: &egui::Ui| {
            egui::Frame::side_top_panel(ui.style())
                .inner_margin(egui::Margin::symmetric(space::M as i8, space::S as i8))
        };
        egui::Panel::top("transport")
            .frame(bar(ui))
            .show(ui, |ui| self.transport_bar(ui));
        egui::Panel::bottom("status")
            .frame(bar(ui).inner_margin(egui::Margin::symmetric(space::M as i8, space::XS as i8)))
            .show(ui, |ui| self.status_bar(ui));
        // Slides open and shut; two fifths of the window to start with, then
        // as tall as it was left (egui keeps panel sizes between runs).
        let height = ctx.content_rect().height();
        let mut open = self.detail_open;
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size((height * 0.4).clamp(260.0, 460.0))
            .min_size(140.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(0))
            .show_collapsible(ui, &mut open, |ui| self.detail_panel(ui));
        self.detail_open = open;
        egui::Panel::right("meter")
            .resizable(false)
            .exact_size(84.0)
            .show(ui, |ui| self.master_panel(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.timeline.show(ui, &mut self.session));

        crate::devices::analyzer_windows(&ctx, &mut self.session, &mut self.timeline);
        crate::patcher::windows(&ctx, &mut self.session, &mut self.timeline);
        crate::plugins::windows(&ctx, &mut self.session, &mut self.timeline);
        crate::devices::preset_menu::save_window(&ctx, &mut self.session);
        self.shortcuts_window(&ctx);
        self.export_window(&ctx);
        self.flp_import_window(&ctx);
        self.unsaved_changes_dialog(&ctx);
        chrome::paint_keyboard_focus(&ctx);
        chrome::finish_drag_label(&ctx);
        self.remember_recent();
        self.update_title(&ctx);
        self.session.flush();

        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.quitting && self.session.is_dirty() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
        if self.quitting {
            self.quitting = false;
            self.session.mark_saved_for_quit();
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }

        // egui only repaints on input by default. Keep frames coming while
        // there's audio to show or a meter still falling.
        if self.session.is_playing() || self.meter.is_active() {
            ctx.request_repaint();
        }
    }
}

/// Whether this frame has a copy, cut or paste request. egui-winit turns
/// Ctrl+C/X/V into these events rather than key presses.
pub fn clipboard_events(ctx: &egui::Context) -> (bool, bool, bool) {
    ctx.input(|i| {
        let has = |f: fn(&egui::Event) -> bool| i.events.iter().any(f);
        (
            has(|e| matches!(e, egui::Event::Copy)),
            has(|e| matches!(e, egui::Event::Cut)),
            has(|e| matches!(e, egui::Event::Paste(_))),
        )
    })
}

fn format_time(seconds: f64) -> String {
    let millis = (seconds * 1000.0) as u64;
    format!(
        "{:02}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format() {
        assert_eq!(format_time(0.0), "00:00.000");
        assert_eq!(format_time(61.5), "01:01.500");
    }
}
