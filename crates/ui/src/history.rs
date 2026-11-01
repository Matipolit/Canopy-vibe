//! The History panel: every undo step in the order it was made, the
//! current state highlighted and the undone ones dimmed. Clicking a step
//! goes back (or forward) to the state right after it, however far away;
//! the steps in between stay as undo or redo, so a jump can be undone too.
//! Steps an LLM made carry a tag naming its client, and a run of them sits
//! under one header, so what the user did and what was done for them read
//! apart.

use std::time::{Duration, SystemTime};

use eframe::egui::{self, Align2, RichText, Sense, Ui};

use crate::session::{LLM_PREFIX, Session};
use crate::theme::{self, size, space, text};
use crate::widgets::chrome;

/// The panel's width when it first opens; it can be dragged wider.
pub const WIDTH: f32 = 220.0;

const ROW: f32 = size::CONTROL + space::S;

/// A row of the panel.
enum Line {
    /// A state to go to: the one after `position` steps.
    Step {
        position: usize,
        label: String,
        at: Option<SystemTime>,
        /// The LLM client that made the step, if one did.
        client: Option<String>,
        /// Drawn under a [`Line::Group`]'s header.
        grouped: bool,
    },
    /// Above consecutive steps made by one LLM client: who, and how many.
    Group { client: String, steps: usize },
}

/// The panel's rows: the state before any step, then every step, runs of
/// two or more by the same LLM client gathered under a header.
fn lines(history: &project::History) -> Vec<Line> {
    // Row 0 is the state before the first step kept.
    let first = if history.forgot_steps() {
        "Oldest step kept"
    } else {
        "Start"
    };
    let steps: Vec<_> = history.steps().collect();
    let mut lines = vec![Line::Step {
        position: 0,
        label: first.to_owned(),
        at: None,
        client: None,
        grouped: false,
    }];
    let mut i = 0;
    while i < steps.len() {
        let client = steps[i].origin.client();
        let run = steps[i..]
            .iter()
            .take_while(|s| client.is_some() && s.origin.client() == client)
            .count()
            .max(1);
        if run > 1
            && let Some(client) = client
        {
            lines.push(Line::Group {
                client: client.to_owned(),
                steps: run,
            });
        }
        for (k, step) in steps[i..i + run].iter().enumerate() {
            lines.push(Line::Step {
                position: i + k + 1,
                label: step.label.to_owned(),
                at: Some(step.at),
                client: step.origin.client().map(str::to_owned),
                grouped: run > 1,
            });
        }
        i += run;
    }
    lines
}

pub fn panel(ui: &mut Ui, session: &mut Session) {
    let history = session.history();
    let position = history.position();
    let steps = history.steps().count();
    let lines = lines(history);

    ui.add_space(space::S);
    chrome::section_header(ui, "History", theme::surface::TEXT_WEAK, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(space::S);
            let plural = if steps == 1 { "" } else { "s" };
            chrome::caption(ui, &format!("{steps} step{plural}"));
        });
    });

    // Follows the current step whenever it moves (an edit, undo, a jump),
    // and leaves the scroll alone otherwise.
    let shown_id = ui.id().with("history shown position");
    let moved = ui.data(|d| d.get_temp::<usize>(shown_id)) != Some(position);
    ui.data_mut(|d| d.insert_temp(shown_id, position));
    let mut area = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("history steps");
    if moved {
        let current = lines
            .iter()
            .position(|l| matches!(l, Line::Step { position: p, .. } if *p == position))
            .unwrap_or(0);
        let spacing = ui.spacing().item_spacing.y;
        let middle = current as f32 * (ROW + spacing) - ui.available_height() / 2.0;
        area = area.vertical_scroll_offset(middle.max(0.0));
    }

    let now = SystemTime::now();
    let mut clicked = None;
    area.show_rows(ui, ROW, lines.len(), |ui, range| {
        for line in &lines[range] {
            match line {
                Line::Group { client, steps } => group(ui, client, *steps),
                Line::Step {
                    position: at_position,
                    label,
                    at,
                    client,
                    grouped,
                } => {
                    let step = Step {
                        position: *at_position,
                        label,
                        at: *at,
                        client: client.as_deref(),
                        grouped: *grouped,
                    };
                    let current = *at_position == position;
                    if row(ui, &step, now, current, *at_position > position).clicked() {
                        clicked = Some(*at_position);
                    }
                }
            }
        }
    });
    if let Some(index) = clicked {
        session.jump_to(index);
    }
}

/// A [`Line::Step`] as [`row`] draws it.
struct Step<'a> {
    position: usize,
    label: &'a str,
    at: Option<SystemTime>,
    client: Option<&'a str>,
    grouped: bool,
}

/// The header over a run of one LLM client's steps: "claude-code · 3 steps".
fn group(ui: &mut Ui, client: &str, steps: usize) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW), Sense::hover());
    let text = format!("{client} · {steps} steps");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &text));
    let inner = rect.shrink2(egui::vec2(space::M, 0.0));
    ui.painter().text(
        inner.left_center(),
        Align2::LEFT_CENTER,
        text,
        text::medium(text::SMALL),
        theme::section::NEUTRAL,
    );
}

