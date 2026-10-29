//! The other bottom detail view: the selected track's mixer, its
//! modulators, its instrument if it has one, and its effects; or the
//! master's volume and effects.
//!
//! Layout, left to right: the mixer column (name, volume, pan, + Effect),
//! which stays put, then everything that plays the sound, in order, each
//! under a title bar of the same height: the instrument (its picker and
//! presets in its bar) and the effects (see `effect_rack`). A long chain
//! scrolls sideways beside the mixer column.

use eframe::egui::{self, Grid, RichText, Shape, Slider};
use project::{AutomationTarget, Mixer, Track, TrackId, TrackKind};

use crate::devices::{self, Panel};
use crate::effect_rack;
use crate::session::Session;
use crate::theme::{self, size, space};
use crate::timeline::{Action, TimelineView, automate_menu, color32};
use crate::widgets::chrome;

/// The mixer column's width.
const MIXER_WIDTH: f32 = 200.0;

pub fn show(ui: &mut egui::Ui, session: &mut Session, timeline: &mut TimelineView) {
    if timeline.selection.master {
        master(ui, session, timeline);
        return;
    }
    let Some(track_id) = timeline.selection.primary_track() else {
        let rect = ui.available_rect_before_wrap();
        chrome::empty_hint(
            ui.painter(),
            rect,
            "No track selected",
            "Click a track's header to see its instrument and effects",
        );
        return;
    };
    // A copy, since the panels below edit the session; without the clips,
    // which nothing here reads.
    let Some(track) = session.project.track(track_id).map(Track::without_clips) else {
        return;
    };
    // The mixer column (and "+ Effect") stays put; the instrument and the
    // effects scroll beside it, however long the chain.
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(MIXER_WIDTH);
            title(ui, &track.name, Some(color32(track.color)));
            mixer(ui, session, timeline, &track);
            ui.add_space(space::S);
            effect_rack::add_button(ui, session, Some(track.id));
        });
        ui.add_space(space::S);
        ui.separator();
        ui.add_space(space::S);
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let color = color32(track.color);
                    crate::modulation::column(ui, session, timeline, &track, color);
                    if let TrackKind::Midi { .. } = &track.kind {
                        instrument(ui, session, timeline, &track);
                    }
                    effect_rack::show(ui, session, timeline, Some(track.id), color);
                });
            });
    });
}

/// The mixer column's title: the track's name, by a dot of its color.
fn title(ui: &mut egui::Ui, name: &str, color: Option<egui::Color32>) {
    ui.horizontal(|ui| {
        ui.set_min_height(size::BAR);
        if let Some(color) = color {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, color);
        }
        ui.label(RichText::new(name).heading().strong());
    });
    ui.add_space(space::XS);
}

/// The master: its volume and its effects.
fn master(ui: &mut egui::Ui, session: &mut Session, timeline: &mut TimelineView) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(MIXER_WIDTH);
            title(ui, "Master", None);
            let mut mixer = session.project.master;
            Grid::new("master_mixer")
                .num_columns(2)
                .spacing([space::M, space::S])
                .show(ui, |ui| {
                    ui.label("Volume");
                    let response = ui.add(
                        Slider::new(
                            &mut mixer.volume_db,
                            project::SILENCE_DB..=project::MAX_VOLUME_DB,
                        )
                        .suffix(" dB")
                        .fixed_decimals(1),
                    );
                    if response.changed() && !right_button(ui.ctx()) {
                        timeline.apply(session, Action::LiveMixer { track: None, mixer });
                    }
                    if response.drag_stopped() || (response.changed() && !response.dragged()) {
                        session.end_live("Master Volume");
                    }
                    ui.end_row();
                });
            ui.add_space(space::S);
            effect_rack::add_button(ui, session, None);
        });
        ui.add_space(space::S);
        ui.separator();
        ui.add_space(space::S);
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let accent = theme::accent(ui.visuals());
                    effect_rack::show(ui, session, timeline, None, accent);
                });
            });
    });
}

fn mixer(ui: &mut egui::Ui, session: &mut Session, timeline: &mut TimelineView, track: &Track) {
    let mixer = track.mixer;
    Grid::new("mixer")
        .num_columns(2)
        .spacing([space::M, space::S])
        .show(ui, |ui| {
            ui.label("Volume");
            let mut volume = mixer.volume_db;
            let response = ui.add(
                Slider::new(&mut volume, project::SILENCE_DB..=project::MAX_VOLUME_DB)
                    .suffix(" dB")
                    .fixed_decimals(1),
            );
            live(
                session,
                timeline,
                track.id,
                &response,
                Mixer {
                    volume_db: volume,
                    ..mixer
                },
                "Volume",
            );
            automate_menu(timeline, &response, track.id, AutomationTarget::Volume);
            ui.end_row();

            ui.label("Pan");
            let mut pan = mixer.pan;
            let response = ui.add(
                Slider::new(&mut pan, -1.0..=1.0)
                    .custom_formatter(|v, _| crate::timeline::format_pan(v as f32)),
            );
            live(
                session,
                timeline,
                track.id,
                &response,
                Mixer { pan, ..mixer },
                "Pan",
            );
            automate_menu(timeline, &response, track.id, AutomationTarget::Pan);
            ui.end_row();
        });
}

/// egui sliders jump to the pointer on any button, so a right-click (for the
/// Automate menu) would also set the value. Changes made then are ignored.
fn right_button(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        i.pointer.button_down(egui::PointerButton::Secondary)
            || i.pointer.button_released(egui::PointerButton::Secondary)
    })
}

fn live(
    session: &mut Session,
    timeline: &mut TimelineView,
    track: TrackId,
    response: &egui::Response,
    mixer: Mixer,
    label: &'static str,
) {
    if response.changed() && !right_button(&response.ctx) {
        timeline.apply(
            session,
            Action::LiveMixer {
                track: Some(track),
                mixer,
            },
        );
    }
    if response.drag_stopped() || (response.changed() && !response.dragged()) {
        session.end_live(label);
    }
}

/// The instrument, under a title bar like an effect's: which instrument it
/// is and its presets in the bar, its panel below.
fn instrument(
    ui: &mut egui::Ui,
    session: &mut Session,
    timeline: &mut TimelineView,
    track: &Track,
) {
    let TrackKind::Midi { instrument, .. } = &track.kind else {
        return;
    };
    let available = instrument.descriptor().is_some();
    let inner = ui.vertical(|ui| {
        let background = ui.painter().add(Shape::Noop);
        ui.horizontal(|ui| {
            ui.set_min_height(size::BAR);
            ui.add_space(space::S);
            crate::plugins::instrument_picker(ui, session, track);
        });
        if available
            && let Some(mut panel) =
                Panel::instrument(session, timeline, track, color32(track.color))
        {
            devices::instrument(ui, &mut panel);
            panel.animate(ui);
        }
        background
    });
    let fill = chrome::bar_fill(
        ui,
        egui::Id::new(("instrument bar", track.id.0)),
        false,
        false,
    );
    ui.painter().set(
        inner.inner,
        chrome::card(ui.visuals(), inner.response.rect, size::BAR, fill),
    );
}
