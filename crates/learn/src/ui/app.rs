//! The window: menus, the view tabs, instrument and volume, the piano along
//! the bottom, the status bar and the shortcuts window (F1).
//!
//! Each frame `logic` takes in what was played (MIDI, the on-screen keys,
//! the typing keyboard) in the order it happened, hands it to the open
//! view, and moves time on; `ui` draws.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, Key, Modifiers, RichText, Ui};
use serde::{Deserialize, Serialize};

use super::drill_view::DrillView;
use super::free_view::FreeView;
use super::piano::{self, KeyEvent, Layout, Look};
use super::reading_view::ReadingView;
use super::song_view::{SongSettings, SongView};
use super::stats_view::StatsView;
use super::theme;
use crate::audio::Audio;
use crate::clock::Clock;
use crate::devices::{Input, MidiDevices};
use crate::progress::{self, Kind, Progress};
use crate::rng::Rng;
use crate::theory::KeySig;
use crate::typing::Typing;
use crate::{drill, reading};

/// What every view shares: sound, MIDI, the clock, what's held down.
pub struct Shared {
    pub clock: Clock,
    pub audio: Audio,
    pub devices: MidiDevices,
    pub progress: Progress,
    /// Keys down now, from any keyboard.
    pub held: [bool; 128],
    pub pedal: bool,
    /// Keys lit for a moment, right or wrong: (pitch, color, until).
    flashes: Vec<(u8, Color32, f64)>,
    /// When a note was last played: practice time counts while recent.
    pub last_input: Option<f64>,
    pub message: Option<Message>,
}

pub struct Message {
    pub text: String,
    pub error: bool,
    pub at: f64,
}

/// How long a key stays lit after a right or wrong note.
const FLASH: f64 = 0.35;
const MESSAGE_TIME: f64 = 6.0;
/// Practice time stops counting this long after the last note.
const IDLE_AFTER: f64 = 10.0;

impl Shared {
    pub fn now(&self) -> f64 {
        self.clock.now()
    }

    pub fn flash(&mut self, pitch: u8, color: Color32) {
        let until = self.now() + FLASH;
        self.flashes.retain(|f| f.0 != pitch);
        self.flashes.push((pitch, color, until));
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
            at: self.now(),
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
            at: self.now(),
        });
    }

    pub fn held_keys(&self) -> Vec<u8> {
        (0..128u8).filter(|p| self.held[usize::from(*p)]).collect()
    }

    fn active(&self, now: f64) -> bool {
        self.last_input.is_some_and(|t| now - t < IDLE_AFTER)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum View {
    #[default]
    Free,
    Reading,
    Drill,
    Songs,
    Stats,
}

impl View {
    pub const ALL: [View; 5] = [
        View::Free,
        View::Reading,
        View::Drill,
        View::Songs,
        View::Stats,
    ];

    pub fn label(self) -> &'static str {
        match self {
            View::Free => "Keyboard",
            View::Reading => "Note reading",
            View::Drill => "Scales & chords",
            View::Songs => "Songs",
            View::Stats => "Progress",
        }
    }

    fn key(self) -> Key {
        match self {
            View::Free => Key::Num1,
            View::Reading => Key::Num2,
            View::Drill => Key::Num3,
            View::Songs => Key::Num4,
            View::Stats => Key::Num5,
        }
    }
}

/// Settings kept between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    view: View,
    instrument: String,
    volume_db: f32,
    sfz: Option<PathBuf>,
    typing: bool,
    typing_octave: i8,
    keys: piano::Size,
    names: bool,
    midi_off: Vec<String>,
    reading: reading::Settings,
    drill: drill::Settings,
    song: SongSettings,
    recent: Vec<PathBuf>,
    free_key: i8,
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            view: View::Free,
            instrument: instruments::DEFAULT_INSTRUMENT.to_owned(),
            volume_db: -6.0,
            sfz: None,
            typing: true,
            typing_octave: 3,
            keys: piano::Size::Fit,
            names: false,
            midi_off: Vec::new(),
            reading: reading::Settings::default(),
            drill: drill::Settings::default(),
            song: SongSettings::default(),
            recent: Vec::new(),
            free_key: 0,
        }
    }
}