/// One step: its label, a tag naming the LLM client that made it (unless
/// its group's header does), and how long ago it was made on the right.
fn row(
    ui: &mut Ui,
    step: &Step<'_>,
    now: SystemTime,
    current: bool,
    undone: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW), Sense::click());
    // An LLM's step is called by its client too, which screen readers
    // (and tests) see as the tag.
    let name = match step.client {
        Some(client) => format!("{} · {client}", step.label),
        None => step.label.to_owned(),
    };
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, current, &name)
    });
    let id = ui.id().with(("history step", step.position));
    let fill = chrome::bar_fill(ui, id, response.hovered(), current);
    let painter = ui.painter();
    painter.rect_filled(rect, theme::radius::SMALL, fill);

    let visuals = ui.visuals();
    let strong = if undone {
        visuals.weak_text_color()
    } else {
        visuals.text_color()
    };
    let indent = if step.grouped { space::L } else { 0.0 };
    let inner = rect.shrink2(egui::vec2(space::M, 0.0));
    let inner = egui::Rect::from_min_max(inner.min + egui::vec2(indent, 0.0), inner.max);
    let when = step
        .at
        .map(|at| ago(now.duration_since(at).unwrap_or_default()));
    let mut right = when.as_ref().map_or(0.0, |when| {
        painter
            .text(
                inner.right_center(),
                Align2::RIGHT_CENTER,
                when,
                egui::FontId::proportional(text::SMALL),
                visuals.weak_text_color(),
            )
            .width()
    });
    // The tag says who made it, so the label needn't.
    let mut label = step.label;
    if let Some(client) = step.client {
        label = label.strip_prefix(LLM_PREFIX).unwrap_or(label);
        if !step.grouped {
            right += space::S
                + tag(
                    painter,
                    inner.right_center() - egui::vec2(right + space::S, 0.0),
                    client,
                );
        }
    }
    // The label gives way to the time, ending in "…" when it doesn't fit.
    let room = (inner.width() - right - space::M).max(0.0);
    let font = if current {
        text::medium(text::BODY)
    } else {
        egui::FontId::proportional(text::BODY)
    };
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), font, strong);
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    let galley = painter.layout_job(job);
    let top = inner.center().y - galley.size().y / 2.0;
    painter.galley(egui::pos2(inner.left(), top), galley, strong);

    let what = match (current, undone) {
        (true, _) => "The project as it is now",
        (false, true) => "Click to redo up to here",
        (false, false) => "Click to go back to here",
    };
    response.on_hover_text(RichText::new(what).size(text::SMALL))
}

/// A small chip naming an LLM client, its right edge at `right`; its width.
fn tag(painter: &egui::Painter, right: egui::Pos2, client: &str) -> f32 {
    let color = theme::section::NEUTRAL;
    let galley = painter.layout_no_wrap(
        client.to_owned(),
        egui::FontId::proportional(text::TINY),
        color,
    );
    let size = galley.size() + egui::vec2(2.0 * space::S, space::XS);
    let rect = egui::Rect::from_min_size(right - egui::vec2(size.x, size.y / 2.0), size);
    painter.rect_filled(rect, theme::radius::SMALL, color.gamma_multiply(0.18));
    painter.galley(
        rect.min + egui::vec2(space::S, space::XS / 2.0),
        galley,
        color,
    );
    size.x
}

/// How long ago, in a few characters: "now", "40 s", "12 min", "3 h", "2 d".
pub fn ago(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..10 => "now".to_owned(),
        10..60 => format!("{seconds} s"),
        60..3600 => format!("{} min", seconds / 60),
        3600..86_400 => format!("{} h", seconds / 3600),
        _ => format!("{} d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use project::{History, NewTrack, Origin, Project, TrackPosition};

    #[test]
    fn runs_of_one_clients_steps_gather_under_a_header() {
        let mut project = Project::new();
        let mut history = History::default();
        let llm = |client: &str| Origin::Llm {
            client: client.to_owned(),
        };
        for origin in [
            Origin::User,
            llm("claude"),
            llm("claude"),
            llm("other"),
            Origin::User,
        ] {
            history
                .edit_from(origin, &mut project, "Add", |p| {
                    p.add_track(NewTrack::Midi, TrackPosition::END)
                })
                .unwrap();
        }
        let shown: Vec<String> = lines(&history)
            .iter()
            .map(|line| match line {
                Line::Group { client, steps } => format!("{client} · {steps}"),
                Line::Step {
                    position,
                    client,
                    grouped,
                    ..
                } => format!(
                    "{}{position}{}",
                    if *grouped { "  " } else { "" },
                    client.as_deref().map_or(String::new(), |c| format!(" {c}"))
                ),
            })
            .collect();
        assert_eq!(
            shown,
            [
                "0",
                "1",
                "claude · 2",
                "  2 claude",
                "  3 claude",
                "4 other",
                "5"
            ]
        );
    }

    #[test]
    fn ages_read_in_their_largest_whole_unit() {
        let s = Duration::from_secs;
        assert_eq!(ago(s(3)), "now");
        assert_eq!(ago(s(42)), "42 s");
        assert_eq!(ago(s(60 * 12 + 59)), "12 min");
        assert_eq!(ago(s(3600 * 3)), "3 h");
        assert_eq!(ago(s(86_400 * 2 + 5)), "2 d");
    }
}
