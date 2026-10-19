//! The Clip tab for an audio clip: level, fades, direction, speed, pitch and
//! tempo, and the chopping and gluing tools.

use eframe::egui::{self, DragValue, Grid, RichText};
use project::{AudioClip, ChannelMode, ClipId, ClipRef, EditError, Project, StretchMode};

use crate::session::Session;
use crate::theme::space;
use crate::timeline::{Action, AudioEdit, Chop, TimelineView};
use crate::widgets::chrome;

/// How wide the labels down the left of each group are, so the controls
/// beside them line up from group to group.
const LABEL_WIDTH: f32 = 56.0;

/// Remembered between frames: the transient chop's sensitivity.
#[derive(Debug, Clone, Copy)]
pub struct ClipPanel {
    pub sensitivity: f32,
}

impl Default for ClipPanel {
    fn default() -> Self {
        ClipPanel {
            sensitivity: crate::timeline::DEFAULT_SENSITIVITY,
        }
    }
}

impl ClipPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        timeline: &mut TimelineView,
        id: ClipId,
        grid_step: midi::Tick,
    ) {
        let Some((_, ClipRef::Audio(clip))) = session.project.clip(id) else {
            return;
        };
        let clip = clip.clone();
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    self.columns(ui, session, timeline, &clip, grid_step);
                });
            });
    }

    fn columns(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        timeline: &mut TimelineView,
        clip: &AudioClip,
        grid_step: midi::Tick,
    ) {
        let id = clip.id;
        let project = &session.project;
        let rate = project
            .source(clip.source)
            .map_or(48_000, |s| s.sample_rate) as f64;
        let speed = clip.stretch.effective_speed(&project.tempo);
        // Fades are frames of material; show them as time on the timeline.
        let to_ms = |frames: u64| frames as f64 / rate / speed * 1000.0;
        let to_frames = |ms: f64| (ms.max(0.0) / 1000.0 * rate * speed).round() as u64;

        ui.vertical(|ui| {
            ui.set_width(190.0);
            chrome::caption(ui, "Clip");
            ui.strong(&clip.name);
            if let Some(source) = project.source(clip.source) {
                let file = source
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                ui.label(RichText::new(file).weak())
                    .on_hover_text(source.path.display().to_string());
                ui.weak(format!(
                    "{} Hz · {} ch · {:.2} s",
                    source.sample_rate,
                    source.channels,
                    source.duration_seconds()
                ));
            }
        });
        ui.separator();

        // Level.
        ui.vertical(|ui| {
            chrome::caption(ui, "Level");
            grid("clip_level").show(ui, |ui| {
                ui.label("Gain");
                ui.horizontal(|ui| {
                    let mut gain = clip.gain_db;
                    let response = ui.add(
                        DragValue::new(&mut gain)
                            .range(project::SILENCE_DB..=12.0)
                            .speed(0.1)
                            .fixed_decimals(1)
                            .suffix(" dB"),
                    );
                    live(session, &response, "Clip Gain", |p| {
                        p.set_clip_gain(id, gain)
                    });
                    if ui
                        .button("Normalize")
                        .on_hover_text("Loudest sample to 0 dB")
                        .clicked()
                    {
                        timeline.apply(session, Action::NormalizeClips(vec![id]));
                    }
                });
                ui.end_row();

                let fades = (
                    clip.fade_in,
                    clip.fade_out,
                    clip.fade_in_curve,
                    clip.fade_out_curve,
                );
                ui.label("Fade in");
                let mut fade_in = to_ms(clip.fade_in);
                let response = ui.add(fade_drag(&mut fade_in));
                live(session, &response, "Clip Fade", |p| {
                    p.set_clip_fades(id, to_frames(fade_in), fades.1, fades.2, fades.3)
                });
                ui.end_row();
                ui.label("Fade out");
                let mut fade_out = to_ms(clip.fade_out);
                let response = ui.add(fade_drag(&mut fade_out));
                live(session, &response, "Clip Fade", |p| {
                    p.set_clip_fades(id, fades.0, to_frames(fade_out), fades.2, fades.3)
                });
                ui.end_row();

                ui.label("Channels");
                let mut channels = clip.channels;
                egui::ComboBox::from_id_salt("clip_channels")
                    .selected_text(channel_label(channels))
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for mode in [ChannelMode::Stereo, ChannelMode::Mono, ChannelMode::Swap] {
                            ui.selectable_value(&mut channels, mode, channel_label(mode));
                        }
                    });
                if channels != clip.channels {
                    let edit = AudioEdit::Channels(channels);
                    timeline.apply(session, Action::EditAudioClips(vec![id], edit));
                }
                ui.end_row();
            });
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                let switches = [
                    (
                        clip.reversed,
                        "Reverse",
                        AudioEdit::Reverse(!clip.reversed),
                        "Play backwards",
                    ),
                    (
                        clip.looped,
                        "Loop",
                        AudioEdit::Loop(!clip.looped),
                        "Repeat to fill the clip",
                    ),
                    (
                        clip.polarity_inverted,
                        "Ø",
                        AudioEdit::InvertPolarity(!clip.polarity_inverted),
                        "Invert polarity",
                    ),
                ];
                for (on, label, edit, tip) in switches {
                    if chrome::switch(ui, on, label, tip, "").clicked() {
                        timeline.apply(session, Action::EditAudioClips(vec![id], edit));
                    }
                }
            });
        });
        ui.separator();

        // Time and pitch.
        let stretch = clip.stretch;
        ui.vertical(|ui| {
            chrome::caption(ui, "Time and pitch");
            grid("clip_time").show(ui, |ui| {
                ui.label("Mode");
                let mut mode = stretch.mode;
                egui::ComboBox::from_id_salt("clip_stretch_mode")
                    .selected_text(mode_label(mode))
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for (option, tip) in [
                            (StretchMode::Resample, "Faster is higher, like a record"),
                            (StretchMode::Stretch, "Speed and pitch apart"),
                            (StretchMode::Beats, "Slices at the hits; for drums"),
                        ] {
                            ui.selectable_value(&mut mode, option, mode_label(option))
                                .on_hover_text(tip);
                        }
                    });
                if mode != stretch.mode {
                    session.edit("Stretch Mode", |p| {
                        p.edit_audio_clip(id, |c| c.stretch.mode = mode)
                    });
                }
                ui.end_row();

                ui.label("Speed");
                let mut speed = stretch.speed;
                let response = ui.add(
                    DragValue::new(&mut speed)
                        .range(project::Stretch::MIN_SPEED..=project::Stretch::MAX_SPEED)
                        .speed(0.005)
                        .max_decimals(3)
                        .prefix("×"),
                );
                live(session, &response, "Clip Speed", |p| {
                    p.edit_audio_clip(id, |c| c.stretch.speed = speed)
                });
                ui.end_row();

                ui.label("Pitch");
                match stretch.mode {
                    StretchMode::Resample => {
                        // Pitch and speed are one thing here.
                        let mut semitones = 12.0 * stretch.speed.log2();
                        let response = ui
                            .add(semitone_drag(&mut semitones))
                            .on_hover_text("Changes the speed too");
                        live(session, &response, "Clip Pitch", |p| {
                            p.edit_audio_clip(id, |c| c.stretch.speed = (semitones / 12.0).exp2())
                        });
                    }
                    StretchMode::Stretch => {
                        let mut semitones = stretch.transpose;
                        let response = ui.add(semitone_drag(&mut semitones));
                        live(session, &response, "Clip Pitch", |p| {
                            p.edit_audio_clip(id, |c| c.stretch.transpose = semitones)
                        });
                    }
                    StretchMode::Beats => {
                        ui.add_enabled(false, egui::Label::new("—"));
                    }
                }
                ui.end_row();

                ui.label("Tempo");
                ui.horizontal(|ui| {
                    let mut bpm = stretch.original_bpm.unwrap_or(0.0);
                    let response = ui
                        .add(
                            DragValue::new(&mut bpm)
                                .range(0.0..=999.0)
                                .speed(0.1)
                                .max_decimals(2)
                                .custom_formatter(|v, _| {
                                    if v <= 0.0 {
                                        "—".into()
                                    } else {
                                        format!("{v:.2}")
                                    }
                                })
                                .suffix(" BPM"),
                        )
                        .on_hover_text("The audio's own tempo");
                    live(session, &response, "Clip Tempo", |p| {
                        p.edit_audio_clip(id, |c| {
                            c.stretch.original_bpm = (bpm > 0.0).then_some(bpm);
                            c.stretch.sync &= bpm > 0.0;
                        })
                    });
                    let mut sync = stretch.sync;
                    let toggle = ui
                        .add_enabled(
                            stretch.original_bpm.is_some(),
                            egui::Button::new("Sync").selected(sync),
                        )
                        .on_hover_text("Follow the project tempo")
                        .on_disabled_hover_text("Set the audio's own tempo to sync it");
                    if toggle.clicked() {
                        sync = !sync;
                        session.edit("Tempo Sync", |p| {
                            p.edit_audio_clip(id, |c| c.stretch.sync = sync)
                        });
                    }
                });
                ui.end_row();
            });
        });
        ui.separator();

        // Cutting and gluing.
        ui.vertical(|ui| {
            chrome::caption(ui, "Cut and glue");
            ui.horizontal(|ui| {
                if ui
                    .button("Chop at hits")
                    .on_hover_text("Split at every transient")
                    .clicked()
                {
                    let chop = Chop::Transients(self.sensitivity);
                    timeline.apply(session, Action::ChopClip(id, chop));
                }
                let mut percent = self.sensitivity * 100.0;
                ui.add(
                    DragValue::new(&mut percent)
                        .range(0.0..=100.0)
                        .speed(0.5)
                        .max_decimals(0)
                        .suffix("%"),
                )
                .on_hover_text("Sensitivity: higher finds more");
                self.sensitivity = percent / 100.0;
            });
            if ui
                .button("Chop to grid")
                .on_hover_text("Split at every grid line")
                .clicked()
            {
                timeline.apply(session, Action::ChopClip(id, Chop::Grid(grid_step)));
            }
            if ui
                .button("Consolidate")
                .on_hover_text("Render to a new file as it sounds")
                .clicked()
            {
                let clips = if timeline.selection.has_clip(id) {
                    timeline.selection.clips.clone()
                } else {
                    vec![id]
                };
                timeline.apply(session, Action::ConsolidateClips(clips));
            }
        });
    }
}

