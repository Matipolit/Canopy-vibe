//! The piano roll's Tools menu and chord stamp, FL Studio style: quantize,
//! naturalize, chop, arpeggiate, strum and strum patterns (in windows that
//! preview as you change them), legato, glue, fit to scale, reverse and
//! flip.

use eframe::egui::{self, ComboBox, DragValue, Key, Modifiers};
use midi::Tick;
use project::note_tools::patterns::Articulation;
use project::note_tools::{self, ArpPattern, Arpeggio, ChordKind, Naturalize, Quantize, Strum};
use project::{ClipId, ClipRef, EditError, Note, NoteId, Project};

use super::PianoRoll;
use super::pattern::{self, PatternTool};
use crate::session::Session;
use crate::widgets::chrome;

/// What the tools remember between uses.
pub struct Tools {
    /// What a double-click writes: a note, or a chord on it.
    pub chord: ChordKind,
    quantize: Quantize,
    naturalize: Naturalize,
    chop: Tick,
    arpeggio: Arpeggio,
    strum: Strum,
    pattern: PatternTool,
    window: Option<Window>,
}

impl Default for Tools {
    fn default() -> Self {
        Tools {
            chord: ChordKind::Single,
            quantize: Quantize::default(),
            naturalize: Naturalize::default(),
            chop: Tick::new(240),
            arpeggio: Arpeggio::default(),
            strum: Strum::default(),
            pattern: PatternTool::default(),
            window: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    QuickQuantize,
    Quantize,
    Naturalize,
    Chop,
    Arpeggiate,
    Strum,
    StrumPattern,
    Legato,
    Glue,
    FitToScale,
    Reverse,
    Flip,
}

impl Tool {
    /// In menu order, with shortcuts as in FL Studio.
    const MENU: [(Tool, &str); 12] = [
        (Tool::QuickQuantize, "Ctrl+Q"),
        (Tool::Quantize, "Alt+Q"),
        (Tool::Naturalize, "Alt+R"),
        (Tool::Chop, "Alt+U"),
        (Tool::Arpeggiate, "Alt+A"),
        (Tool::Strum, "Alt+S"),
        (Tool::StrumPattern, "Alt+P"),
        (Tool::Legato, "Ctrl+L"),
        (Tool::Glue, ""),
        (Tool::FitToScale, ""),
        (Tool::Reverse, ""),
        (Tool::Flip, "Alt+Y"),
    ];

    const SHORTCUTS: [(Tool, Modifiers, Key); 9] = [
        (Tool::QuickQuantize, Modifiers::COMMAND, Key::Q),
        (Tool::Quantize, Modifiers::ALT, Key::Q),
        (Tool::Naturalize, Modifiers::ALT, Key::R),
        (Tool::Chop, Modifiers::ALT, Key::U),
        (Tool::Arpeggiate, Modifiers::ALT, Key::A),
        (Tool::Strum, Modifiers::ALT, Key::S),
        (Tool::StrumPattern, Modifiers::ALT, Key::P),
        (Tool::Legato, Modifiers::COMMAND, Key::L),
        (Tool::Flip, Modifiers::ALT, Key::Y),
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::QuickQuantize => "Quick Quantize",
            Tool::Quantize => "Quantize…",
            Tool::Naturalize => "Naturalize…",
            Tool::Chop => "Chop…",
            Tool::Arpeggiate => "Arpeggiate…",
            Tool::Strum => "Strum…",
            Tool::StrumPattern => "Strum Pattern…",
            Tool::Legato => "Legato",
            Tool::Glue => "Glue",
            Tool::FitToScale => "Fit to Scale",
            Tool::Reverse => "Reverse",
            Tool::Flip => "Flip",
        }
    }

    /// The undo step's name.
    fn undo_label(self) -> &'static str {
        match self {
            Tool::QuickQuantize | Tool::Quantize => "Quantize",
            Tool::Naturalize => "Naturalize",
            Tool::Chop => "Chop Notes",
            Tool::Arpeggiate => "Arpeggiate",
            Tool::Strum => "Strum",
            Tool::StrumPattern => "Strum Pattern",
            Tool::Legato => "Legato",
            Tool::Glue => "Glue Notes",
            Tool::FitToScale => "Fit to Scale",
            Tool::Reverse => "Reverse Notes",
            Tool::Flip => "Flip Notes",
        }
    }

