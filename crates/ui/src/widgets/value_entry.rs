//! Typing a parameter's value: a double-click on its control (or "Type
//! Value…" in its menu) opens a text box over it holding the value as
//! shown, selected, so typing replaces it. Enter, or clicking anywhere
//! else, sets what was typed as [`ParamDescriptor::parse`] reads it ("250
//! ms", "1.5 s", "2k", "-6 dB", "A4", a choice's name); Escape, or text
//! that isn't a value, leaves the value as it was.

use eframe::egui::{
    self, Align2, Area, Id, Key, Order, Response, TextEdit, text::CCursor, text::CCursorRange,
};
use instruments::ParamDescriptor;

use super::knob::format_value;

/// How tall the box is, and how wide at least.
const HEIGHT: f32 = 18.0;
const MIN_WIDTH: f32 = 64.0;

#[derive(Debug, Clone, Default)]
struct Editing {
    text: String,
    /// Not shown yet: focus it and select its text.
    fresh: bool,
}

fn state_id(response: &Response) -> Id {
    response.id.with("typing")
}

/// Opens the box for the control `response` belongs to, holding `value`.
pub fn open(response: &Response, param: &ParamDescriptor, value: f32) {
    let editing = Editing {
        text: format_value(param, value),
        fresh: true,
    };
    response
        .ctx
        .data_mut(|d| d.insert_temp(state_id(response), editing));
}

/// Opens the box on a double-click, and shows it while it's open. Returns
/// the value typed, once one is entered.
pub fn show(response: &Response, param: &ParamDescriptor, value: f32) -> Option<f32> {
    if response.double_clicked() {
        open(response, param, value);
    }
    let id = state_id(response);
    let ctx = &response.ctx;
    let mut editing: Editing = ctx.data(|d| d.get_temp(id))?;
    let rect = response.rect;
    let width = rect.width().max(MIN_WIDTH);
    // Over a knob's value, under its dial; over smaller controls whole.
    let anchor = egui::pos2(
        rect.center().x,
        (rect.bottom() - HEIGHT).max(rect.top()) + HEIGHT / 2.0,
    );
    let edit_id = id.with("edit");
    let mut outcome = None;
    let mut close = false;
    Area::new(id.with("area"))
        .order(Order::Foreground)
        .pivot(Align2::CENTER_CENTER)
        .fixed_pos(anchor)
        .show(ctx, |ui| {
            let output = TextEdit::singleline(&mut editing.text)
                .id(edit_id)
                .desired_width(width)
                .font(egui::TextStyle::Monospace)
                .horizontal_align(egui::Align::Center)
                .show(ui);
            if editing.fresh {
                editing.fresh = false;
                let mut state = output.state;
                let end = CCursor::new(editing.text.chars().count());
                state
                    .cursor
                    .set_char_range(Some(CCursorRange::two(CCursor::new(0), end)));
                state.store(ui.ctx(), edit_id);
                output.response.request_focus();
            } else if output.response.lost_focus() {
                close = true;
                if !ui.input(|i| i.key_pressed(Key::Escape)) {
                    outcome = param.parse(&editing.text);
                }
            } else if !output.response.has_focus() {
                // Never got the focus, or lost it unseen: nothing to set.
                close = true;
            }
        });
    if close {
        ctx.data_mut(|d| d.remove::<Editing>(id));
    } else {
        ctx.data_mut(|d| d.insert_temp(id, editing));
    }
    outcome
}
