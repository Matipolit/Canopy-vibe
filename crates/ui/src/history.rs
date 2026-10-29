//! The History panel: every undo step in the order it was made, the
//! current state highlighted and the undone ones dimmed. Clicking a step
//! goes back (or forward) to the state right after it, however far away;
//! the steps in between stay as undo or redo, so a jump can be undone too.

use std::time::{Duration, SystemTime};

use eframe::egui::{self, Align2, RichText, Sense, Ui};

use crate::session::Session;
use crate::theme::{self, size, space, text};
use crate::widgets::chrome;

/// The panel's width when it first opens; it can be dragged wider.
pub const WIDTH: f32 = 220.0;

const ROW: f32 = size::CONTROL + space::S;

pub fn panel(ui: &mut Ui, session: &mut Session) {
    let history = session.history();
    let position = history.position();
    // Row 0 is the state before the first step kept.
    let first = if history.forgot_steps() {
        "Oldest step kept"
    } else {
        "Start"
    };
    let rows: Vec<(String, Option<SystemTime>)> = std::iter::once((first.to_owned(), None))
        .chain(history.steps().map(|s| (s.label.to_owned(), Some(s.at))))
        .collect();

    ui.add_space(space::S);
    chrome::section_header(ui, "History", theme::surface::TEXT_WEAK, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(space::S);
            let steps = rows.len() - 1;
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
        let spacing = ui.spacing().item_spacing.y;
        let middle = position as f32 * (ROW + spacing) - ui.available_height() / 2.0;
        area = area.vertical_scroll_offset(middle.max(0.0));
    }

    let now = SystemTime::now();
    let mut clicked = None;
    area.show_rows(ui, ROW, rows.len(), |ui, range| {
        for index in range {
            let (label, at) = &rows[index];
            if row(
                ui,
                index,
                label,
                *at,
                now,
                index == position,
                index > position,
            )
            .clicked()
            {
                clicked = Some(index);
            }
        }
    });
    if let Some(index) = clicked {
        session.jump_to(index);
    }
}

/// One step: its label, and how long ago it was made on the right.
fn row(
    ui: &mut Ui,
    index: usize,
    label: &str,
    at: Option<SystemTime>,
    now: SystemTime,
    current: bool,
    undone: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, current, label)
    });
    let id = ui.id().with(("history step", index));
    let fill = chrome::bar_fill(ui, id, response.hovered(), current);
    let painter = ui.painter();
    painter.rect_filled(rect, theme::radius::SMALL, fill);

    let visuals = ui.visuals();
    let strong = if undone {
        visuals.weak_text_color()
    } else {
        visuals.text_color()
    };
    let inner = rect.shrink2(egui::vec2(space::M, 0.0));
    let when = at.map(|at| ago(now.duration_since(at).unwrap_or_default()));
    let when_width = when.as_ref().map_or(0.0, |when| {
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
    // The label gives way to the time, ending in "…" when it doesn't fit.
    let room = (inner.width() - when_width - space::M).max(0.0);
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