    /// Tools with settings, shown in a window that previews them.
    fn has_window(self) -> bool {
        matches!(
            self,
            Tool::Quantize
                | Tool::Naturalize
                | Tool::Chop
                | Tool::Arpeggiate
                | Tool::Strum
                | Tool::StrumPattern
        )
    }
}

/// A tool window: what the clip was when it opened, which it works from
/// every time a setting changes.
struct Window {
    tool: Tool,
    clip: project::ClipId,
    notes: Vec<Note>,
    ids: Vec<NoteId>,
    origin: Tick,
    /// The notes last previewed.
    sent: Option<Vec<Note>>,
}

/// Grid choices for the tools, in ticks.
const GRIDS: [(&str, i64); 8] = [
    ("1 bar", 3840),
    ("1/2", 1920),
    ("1/4", 960),
    ("1/8", 480),
    ("1/8 triplet", 320),
    ("1/16", 240),
    ("1/16 triplet", 160),
    ("1/32", 120),
];

impl Tools {
    /// The tool's result on `notes`, of `clip`.
    fn apply(
        &self,
        tool: Tool,
        clip: ClipId,
        notes: &[Note],
        ids: &[NoteId],
        origin: Tick,
        project: &mut Project,
    ) -> Vec<Note> {
        let scale = project.scale;
        let bar = project.tempo.bar_len();
        let articulation = articulation(project, clip);
        let mut new_id = || project.new_note_id();
        match tool {
            Tool::QuickQuantize => {
                let q = Quantize {
                    strength: 1.0,
                    starts: true,
                    ends: false,
                    ..self.quantize
                };
                note_tools::quantize(notes, ids, q, origin)
            }
            Tool::Quantize => note_tools::quantize(notes, ids, self.quantize, origin),
            Tool::Naturalize => note_tools::naturalize(notes, ids, self.naturalize),
            Tool::Chop => note_tools::chop(notes, ids, self.chop, origin, &mut new_id),
            Tool::Arpeggiate => note_tools::arpeggiate(notes, ids, self.arpeggio, &mut new_id),
            Tool::Strum => note_tools::strum(notes, ids, self.strum),
            // It strums the chords picked out, never the whole clip.
            Tool::StrumPattern if ids.is_empty() => notes.to_vec(),
            Tool::StrumPattern => note_tools::strum_pattern(
                notes,
                ids,
                &self.pattern.settings,
                articulation,
                origin,
                bar,
                &mut new_id,
            ),
            Tool::Legato => note_tools::legato(notes, ids),
            Tool::Glue => note_tools::glue(notes, ids),
            Tool::FitToScale => match scale {
                Some(scale) => note_tools::fit_to_scale(notes, ids, scale),
                None => notes.to_vec(),
            },
            Tool::Reverse => note_tools::reverse(notes, ids),
            Tool::Flip => note_tools::flip(notes, ids),
        }
    }