const SETTINGS_KEY: &str = "settings";
const PROGRESS_KEY: &str = "progress";

pub struct LearnApp {
    pub shared: Shared,
    pub typing: Typing,
    /// The computer keyboard plays notes.
    pub typing_on: bool,
    pub view: View,
    pub free: FreeView,
    pub reading: ReadingView,
    pub drill: DrillView,
    pub songs: SongView,
    stats: StatsView,
    pub keys: piano::Size,
    /// Letter names on every white key.
    pub names: bool,
    pub help_open: bool,
    last_logic: Option<f64>,
    /// Where the piano was last drawn.
    pub layout: Option<Layout>,
}

impl LearnApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> LearnApp {
        let ctx = cc.egui_ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let clock = Clock::real();
        let audio = Audio::start(wake.clone());
        let devices = MidiDevices::new(clock.clone(), wake);
        let saved = cc
            .storage
            .and_then(|s| s.get_string(SETTINGS_KEY))
            .and_then(|text| ron::from_str::<Saved>(&text).ok())
            .unwrap_or_default();
        let progress = cc
            .storage
            .and_then(|s| s.get_string(PROGRESS_KEY))
            .map(|text| Progress::from_ron(&text))
            .unwrap_or_default();
        let mut app = LearnApp::build(clock, audio, devices, progress, Rng::from_time());
        app.restore(saved);
        app
    }

    /// The app on an engine the caller runs, a clock the caller moves and
    /// no MIDI devices: for tests.
    pub fn for_tests(handle: engine::EngineHandle, clock: Clock, seed: u64) -> LearnApp {
        let audio = Audio::with_engine(handle);
        let devices = MidiDevices::off(clock.clone(), Arc::new(|| {}));
        let mut app = LearnApp::build(clock, audio, devices, Progress::default(), Rng::new(seed));
        app.shared.audio.set_volume(-6.0);
        app
    }

    fn build(
        clock: Clock,
        audio: Audio,
        devices: MidiDevices,
        progress: Progress,
        mut rng: Rng,
    ) -> LearnApp {
        devices.set_live(audio.live_sender());
        let now = clock.now();
        let reading = ReadingView::new(reading::Settings::default(), Rng::new(rng.next_u64()), now);
        let drill = DrillView::new(drill::Settings::default(), Rng::new(rng.next_u64()));
        LearnApp {
            shared: Shared {
                clock,
                audio,
                devices,
                progress,
                held: [false; 128],
                pedal: false,
                flashes: Vec::new(),
                last_input: None,
                message: None,
            },
            typing: Typing::default(),
            typing_on: true,
            view: View::Free,
            free: FreeView { key: KeySig::C },
            reading,
            drill,
            songs: SongView::new(SongSettings::default()),
            stats: StatsView::default(),
            keys: piano::Size::Fit,
            names: false,
            help_open: false,
            last_logic: None,
            layout: None,
        }
    }

    fn restore(&mut self, saved: Saved) {
        let now = self.shared.now();
        self.view = saved.view;
        self.shared.audio.set_instrument(&saved.instrument);
        self.shared.audio.set_volume(saved.volume_db);
        if let Some(sfz) = saved.sfz {
            self.shared.audio.load_sfz(sfz);
        }
        self.typing_on = saved.typing;
        self.typing.octave = saved.typing_octave.clamp(0, 7);
        self.keys = saved.keys;
        self.names = saved.names;
        self.shared.devices.disabled = saved.midi_off.into_iter().collect();
        self.reading.set_settings(saved.reading, now);
        self.drill.set_settings(saved.drill, &mut self.shared);
        self.songs.settings = saved.song;
        self.songs.recent = saved.recent;
        self.free.key = KeySig(saved.free_key.clamp(-7, 7));
    }

    fn saved(&self) -> Saved {
        let mut midi_off: Vec<String> = self.shared.devices.disabled.iter().cloned().collect();
        midi_off.sort();
        Saved {
            view: self.view,
            instrument: self.shared.audio.instrument().to_owned(),
            volume_db: self.shared.audio.volume_db(),
            sfz: self.shared.audio.bank_path().map(|p| p.to_owned()),
            typing: self.typing_on,
            typing_octave: self.typing.octave,
            keys: self.keys,
            names: self.names,
            midi_off,
            reading: self.reading.settings().clone(),
            drill: self.drill.settings().clone(),
            song: self.songs.settings.clone(),
            recent: self.songs.recent.clone(),
            free_key: self.free.key.0,
        }
    }

    pub fn set_view(&mut self, view: View) {
        if view == self.view {
            return;
        }
        // Leaving a view stops what it was playing.
        match self.view {
            View::Songs => self.songs.pause(&mut self.shared),
            View::Drill => self.drill.stop(&mut self.shared),
            _ => {}
        }
        self.view = view;
    }

    /// Takes in what was played, and moves time on. Runs every frame, and
    /// while the window is hidden if a repaint was asked for.
    fn step(&mut self, ctx: &egui::Context) {
        self.shared.audio.update();
        self.shared.devices.update();
        self.typing_keys(ctx);
        let mut inputs: Vec<Input> =
            std::iter::from_fn(|| self.shared.devices.try_recv()).collect();
        inputs.sort_by(|a, b| a.time().total_cmp(&b.time()));
        for input in inputs {
            self.input(input);
        }
        let now = self.shared.now();
        match self.view {
            View::Songs => self.songs.update(&mut self.shared, now),
            View::Drill => self.drill.update(&mut self.shared, now),
            _ => {}
        }
        self.shared.flashes.retain(|f| f.2 > now);
        self.count_practice(now);
        let animating = !self.shared.flashes.is_empty()
            || self.songs.is_playing()
            || self.drill.animating(now)
            || self.reading.animating(now)
            || self.shared.devices.active();
        if animating {
            ctx.request_repaint();
        }
    }

    fn input(&mut self, input: Input) {
        match input {
            Input::Note {
                pitch, on, time, ..
            } => {
                self.shared.held[usize::from(pitch)] = on;
                if !on {
                    return;
                }
                self.shared.last_input = Some(time);
                match self.view {
                    View::Reading => self.reading.on_note(&mut self.shared, pitch, time),
                    View::Drill => self.drill.on_note(&mut self.shared, pitch, time),
                    View::Songs => self.songs.on_note(&mut self.shared, pitch, time),
                    View::Free | View::Stats => {}
                }
            }
            Input::Pedal { down, .. } => self.shared.pedal = down,
        }
    }

    /// The letter rows play notes, unless text is being typed.
    fn typing_keys(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        if !focused || !self.typing_on {
            for pitch in self.typing.release_all() {
                self.shared.devices.play(pitch, false, 0);
            }
            return;
        }
        if ctx.text_edit_focused() {
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let key = physical_key.unwrap_or(key);
            if pressed && (modifiers.ctrl || modifiers.alt || modifiers.command) {
                continue;
            }
            match key {
                Key::Minus if pressed && !repeat => self.typing.shift(-1),
                Key::Equals if pressed && !repeat => self.typing.shift(1),
                _ => {
                    if let Some((pitch, on)) = self.typing.key(key, pressed, repeat) {
                        self.shared.devices.play(pitch, on, 100);
                    }
                }
            }
        }
    }

    fn count_practice(&mut self, now: f64) {
        let dt = self.last_logic.map_or(0.0, |t| (now - t).clamp(0.0, 0.5));
        self.last_logic = Some(now);
        let active = self.shared.active(now);
        let activity = match self.view {
            View::Songs => self.songs.practising().map(|name| (Kind::Song, name)),
            View::Reading if active => Some((Kind::Exercise, self.reading.name())),
            View::Drill if active || self.drill.running() => {
                Some((Kind::Exercise, self.drill.name()))
            }
            View::Free if active => Some((Kind::Exercise, "Free play".to_owned())),
            _ => None,
        };
        if let Some((kind, name)) = activity
            && dt > 0.0
        {
            let date = progress::date_label(progress::today());
            self.shared.progress.practise(kind, &name, dt, &date);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }
        let shortcut = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if shortcut(Modifiers::NONE, Key::F1) {
            self.help_open = !self.help_open;
        }
        for view in View::ALL {
            if shortcut(Modifiers::COMMAND, view.key()) {
                self.set_view(view);
            }
        }
        if shortcut(Modifiers::COMMAND, Key::O) {
            self.set_view(View::Songs);
            self.songs.open_dialog(&mut self.shared);
        }
        let now = self.shared.now();
        match self.view {
            View::Reading => self.reading.shortcuts(ctx, now),
            View::Drill => self.drill.shortcuts(ctx, &mut self.shared),
            View::Songs => self.songs.shortcuts(ctx, &mut self.shared),
            _ => {}
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open MIDI File…\tCtrl+O").clicked() {
                    self.set_view(View::Songs);
                    self.songs.open_dialog(&mut self.shared);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("MIDI", |ui| self.midi_menu(ui));
            ui.menu_button("View", |ui| {
                for size in piano::Size::ALL {
                    ui.radio_value(&mut self.keys, size, size.label());
                }
                ui.separator();
                ui.checkbox(&mut self.names, "Note names on keys");
                ui.checkbox(&mut self.typing_on, "Computer keyboard plays notes");
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard Shortcuts\tF1").clicked() {
                    self.help_open = true;
                }
            });
        });
    }

    fn midi_menu(&mut self, ui: &mut Ui) {
        let ports: Vec<(String, bool)> = self
            .shared
            .devices
            .ports()
            .map(|(name, open)| (name.to_owned(), open))
            .collect();
        if ports.is_empty() {
            ui.weak("No MIDI devices");
        }
        for (name, _) in ports {
            let mut on = !self.shared.devices.disabled.contains(&name);
            if ui.checkbox(&mut on, &name).changed() {
                self.shared.devices.set_enabled(&name, on);
            }
        }
        if let Some(error) = &self.shared.devices.error {
            ui.separator();
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for view in View::ALL {
                let shortcut = format!(
                    "Ctrl+{}",
                    View::ALL.iter().position(|v| *v == view).unwrap_or(0) + 1
                );
                if ui
                    .selectable_label(self.view == view, view.label())
                    .on_hover_text(shortcut)
                    .clicked()
                {
                    self.set_view(view);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let typing = ui.selectable_label(self.typing_on, "⌨").on_hover_text(
                    "The computer keyboard plays notes: Z and Q rows, - and = shift octaves",
                );
                if typing.clicked() {
                    self.typing_on = !self.typing_on;
                }
                let mut db = self.shared.audio.volume_db();
                let volume = ui
                    .add(
                        egui::DragValue::new(&mut db)
                            .range(project::SILENCE_DB..=project::MAX_VOLUME_DB)
                            .speed(0.25)
                            .update_while_editing(false)
                            .fixed_decimals(1)
                            .custom_formatter(|v, _| {
                                if v as f32 <= project::SILENCE_DB {
                                    "-inf dB".to_owned()
                                } else {
                                    format!("{v:.1} dB")
                                }
                            }),
                    )
                    .on_hover_text("Volume");
                if volume.changed() {
                    self.shared.audio.set_volume(db);
                }
                self.instrument_picker(ui);
            });
        });
    }

    fn instrument_picker(&mut self, ui: &mut Ui) {
        if self.shared.audio.needs_file() {
            let label = match self.shared.audio.bank_state() {
                None => "Load .sfz…".to_owned(),
                Some((name, Ok(true))) => name,
                Some((name, Ok(false))) => format!("{name} (loading)"),
                Some((name, Err(_))) => format!("{name} (failed)"),
            };
            let mut button = ui.button(label);
            if let Some((_, Err(err))) = self.shared.audio.bank_state() {
                button = button.on_hover_text(err);
            }
            if button.clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Load an SFZ instrument")
                    .add_filter("SFZ", &["sfz"])
                    .pick_file()
            {
                self.shared.audio.load_sfz(path);
            }
        }
        let current = self.shared.audio.instrument().to_owned();
        let name = instruments::find_builtin(&current).map_or("?", |d| d.name);
        let mut picked = None;
        egui::ComboBox::from_id_salt("instrument")
            .selected_text(name)
            .width(140.0)
            .show_ui(ui, |ui| {
                for descriptor in instruments::builtins() {
                    if ui
                        .selectable_label(descriptor.id == current, descriptor.name)
                        .on_hover_text(descriptor.description)
                        .clicked()
                    {
                        picked = Some(descriptor.id);
                    }
                }
            })
            .response
            .on_hover_text("Instrument");
        if let Some(id) = picked {
            self.shared.audio.set_instrument(id);
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let now = self.shared.now();
        ui.horizontal(|ui| {
            match self.shared.audio.status() {
                Ok(text) => {
                    ui.weak(text);
                }
                Err(err) => {
                    ui.colored_label(ui.visuals().error_fg_color, format!("No audio: {err}"));
                }
            }
            ui.separator();
            let devices = self.shared.devices.open_count();
            let size = egui::vec2(10.0, 10.0);
            let (dot, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let light = if self.shared.devices.active() {
                Color32::from_rgb(90, 200, 120)
            } else {
                ui.visuals().weak_text_color().gamma_multiply(0.5)
            };
            ui.painter().circle_filled(dot.center(), 4.0, light);
            ui.label(match devices {
                0 => "No MIDI keyboard".to_owned(),
                1 => "1 MIDI keyboard".to_owned(),
                n => format!("{n} MIDI keyboards"),
            });
            if self.shared.pedal {
                ui.label(RichText::new("Ped.").strong().italics());
            }
            if self.typing_on {
                ui.separator();
                let (low, _) = self.typing.span();
                ui.weak(format!("⌨ Z = {}", crate::theory::pitch_label(low)))
                    .on_hover_text("- and = shift the typing keyboard an octave");
            }
            if let Some(message) = &self.shared.message
                && now - message.at < MESSAGE_TIME
            {
                ui.separator();
                if message.error {
                    ui.colored_label(ui.visuals().error_fg_color, &message.text);
                } else {
                    ui.label(&message.text);
                }
            }
        });
    }

    /// The keys as they look this frame: held, flashing, hints from the view.
    fn looks(&self) -> [Look; 128] {
        let mut looks = [Look::default(); 128];
        for (pitch, held) in self.shared.held.iter().enumerate() {
            if *held {
                looks[pitch].fill = Some(theme::HELD);
            }
        }
        match self.view {
            View::Drill => self.drill.looks(&mut looks),
            View::Songs => self.songs.looks(&mut looks),
            _ => {}
        }
        for &(pitch, color, _) in &self.shared.flashes {
            looks[usize::from(pitch)].fill = Some(color);
        }
        looks
    }

    fn piano(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect().shrink2(egui::vec2(6.0, 0.0));
        let rect = egui::Rect::from_min_max(
            rect.min + egui::vec2(0.0, 2.0),
            rect.max - egui::vec2(0.0, 4.0),
        );
        let (low, high) = self.keys.range(rect.width());
        let layout = Layout::new(rect, low, high);
        self.layout = Some(layout);
        let looks = self.looks();
        for event in piano::show(ui, &layout, &looks, self.names) {
            match event {
                KeyEvent::Down { pitch, velocity } => {
                    self.shared.devices.play(pitch, true, velocity)
                }
                KeyEvent::Up(pitch) => self.shared.devices.play(pitch, false, 0),
            }
        }
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut self.help_open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .show(ctx, |ui| {
                // One grid, so the columns line up across sections.
                egui::Grid::new("shortcuts")
                    .num_columns(2)
                    .spacing([24.0, 3.0])
                    .show(ui, |ui| {
                        for (i, (section, rows)) in SHORTCUTS.iter().enumerate() {
                            if i > 0 {
                                ui.end_row();
                            }
                            ui.strong(*section);
                            ui.end_row();
                            for (keys, action) in *rows {
                                ui.monospace(*keys);
                                ui.label(*action);
                                ui.end_row();
                            }
                        }
                    });
            });
    }
}

/// What the Keyboard Shortcuts window lists, by section.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Playing",
        &[
            ("Z S X D C … M", "One octave, like a piano's keys"),
            ("Q 2 W 3 E … P", "The octave above"),
            ("- / =", "Typing keyboard an octave down / up"),
            ("Click the keys", "Play them; slide for more"),
        ],
    ),
    (
        "Views",
        &[
            (
                "Ctrl+1 … Ctrl+5",
                "Keyboard, reading, scales, songs, progress",
            ),
            ("F1", "This window"),
        ],
    ),
    (
        "Note reading",
        &[("Esc", "Skip the note"), ("Enter", "New round")],
    ),
    (
        "Scales & chords",
        &[
            ("Space", "Start with the metronome (count-in)"),
            ("Enter", "Start over"),
        ],
    ),
    (
        "Songs",
        &[
            ("Ctrl+O", "Open a MIDI file"),
            ("Space", "Play / pause"),
            ("Home", "From the top"),
            ("Left / Right", "5 seconds back / on"),
            ("Click the bar", "Go there"),
            ("Shift+drag the bar", "Loop a section"),
            ("Ctrl+[ / Ctrl+]", "Loop from / to here"),
            ("Ctrl+L", "Loop on / off"),
        ],
    ),
];