/// A group's two columns: labels, then their controls.
fn grid(id: &str) -> Grid {
    Grid::new(id)
        .num_columns(2)
        .min_col_width(LABEL_WIDTH)
        .spacing([space::M, space::S])
}

/// Applies a control's change live, as one undo step per drag or typed value.
fn live(
    session: &mut Session,
    response: &egui::Response,
    label: &str,
    edit: impl FnOnce(&mut Project) -> Result<(), EditError>,
) {
    if response.changed() {
        session.live_edit(edit);
    }
    if response.drag_stopped() || (response.changed() && !response.dragged()) {
        session.end_live(label);
    }
}

fn fade_drag(ms: &mut f64) -> DragValue<'_> {
    DragValue::new(ms)
        .range(0.0..=60_000.0)
        .speed(1.0)
        .max_decimals(1)
        .suffix(" ms")
}

fn semitone_drag(semitones: &mut f64) -> DragValue<'_> {
    DragValue::new(semitones)
        .range(-36.0..=36.0)
        .speed(0.05)
        .max_decimals(2)
        .suffix(" st")
}

fn channel_label(mode: ChannelMode) -> &'static str {
    match mode {
        ChannelMode::Stereo => "Stereo",
        ChannelMode::Mono => "Mono",
        ChannelMode::Swap => "Swapped",
    }
}

fn mode_label(mode: StretchMode) -> &'static str {
    match mode {
        StretchMode::Resample => "Resample",
        StretchMode::Stretch => "Stretch",
        StretchMode::Beats => "Beats",
    }
}