    /// The settings for `tool`. True if any changed.
    fn settings(
        &mut self,
        ui: &mut egui::Ui,
        tool: Tool,
        tempo: &project::TempoMap,
        context: &pattern::Context,
    ) -> bool {
        let before = (
            self.quantize,
            self.naturalize,
            self.chop,
            self.arpeggio,
            self.strum,
            self.pattern.settings.clone(),
        );
        let ms_of = |ticks: Tick| tempo.ticks_to_seconds(ticks.get() as f64) * 1000.0;
        let ticks_of = |ms: f64| Tick::new(tempo.seconds_to_ticks(ms / 1000.0).round() as i64);
        egui::Grid::new("note_tool")
            .num_columns(2)
            .show(ui, |ui| match tool {
                Tool::Quantize | Tool::QuickQuantize => {
                    ui.label("Grid");
                    grid_choice(ui, "quantize_grid", &mut self.quantize.grid);
                    ui.end_row();
                    ui.label("Strength");
                    percent(ui, &mut self.quantize.strength);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut self.quantize.starts, "Starts");
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut self.quantize.ends, "Ends");
                    ui.end_row();
                }
                Tool::Naturalize => {
                    let n = &mut self.naturalize;
                    ui.label("Timing");
                    let mut ms = ms_of(n.timing);
                    ui.add(
                        DragValue::new(&mut ms)
                            .range(0.0..=100.0)
                            .speed(0.2)
                            .max_decimals(1)
                            .suffix(" ms"),
                    );
                    n.timing = ticks_of(ms);
                    ui.end_row();
                    ui.label("Velocity");
                    percent(ui, &mut n.velocity);
                    ui.end_row();
                    ui.label("Length");
                    percent(ui, &mut n.length);
                    ui.end_row();
                    ui.label("");
                    if ui.button("Reroll").clicked() {
                        n.seed = n.seed.wrapping_add(1);
                    }
                    ui.end_row();
                }
                Tool::Chop => {
                    ui.label("Into");
                    grid_choice(ui, "chop_grid", &mut self.chop);
                    ui.end_row();
                }
                Tool::Arpeggiate => {
                    let a = &mut self.arpeggio;
                    ui.label("Pattern");
                    ComboBox::from_id_salt("arp_pattern")
                        .selected_text(a.pattern.label())
                        .show_ui(ui, |ui| {
                            for pattern in ArpPattern::ALL {
                                ui.selectable_value(&mut a.pattern, pattern, pattern.label());
                            }
                        });
                    ui.end_row();
                    ui.label("Step");
                    grid_choice(ui, "arp_step", &mut a.step);
                    ui.end_row();
                    ui.label("Gate");
                    percent(ui, &mut a.gate);
                    ui.end_row();
                    ui.label("Octaves");
                    ui.add(DragValue::new(&mut a.octaves).range(1..=4));
                    ui.end_row();
                    if a.pattern == ArpPattern::Random {
                        ui.label("");
                        if ui.button("Reroll").clicked() {
                            a.seed = a.seed.wrapping_add(1);
                        }
                        ui.end_row();
                    }
                }
                Tool::Strum => {
                    let s = &mut self.strum;
                    ui.label("Delay");
                    let mut ms = ms_of(s.delay);
                    ui.add(
                        DragValue::new(&mut ms)
                            .range(0.0..=200.0)
                            .speed(0.5)
                            .max_decimals(1)
                            .suffix(" ms"),
                    );
                    s.delay = ticks_of(ms);
                    ui.end_row();
                    ui.label("");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.upward, true, "Up");
                        ui.selectable_value(&mut s.upward, false, "Down");
                    });
                    ui.end_row();
                }
                _ => {}
            });
        if tool == Tool::StrumPattern {
            self.pattern.ui(ui, context);
        }
        before
            != (
                self.quantize,
                self.naturalize,
                self.chop,
                self.arpeggio,
                self.strum,
                self.pattern.settings.clone(),
            )
    }
}

/// How the instrument playing `clip` plays dead notes and palm mutes.
fn articulation(project: &Project, clip: ClipId) -> Articulation {
    project
        .clip(clip)
        .and_then(|(track, _)| project.track(track))
        .map_or(Articulation::Shortened, Articulation::for_track)
}

fn percent(ui: &mut egui::Ui, value: &mut f32) {
    let mut p = *value * 100.0;
    ui.add(
        DragValue::new(&mut p)
            .range(0.0..=100.0)
            .speed(0.5)
            .max_decimals(0)
            .suffix("%"),
    );
    *value = p / 100.0;
}

fn grid_choice(ui: &mut egui::Ui, id: &str, value: &mut Tick) {
    let current = GRIDS
        .iter()
        .find(|(_, t)| *t == value.get())
        .map_or("?", |(label, _)| label);
    ComboBox::from_id_salt(id)
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (label, ticks) in GRIDS {
                ui.selectable_value(value, Tick::new(ticks), label);
            }
        });
}