impl eframe::App for LearnApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(text) = ron::to_string(&self.saved()) {
            storage.set_string(SETTINGS_KEY, text);
        }
        storage.set_string(PROGRESS_KEY, self.shared.progress.to_ron());
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.step(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        let height = (ctx.content_rect().height() * 0.19).clamp(90.0, 150.0);
        egui::Panel::bottom("piano")
            .exact_size(height)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(Color32::from_gray(18)))
            .show(ui, |ui| self.piano(ui));
        let layout = self.layout;
        let margin = if self.view == View::Songs { 0 } else { 10 };
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.global_style()).inner_margin(margin))
            .show(ui, |ui| match self.view {
                View::Free => self.free.ui(ui, &self.shared),
                View::Reading => self.reading.ui(ui, &mut self.shared),
                View::Drill => self.drill.ui(ui, &mut self.shared),
                View::Songs => self.songs.ui(ui, &mut self.shared, layout),
                View::Stats => self.stats.ui(ui, &mut self.shared),
            });
        self.help_window(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_restart() {
        let saved = Saved {
            view: View::Drill,
            volume_db: -12.0,
            sfz: Some(PathBuf::from("/samples/piano.sfz")),
            keys: piano::Size::Keys61,
            midi_off: vec!["Through".to_owned()],
            reading: reading::Settings {
                key: reading::KeyChoice::Fixed(KeySig(-3)),
                ..reading::Settings::default()
            },
            drill: drill::Settings {
                root: crate::theory::NoteName::new(3, 1),
                metronome: true,
                ..drill::Settings::default()
            },
            recent: vec![PathBuf::from("/songs/minuet.mid")],
            ..Saved::default()
        };
        let text = ron::to_string(&saved).unwrap();
        let back: Saved = ron::from_str(&text).unwrap();
        assert_eq!(ron::to_string(&back).unwrap(), text);
        assert_eq!(back.view, View::Drill);
        assert_eq!(back.reading.key, reading::KeyChoice::Fixed(KeySig(-3)));
        // Settings saved by an older version, with fields missing, still load.
        let old: Saved = ron::from_str("(view: Songs, volume_db: -3.0)").unwrap();
        assert_eq!(old.view, View::Songs);
        assert_eq!(old.instrument, instruments::DEFAULT_INSTRUMENT);
    }
}