impl PianoRoll {
    /// The piano roll header's chord stamp and Tools menu.
    pub fn tools_menu(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        let chord = &mut self.tools.chord;
        ComboBox::from_id_salt("chord_stamp")
            .selected_text(chord.label())
            .width(70.0)
            .show_ui(ui, |ui| {
                for kind in ChordKind::ALL {
                    ui.selectable_value(chord, kind, kind.label());
                }
            })
            .response
            .on_hover_text("What double-clicking writes; chords fit the song's scale");
        ui.menu_button("Tools", |ui| self.tool_items(ui, session));
    }

    /// The tools, as menu items with their shortcuts.
    pub(super) fn tool_items(&mut self, ui: &mut egui::Ui, session: &mut Session) {
        for (tool, shortcut) in Tool::MENU {
            if crate::widgets::chrome::menu_item(ui, tool.label(), shortcut).clicked() {
                self.run_tool(session, tool);
                ui.close();
            }
        }
    }

    /// Tool shortcuts, while the piano roll has the keyboard.
    pub(super) fn tool_shortcuts(&mut self, ui: &egui::Ui, session: &mut Session) {
        for (tool, modifiers, key) in Tool::SHORTCUTS {
            if ui.ctx().input_mut(|i| i.consume_key(modifiers, key)) {
                self.run_tool(session, tool);
            }
        }
    }

    /// Runs `tool` on the selected notes, or all of them if none are: at
    /// once, or in a window with its settings.
    pub fn run_tool(&mut self, session: &mut Session, tool: Tool) {
        let Some(clip) = self.shown else {
            return;
        };
        let Some((_, ClipRef::Midi(midi))) = session.project.clip(clip) else {
            return;
        };
        let (notes, origin) = (midi.notes.clone(), midi.start - midi.offset);
        if notes.is_empty() {
            return;
        }
        let ids = self.selection.clone();
        if tool == Tool::FitToScale && session.project.scale.is_none() {
            session.error("Pick a scale first, next to the tempo");
            return;
        }
        if tool.has_window() {
            // The grid tools start from the piano roll's grid.
            let step = self.grid.step(&self.viewport, &session.project.tempo);
            if GRIDS.iter().any(|(_, t)| *t == step.get()) {
                self.tools.quantize.grid = step;
                self.tools.chop = step;
            }
            self.tools.window = Some(Window {
                tool,
                clip,
                notes,
                ids,
                origin,
                sent: None,
            });
            return;
        }
        let tools = &self.tools;
        let result = session.edit(tool.undo_label(), |p| {
            let new = tools.apply(tool, clip, &notes, &ids, origin, p);
            p.set_notes(clip, new.clone())?;
            Ok(new)
        });
        if let Some(new) = result {
            self.select_changed(&notes, &ids, &new);
        }
    }

    /// After a tool: what it made or changed stays selected, if anything was.
    fn select_changed(&mut self, before: &[Note], ids: &[NoteId], after: &[Note]) {
        if ids.is_empty() {
            return;
        }
        let untouched: Vec<NoteId> = before
            .iter()
            .filter(|n| !ids.contains(&n.id))
            .map(|n| n.id)
            .collect();
        self.selection = after
            .iter()
            .map(|n| n.id)
            .filter(|id| !untouched.contains(id))
            .collect();
    }

    /// The open tool window, previewing its settings live.
    pub(super) fn tool_window(&mut self, ctx: &egui::Context, session: &mut Session) {
        let Some(mut window) = self.tools.window.take() else {
            return;
        };
        let Some((_, ClipRef::Midi(midi))) = session.project.clip(window.clip) else {
            session.cancel_live();
            return;
        };
        // Strum Pattern opened with nothing selected waits for chords to
        // be picked out, then works on them.
        let mut refreshed = false;
        if window.tool == Tool::StrumPattern
            && window.ids.is_empty()
            && window.sent.is_none()
            && !self.selection.is_empty()
        {
            window.notes = midi.notes.clone();
            window.ids = self.selection.clone();
            refreshed = true;
        }
        let mut outcome = None;
        let mut changed = false;
        let tempo = session.project.tempo;
        let context = pattern::Context {
            meter: tempo.time_signature,
            selected: !window.ids.is_empty(),
            articulation: articulation(&session.project, window.clip),
        };
        // Top right of the piano roll, clear of where notes usually start.
        let at = self.rect.right_top() + egui::vec2(-12.0, 24.0);
        egui::Window::new(window.tool.label().trim_end_matches('…'))
            .id(egui::Id::new("note_tool_window"))
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::RIGHT_TOP)
            .default_pos(at)
            .show(ctx, |ui| {
                changed = self.tools.settings(ui, window.tool, &tempo, &context);
                let mut enabled = true;
                if window.tool == Tool::StrumPattern {
                    enabled = context.selected && self.tools.pattern.valid();
                    if context.selected {
                        listen(ui, session, &window);
                    }
                }
                ui.separator();
                let buttons = chrome::dialog_buttons(ui, "Apply", enabled, None, "Cancel");
                if buttons.primary.clicked() {
                    outcome = Some(true);
                }
                if buttons.cancel.clicked() {
                    outcome = Some(false);
                }
            });
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            outcome = Some(false);
        }
        match outcome {
            Some(true) => {
                session.end_live(window.tool.undo_label());
                if let Some(new) = &window.sent {
                    self.select_changed(&window.notes, &window.ids, new);
                }
            }
            Some(false) => session.cancel_live(),
            // Only when a setting changes: the tools that make notes give
            // them new ids every time.
            None if changed || refreshed || window.sent.is_none() => {
                let tools = &self.tools;
                let mut preview = None;
                // Nothing to strum yet: nothing to preview.
                let idle = window.tool == Tool::StrumPattern && window.ids.is_empty();
                if !idle {
                    session.live_edit(|p| {
                        let new = tools.apply(
                            window.tool,
                            window.clip,
                            &window.notes,
                            &window.ids,
                            window.origin,
                            p,
                        );
                        // Unchanged settings: nothing to redo.
                        if window.sent.as_ref() == Some(&new) {
                            return Err(EditError::NothingSelected);
                        }
                        preview = Some(new.clone());
                        p.set_notes(window.clip, new)
                    });
                }
                if let Some(new) = preview {
                    window.sent = Some(new);
                }
                self.tools.window = Some(window);
            }
            None => self.tools.window = Some(window),
        }
    }

    /// The notes a double-click at `pitch` writes: one, or a chord on it.
    pub(super) fn stamp(&self, project: &Project, note: Note) -> Vec<Note> {
        note_tools::chord(note.pitch, self.tools.chord, project.scale)
            .into_iter()
            .map(|pitch| Note {
                pitch,
                ..note.clone()
            })
            .collect()
    }
}

/// The Strum Pattern window's Listen switch: loops the bars the chords are
/// in and plays them, so each change is heard as it's made.
fn listen(ui: &mut egui::Ui, session: &mut Session, window: &Window) {
    let playing = session.is_playing();
    let what = "Loop the chords' bars and play them while you try patterns";
    if !chrome::switch(ui, playing, "Listen", what, "Space").clicked() {
        return;
    }
    if playing {
        session.set_playing(false);
        return;
    }
    let chords = window.notes.iter().filter(|n| window.ids.contains(&n.id));
    let span = chords.fold(None, |span: Option<(Tick, Tick)>, n| {
        let (start, end) = (window.origin + n.start, window.origin + n.end());
        Some(span.map_or((start, end), |(a, b)| (a.min(start), b.max(end))))
    });
    if let Some((start, end)) = span {
        let bar = session.project.tempo.bar_len().get().max(1);
        let first = start.get().div_euclid(bar) * bar;
        let last = (end.get() + bar - 1).div_euclid(bar) * bar;
        session.loop_span(Tick::new(first.max(0)), Tick::new(last));
    }
    session.set_playing(true);
}
