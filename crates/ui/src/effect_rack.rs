//! A track's (or the master's) effects in the device view, in the order the
//! sound goes through them. Each has a title bar:
//! - click it to select the effect (Ctrl and Shift to select more);
//! - drag it to move the selected effects, here or onto a track's header or
//!   the master (Ctrl to copy them instead);
//! - double-click it to fold the effect to its bar;
//! - right-click it, or use its menu button, for presets, duplicating,
//!   copying, moving and removing.
//!
//! The selected effects also take the keyboard while the device view was
//! clicked last (see [`shortcuts`]).

use effects::Category;
use eframe::egui::{
    self, Color32, CursorIcon, Key, KeyboardShortcut, Modifiers, Rect, RichText, Sense, Shape,
    Stroke, StrokeKind, vec2,
};
use project::{CopiedDevice, Device, DeviceId, EffectRef, TrackId};

use crate::devices::{self, Panel};
use crate::session::{PresetTarget, Session};
use crate::theme::{self, radius, size, space, stroke};
use crate::timeline::{TimelineView, color32};
use crate::widgets::chrome::{self, Icon};

/// The height of an effect's title bar.
const BAR_HEIGHT: f32 = size::BAR;
/// The size of the bar's buttons at its right end.
const ICON: f32 = size::ICON;

/// Effects being dragged by their title bars, in chain order.
#[derive(Clone, Debug)]
pub struct DeviceDrag {
    pub ids: Vec<DeviceId>,
}

/// The button that adds an effect to the end of `owner`'s chain. It lives in
/// the device view's first column, so it's in reach however long the chain.
pub fn add_button(ui: &mut egui::Ui, session: &mut Session, owner: Option<TrackId>) {
    ui.menu_button("+ Effect", |ui| {
        // Plugins first: the built-ins make a long list.
        if let Some(id) = crate::plugins::effect_menu(ui, session, owner) {
            reveal(ui.ctx(), id);
        }
        for category in Category::ALL {
            let mut listed = effects::builtins()
                .iter()
                .filter(|d| d.category == category)
                .peekable();
            if listed.peek().is_none() {
                continue;
            }
            ui.label(
                RichText::new(category.name())
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            for descriptor in listed {
                if ui
                    .button(descriptor.name)
                    .on_hover_text(descriptor.description)
                    .clicked()
                {
                    let effect = EffectRef::builtin(descriptor.id);
                    if let Some(id) =
                        session.edit("Add Effect", |p| p.add_device(owner, effect, None))
                    {
                        reveal(ui.ctx(), id);
                    }
                    ui.close();
                }
            }
        }
    });
}

fn reveal_id() -> egui::Id {
    egui::Id::new("reveal_effect")
}

/// Scrolls the device view to `id` next frame: a long chain runs off the side.
fn reveal(ctx: &egui::Context, id: DeviceId) {
    ctx.data_mut(|d| d.insert_temp(reveal_id(), id));
}

fn clipboard_id() -> egui::Id {
    egui::Id::new("effect clipboard")
}

/// Copied effects. Kept in egui's memory rather than a view, so they can
/// be pasted into another project.
fn clipboard(ctx: &egui::Context) -> Vec<CopiedDevice> {
    ctx.data(|d| d.get_temp(clipboard_id())).unwrap_or_default()
}

/// The chain the device view shows: a track's, or (`Some(None)`) the
/// master's.
fn shown_chain(timeline: &TimelineView) -> Option<Option<TrackId>> {
    if timeline.selection.master {
        Some(None)
    } else {
        timeline.selection.primary_track().map(Some)
    }
}

fn chain_ids(session: &Session, owner: Option<TrackId>) -> Vec<DeviceId> {
    session
        .project
        .devices(owner)
        .map(|devices| devices.iter().map(|d| d.id).collect())
        .unwrap_or_default()
}

/// "Remove Effect", or "Remove Effects" for more than one.
fn label(verb: &str, count: usize) -> String {
    if count == 1 {
        format!("{verb} Effect")
    } else {
        format!("{verb} Effects")
    }
}

/// An effect's name, or how many there are.
fn describe(session: &Session, ids: &[DeviceId]) -> String {
    match ids {
        [id] => session
            .project
            .device(*id)
            .map_or_else(|| "the effect".to_owned(), |d| d.name().to_owned()),
        _ => format!("{} effects", ids.len()),
    }
}

/// What the title bar's menu, its buttons and the keyboard do to the
/// selected effects.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Remove,
    Duplicate,
    Copy,
    Cut,
    Paste,
    /// One place left (-1) or right (1).
    Move(isize),
    /// Select the neighbour on the left (-1) or right (1).
    Select(isize),
    SelectAll,
    Fold,
    /// To the end of another chain (`None`: the master's).
    MoveTo(Option<TrackId>),
    CopyTo(Option<TrackId>),
}

fn run(ctx: &egui::Context, session: &mut Session, timeline: &mut TimelineView, op: Op) {
    let Some(owner) = shown_chain(timeline) else {
        return;
    };
    let chain = chain_ids(session, owner);
    let picked: Vec<DeviceId> = chain
        .iter()
        .copied()
        .filter(|id| timeline.selection.devices.contains(id))
        .collect();
    let first = chain.iter().position(|id| picked.contains(id));
    let last = chain.iter().rposition(|id| picked.contains(id));
    let count = picked.len();
    if picked.is_empty() && !matches!(op, Op::Paste | Op::Select(_) | Op::SelectAll) {
        return;
    }
    if matches!(op, Op::Duplicate | Op::Copy | Op::Cut) {
        // Copies carry plugins' state as it is now.
        session.refresh_plugin_states();
    }
    match op {
        Op::Remove => {
            // The selection passes to the next one along, or the one before.
            let next = last
                .and_then(|i| chain[i + 1..].first())
                .or_else(|| first.and_then(|i| chain[..i].last()))
                .copied();
            if session
                .edit(&label("Remove", count), |p| p.remove_devices(&picked))
                .is_some()
            {
                timeline.selection.devices = next.into_iter().collect();
            }
        }
        Op::Duplicate => {
            if let Some(copies) =
                session.edit(&label("Duplicate", count), |p| p.duplicate_devices(&picked))
            {
                if let Some(id) = copies.first() {
                    reveal(ctx, *id);
                }
                timeline.selection.devices = copies;
            }
        }
        Op::Copy | Op::Cut => {
            let copied = session.project.copy_devices(&picked);
            ctx.data_mut(|d| d.insert_temp(clipboard_id(), copied));
            // Ctrl+V only reaches us as a paste when the system clipboard
            // holds text, so leave some there.
            ctx.copy_text("canopy effects".to_owned());
            if op == Op::Cut
                && session
                    .edit(&label("Cut", count), |p| p.remove_devices(&picked))
                    .is_some()
            {
                timeline.selection.devices.clear();
            }
        }
        Op::Paste => {
            let copied = clipboard(ctx);
            if copied.is_empty() {
                return;
            }
            // After the selection, or at the end.
            let at = last.map(|i| i + 1);
            if let Some(ids) = session.edit(&label("Paste", copied.len()), |p| {
                p.insert_devices(owner, at, &copied)
            }) {
                if let Some(id) = ids.first() {
                    reveal(ctx, *id);
                }
                timeline.selection.devices = ids;
            }
        }
        Op::Move(step) => {
            let (Some(first), Some(last)) = (first, last) else {
                return;
            };
            // Counted before they leave their places.
            let to = if step < 0 && first > 0 {
                first - 1
            } else if step > 0 && last + 1 < chain.len() {
                last + 2
            } else {
                return;
            };
            session.edit(&label("Move", count), |p| {
                p.move_devices(&picked, owner, to)
            });
        }
        Op::Select(step) => {
            if chain.is_empty() {
                return;
            }
            let index = match (first, last) {
                (Some(first), _) if step < 0 => first.saturating_sub(1),
                (_, Some(last)) if step > 0 => (last + 1).min(chain.len() - 1),
                _ if step < 0 => chain.len() - 1,
                _ => 0,
            };
            timeline.selection.devices = vec![chain[index]];
            reveal(ctx, chain[index]);
        }
        Op::SelectAll => timeline.selection.devices = chain,
        Op::Fold => {
            // Folds them all, or opens them all if they all are folded.
            let folded = picked
                .iter()
                .all(|id| session.project.device(*id).is_some_and(|d| d.folded));
            for id in picked {
                session.view_edit(|p| p.set_device_folded(id, !folded));
            }
        }
        Op::MoveTo(to) => {
            drop_devices(session, timeline, &picked, to, None, false);
        }
        Op::CopyTo(to) => {
            drop_devices(session, timeline, &picked, to, None, true);
        }
    }
}

/// Effects dropped on a chain: moved there to `index` (counted before
/// they leave; the end if `None`), or copied there. Effects that stay in
/// their chain stay selected; ones sent elsewhere are reported. Returns the
/// ids of the effects in their new places.
pub(crate) fn drop_devices(
    session: &mut Session,
    timeline: &mut TimelineView,
    ids: &[DeviceId],
    to: Option<TrackId>,
    index: Option<usize>,
    copy: bool,
) -> Vec<DeviceId> {
    let Some((from, _)) = ids.first().and_then(|id| session.project.find_device(*id)) else {
        return Vec::new();
    };
    let Some(len) = session.project.devices(to).map(<[Device]>::len) else {
        return Vec::new();
    };
    let index = index.unwrap_or(len);
    let what = describe(session, ids);
    let placed = if copy {
        session.refresh_plugin_states();
        let copied = session.project.copy_devices(ids);
        session.edit(&label("Copy", ids.len()), |p| {
            p.insert_devices(to, Some(index), &copied)
        })
    } else {
        session
            .edit(&label("Move", ids.len()), |p| {
                p.move_devices(ids, to, index)
            })
            .map(|()| ids.to_vec())
    };
    let Some(placed) = placed else {
        return Vec::new();
    };
    if from == to {
        timeline.selection.devices = placed.clone();
    } else {
        let name = match to {
            Some(track) => session
                .project
                .track(track)
                .map_or_else(String::new, |t| t.name.clone()),
            None => "the master".to_owned(),
        };
        let verb = if copy { "Copied" } else { "Moved" };
        session.info(format!("{verb} {what} to {name}"));
    }
    placed
}

/// The keyboard, while the device view was clicked last: Delete removes the
/// selected effects (or a slicer's picked marker), Ctrl+D duplicates them, Ctrl+C, X and V copy, cut and
/// paste, Left and Right select the neighbours, Ctrl+Left and Right move
/// the selection, Ctrl+A selects the whole chain, Escape nothing.
pub(crate) fn shortcuts(ctx: &egui::Context, session: &mut Session, timeline: &mut TimelineView) {
    // The arrows are a focused knob's, if there is one.
    let shortcut = |modifiers, key| {
        !crate::widgets::knob::takes_key(ctx, key)
            && ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(modifiers, key)))
    };
    let ctrl = Modifiers::COMMAND;
    let mut ops = Vec::new();
    if (shortcut(Modifiers::NONE, Key::Delete) || shortcut(Modifiers::NONE, Key::Backspace))
        && !crate::devices::slicer_delete_marker(ctx, session)
    {
        ops.push(Op::Remove);
    }
    if shortcut(ctrl, Key::D) {
        ops.push(Op::Duplicate);
    }
    let (copy, cut, paste) = crate::app::clipboard_events(ctx);
    if copy {
        ops.push(Op::Copy);
    }
    if cut {
        ops.push(Op::Cut);
    }
    if paste {
        ops.push(Op::Paste);
    }
    // Ctrl first: the plain arrows would match these too.
    for (key, step) in [(Key::ArrowLeft, -1), (Key::ArrowRight, 1)] {
        if shortcut(ctrl, key) {
            ops.push(Op::Move(step));
        } else if shortcut(Modifiers::NONE, key) {
            ops.push(Op::Select(step));
        }
    }
    if shortcut(ctrl, Key::A) {
        ops.push(Op::SelectAll);
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        timeline.selection.devices.clear();
    }
    for op in ops {
        run(ctx, session, timeline, op);
    }
}

/// Every effect in `owner`'s chain (`None`: the master).
pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    timeline: &mut TimelineView,
    owner: Option<TrackId>,
    color: Color32,
) {
    let devices = session
        .project
        .devices(owner)
        .map(<[Device]>::to_vec)
        .unwrap_or_default();
    timeline
        .selection
        .devices
        .retain(|id| devices.iter().any(|d| d.id == *id));
    crate::patcher::rack_banner(ui, session, timeline, owner);
    let drag = egui::DragAndDrop::payload::<DeviceDrag>(ui.ctx());
    let reveal_target: Option<DeviceId> = ui.ctx().data(|d| d.get_temp(reveal_id()));
    let mut slots = Vec::new();
    let mut ops = Vec::new();
    for (index, device) in devices.iter().enumerate() {
        ui.add_space(space::L + space::S);
        let (rect, op) = slot(
            ui,
            session,
            timeline,
            owner,
            &devices,
            index,
            color,
            drag.as_deref(),
        );
        if reveal_target == Some(device.id) {
            ui.scroll_to_rect(rect, Some(egui::Align::Min));
            ui.ctx().data_mut(|d| d.remove::<DeviceId>(reveal_id()));
        }
        slots.push(rect);
        ops.extend(op);
    }
    // Room after the last effect, to drop one at the end. With no effects
    // yet, it says how to add one.
    let height = slots
        .iter()
        .map(Rect::height)
        .fold(BAR_HEIGHT * 2.0, f32::max);
    let tail = if devices.is_empty() {
        ui.add_space(space::L);
        let size = vec2(220.0, height.max(ui.available_height().min(160.0)));
        let (tail, _) = ui.allocate_exact_size(size, Sense::hover());
        let visuals = ui.visuals();
        ui.painter().rect_stroke(
            tail.shrink(space::S),
            radius::PANEL,
            egui::Stroke::new(
                stroke::HAIRLINE,
                visuals.widgets.noninteractive.bg_stroke.color,
            ),
            StrokeKind::Inside,
        );
        chrome::empty_hint(
            ui.painter(),
            tail,
            "No effects",
            "+ Effect adds one; or drag one here",
        );
        tail
    } else {
        ui.allocate_exact_size(vec2(48.0, height), Sense::hover()).0
    };
    if let Some(drag) = drag {
        drop_in_chain(ui, session, timeline, owner, &slots, tail, &drag);
        ghost(ui, session, &drag);
    }
    for op in ops {
        run(ui.ctx(), session, timeline, op);
    }
}

/// One effect: its title bar and, unless folded, its panel. Returns where
/// it is and what its bar asked for.
#[allow(clippy::too_many_arguments)]
fn slot(
    ui: &mut egui::Ui,
    session: &mut Session,
    timeline: &mut TimelineView,
    owner: Option<TrackId>,
    devices: &[Device],
    index: usize,
    color: Color32,
    drag: Option<&DeviceDrag>,
) -> (Rect, Option<Op>) {
    let device = &devices[index];
    let id = device.id;
    let selected = timeline.selection.devices.contains(&id);
    let dragged = drag.is_some_and(|d| d.ids.contains(&id));
    // Last frame's widths, so the bar spans the panel and its buttons sit
    // at its right end.
    let widths_id = egui::Id::new(("effect widths", id));
    let (slot_width, body_width): (f32, f32) =
        ui.data(|d| d.get_temp(widths_id)).unwrap_or((200.0, 0.0));
    let inner = ui.vertical(|ui| {
        if dragged {
            ui.set_opacity(0.4);
        }
        let background = ui.painter().add(Shape::Noop);
        // Registered before the bar's own buttons, which take their clicks;
        // the rest of the bar is this.
        let bar_rect = Rect::from_min_size(ui.cursor().min, vec2(slot_width, BAR_HEIGHT));
        let bar = ui.interact(
            bar_rect,
            egui::Id::new(("effect bar", id)),
            Sense::click_and_drag(),
        );
        let mut op = title_bar(ui, session, timeline, owner, devices, index, body_width);
        let modifiers = ui.input(|i| i.modifiers);
        if bar.clicked() {
            click_select(timeline, devices, id, modifiers);
        }
        if bar.double_clicked() {
            session.view_edit(|p| p.set_device_folded(id, !device.folded));
        }
        if bar.drag_started() {
            if !timeline.selection.devices.contains(&id) {
                timeline.selection.devices = vec![id];
            }
            let ids = devices
                .iter()
                .map(|d| d.id)
                .filter(|d| timeline.selection.devices.contains(d))
                .collect();
            egui::DragAndDrop::set_payload(ui.ctx(), DeviceDrag { ids });
        }
        if bar.secondary_clicked() && !timeline.selection.devices.contains(&id) {
            timeline.selection.devices = vec![id];
        }
        if let Some(chosen) = egui::Popup::context_menu(&bar)
            .show(|ui| menu(ui, session, timeline, owner, devices, index))
            .and_then(|r| r.inner)
        {
            op = Some(chosen);
        }
        // Folding and unfolding slide the panel shut or open.
        let open = theme::fade(ui.ctx(), egui::Id::new(("effect open", id)), !device.folded);
        let panel = |ui: &mut egui::Ui, session: &mut Session, timeline: &mut TimelineView| {
            if device.bypassed {
                ui.set_opacity(0.5);
            }
            match Panel::effect(session, timeline, owner, device, color) {
                Some(mut panel) => {
                    devices::effect(ui, &mut panel);
                    panel.animate(ui);
                }
                None => {
                    if !crate::plugins::missing(ui, session, owner, device) {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "{} isn't available; the sound passes through.",
                                device.effect.id
                            ),
                        );
                    }
                }
            }
        };
        let open_width_id = egui::Id::new(("effect open width", id));
        // How wide it was when last open (unknown for one folded since the
        // project was loaded: that one just appears).
        let full: Option<f32> = ui.data(|d| d.get_temp(open_width_id));
        let body = if open <= 0.0 {
            0.0
        } else if open >= 1.0 || full.is_none() {
            let width = ui
                .vertical(|ui| panel(ui, session, timeline))
                .response
                .rect
                .width();
            ui.data_mut(|d| d.insert_temp(open_width_id, width));
            width
        } else {
            // Mid-fold: the panel at its full width, seen through a window
            // as wide as the fold has opened.
            let full = full.unwrap_or(body_width);
            let shown = (full * open).max(1.0);
            let top_left = ui.cursor().min;
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        top_left,
                        vec2(full.max(1.0), ui.available_height()),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let window = Rect::from_min_size(top_left, vec2(shown, child.available_height()));
            child.set_clip_rect(child.clip_rect().intersect(window));
            child.multiply_opacity(open);
            panel(&mut child, session, timeline);
            let height = child.min_rect().height();
            ui.allocate_exact_size(vec2(shown, height), Sense::hover());
            shown
        };
        (background, bar.hovered() || bar.dragged(), op, body)
    });
    let (background, hovered, op, body) = inner.inner;
    let rect = inner.response.rect;
    ui.data_mut(|d| d.insert_temp(widths_id, (rect.width(), body)));

    let bar_id = egui::Id::new(("effect bar", id));
    let fill = chrome::bar_fill(ui, bar_id, hovered, selected);
    // Each effect sits on a card of its own, its title bar across the top.
    ui.painter().set(
        background,
        chrome::card(ui.visuals(), rect, BAR_HEIGHT, fill),
    );
    let outline =
        ui.ctx()
            .animate_bool_with_time(bar_id.with("outline"), selected, theme::anim::NORMAL);
    if outline > 0.0 {
        let accent = theme::accent(ui.visuals());
        ui.painter().rect_stroke(
            rect.expand(space::S),
            radius::PANEL,
            Stroke::new(stroke::HAIRLINE, accent.gamma_multiply(0.7 * outline)),
            StrokeKind::Outside,
        );
    }
    (rect, op)
}

/// Click selects one effect; Ctrl-click adds or removes one; Shift-click
/// selects the run from the last one selected.
fn click_select(
    timeline: &mut TimelineView,
    devices: &[Device],
    id: DeviceId,
    modifiers: Modifiers,
) {
    let selection = &mut timeline.selection.devices;
    if modifiers.command {
        if let Some(at) = selection.iter().position(|d| *d == id) {
            selection.remove(at);
        } else {
            selection.push(id);
        }
    } else if modifiers.shift
        && let Some(anchor) = selection.last().copied()
    {
        let position = |id: DeviceId| devices.iter().position(|d| d.id == id);
        if let (Some(a), Some(b)) = (position(anchor), position(id)) {
            let (low, high) = (a.min(b), a.max(b));
            *selection = devices[low..=high].iter().map(|d| d.id).collect();
            // Keep the anchor where Shift-clicks start from.
            selection.retain(|d| *d != anchor);
            selection.push(anchor);
        }
    } else {
        *selection = vec![id];
    }
}

/// The bar's contents: fold arrow, on switch, name, sidechain input and
/// lateness at the left; duplicate, menu and remove at the right end of
/// `room` (the panel's width). Returns what its buttons asked for.
fn title_bar(
    ui: &mut egui::Ui,
    session: &mut Session,
    timeline: &mut TimelineView,
    owner: Option<TrackId>,
    devices: &[Device],
    index: usize,
    room: f32,
) -> Option<Op> {
    let device = &devices[index];
    let id = device.id;
    let mut op = None;
    ui.horizontal(|ui| {
        ui.set_min_height(BAR_HEIGHT);
        let start = ui.cursor().min.x;
        ui.add_space(space::XS);
        if chrome::fold_arrow(ui, device.folded).clicked() {
            session.view_edit(|p| p.set_device_folded(id, !device.folded));
        }
        let on = !device.bypassed;
        let lit = theme::accent(ui.visuals()).gamma_multiply(0.8);
        if chrome::toggle(
            ui,
            if on { "On" } else { "Off" },
            on,
            lit,
            "On, or bypassed (the sound passes through)",
        )
        .clicked()
        {
            session.edit("Bypass Effect", |p| p.set_device_bypassed(id, on));
        }
        // Not interactive: clicks and drags on the name are the bar's.
        ui.add(egui::Label::new(RichText::new(device.name()).strong()).selectable(false));
        if device.effect.descriptor().is_some_and(|d| d.sidechain) {
            sidechain_picker(ui, session, owner, device);
        }
        let rate = session.sample_rate() as f32;
        let latency = device
            .effect
            .descriptor()
            .map_or(0, |d| (d.latency)(&device.values(), rate));
        if latency > 0 {
            ui.weak(format!("{:.1} ms", latency as f32 / rate * 1000.0))
                .on_hover_text(
                    "How late it plays its sound; the other tracks wait as long to stay in time",
                );
        }
        crate::plugins::title_bar_extras(ui, session, owner, device);
        let spacing = ui.spacing().item_spacing.x;
        let buttons = 3.0 * ICON + 2.0 * spacing;
        let used = ui.cursor().min.x - start;
        ui.add_space((room - used - buttons - spacing).max(0.0));
        let selected = timeline.selection.devices.contains(&id);
        // A button acts on the selection if this effect is in it, else on
        // this one alone.
        let mut act = |timeline: &mut TimelineView, chosen: Op| {
            if !selected {
                timeline.selection.devices = vec![id];
            }
            op = Some(chosen);
        };
        let duplicate = chrome::icon_button(ui, Icon::Duplicate, "Duplicate effect", "");
        if chrome::shortcut_tip(duplicate, "Duplicate", "Ctrl+D").clicked() {
            act(timeline, Op::Duplicate);
        }
        let more = chrome::icon_button(
            ui,
            Icon::Menu,
            "Effect menu",
            "Presets, copy, move… (or right-click the bar)",
        );
        if more.clicked() && !selected {
            timeline.selection.devices = vec![id];
        }
        if let Some(chosen) = egui::Popup::menu(&more)
            .show(|ui| menu(ui, session, timeline, owner, devices, index))
            .and_then(|r| r.inner)
        {
            act(timeline, chosen);
        }
        let remove = chrome::icon_button(ui, Icon::Remove, "Remove effect", "");
        if chrome::shortcut_tip(remove, "Remove", "Delete").clicked() {
            act(timeline, Op::Remove);
        }
    });
    op
}

/// The title bar's menu (also on right-click): this effect's presets, then
/// what can be done to the selected effects.
fn menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    timeline: &TimelineView,
    owner: Option<TrackId>,
    devices: &[Device],
    index: usize,
) -> Option<Op> {
    let device = &devices[index];
    let mut op = None;
    devices::preset_menu::menu(ui, session, PresetTarget::Device(device.id));
    ui.separator();
    if chrome::menu_item(ui, "Duplicate", "Ctrl+D").clicked() {
        op = Some(Op::Duplicate);
    }
    if chrome::menu_item(ui, "Copy", "Ctrl+C").clicked() {
        op = Some(Op::Copy);
    }
    if chrome::menu_item(ui, "Cut", "Ctrl+X").clicked() {
        op = Some(Op::Cut);
    }
    let can_paste = !clipboard(ui.ctx()).is_empty();
    if chrome::menu_item_enabled(ui, can_paste, "Paste", "Ctrl+V").clicked() {
        op = Some(Op::Paste);
    }
    ui.separator();
    let selected = |d: &&Device| timeline.selection.devices.contains(&d.id);
    let first = devices.iter().position(|d| selected(&d)).unwrap_or(index);
    let last = devices.iter().rposition(|d| selected(&d)).unwrap_or(index);
    if chrome::menu_item_enabled(ui, first > 0, "Move Left", "Ctrl+Left").clicked() {
        op = Some(Op::Move(-1));
    }
    if chrome::menu_item_enabled(ui, last + 1 < devices.len(), "Move Right", "Ctrl+Right").clicked()
    {
        op = Some(Op::Move(1));
    }
    ui.menu_button("Move to", |ui| {
        if let Some(to) = chains_menu(ui, session, owner) {
            op = Some(Op::MoveTo(to));
        }
    });
    ui.menu_button("Copy to", |ui| {
        if let Some(to) = chains_menu(ui, session, owner) {
            op = Some(Op::CopyTo(to));
        }
    });
    ui.separator();
    if ui
        .button(if device.folded { "Unfold" } else { "Fold" })
        .clicked()
    {
        op = Some(Op::Fold);
    }
    if chrome::menu_item(ui, "Remove", "Delete").clicked() {
        op = Some(Op::Remove);
    }
    if op.is_some() {
        ui.close();
    }
    op
}

/// Every chain but `owner`'s: the master, then the tracks as nested.
fn chains_menu(
    ui: &mut egui::Ui,
    session: &Session,
    owner: Option<TrackId>,
) -> Option<Option<TrackId>> {
    let mut chosen = None;
    if owner.is_some() && ui.button("Master").clicked() {
        chosen = Some(None);
    }
    session.project.visit(|track, depth| {
        if Some(track.id) == owner {
            return;
        }
        let name = format!("{}{}", "   ".repeat(depth), track.name);
        if ui
            .button(RichText::new(name).color(color32(track.color)))
            .clicked()
        {
            chosen = Some(Some(track.id));
        }
    });
    chosen
}

/// While effects are dragged over the chain: a line where they'd land,
/// and on release, the move (or with Ctrl, the copy).
fn drop_in_chain(
    ui: &egui::Ui,
    session: &mut Session,
    timeline: &mut TimelineView,
    owner: Option<TrackId>,
    slots: &[Rect],
    tail: Rect,
    drag: &DeviceDrag,
) {
    let Some(pointer) = ui.ctx().pointer_hover_pos() else {
        return;
    };
    let area = slots.iter().fold(tail, |area, slot| area.union(*slot));
    if !area.expand(4.0).contains(pointer) {
        return;
    }
    // Before the first effect whose middle is right of the pointer.
    let index = slots
        .iter()
        .position(|slot| pointer.x < slot.center().x)
        .unwrap_or(slots.len());
    let x = match (slots.get(index), slots.last()) {
        (Some(slot), _) => slot.left() - 4.0,
        (None, Some(last)) => last.right() + 4.0,
        (None, None) => tail.left(),
    };
    // It slides from gap to gap as the pointer moves along the chain.
    let x = theme::glide_marker(ui.ctx(), egui::Id::new("effect drop line"), x);
    let accent = theme::accent(ui.visuals());
    ui.painter()
        .vline(x, area.y_range(), Stroke::new(stroke::INDICATOR, accent));
    // A notch at each end of the line, so it reads as "between".
    for y in [area.top(), area.bottom()] {
        ui.painter()
            .hline(x - 4.0..=x + 4.0, y, Stroke::new(stroke::INDICATOR, accent));
    }
    if ui.input(|i| i.pointer.any_released()) {
        let copy = ui.input(|i| i.modifiers.command);
        egui::DragAndDrop::clear_payload(ui.ctx());
        let placed = drop_devices(session, timeline, &drag.ids, owner, Some(index), copy);
        if let Some(id) = placed.first() {
            reveal(ui.ctx(), *id);
        }
    }
}

/// What's being dragged, by the pointer: its name (or how many), with a +
/// when Ctrl copies it. A drop target under the pointer says more instead.
fn ghost(ui: &egui::Ui, session: &Session, drag: &DeviceDrag) {
    let ctx = ui.ctx();
    let Some(pointer) = ctx.pointer_hover_pos() else {
        return;
    };
    let copy = ctx.input(|i| i.modifiers.command);
    ctx.set_cursor_icon(if copy {
        CursorIcon::Copy
    } else {
        CursorIcon::Grabbing
    });
    let text = format!(
        "{}{}",
        if copy { "+ " } else { "" },
        describe(session, &drag.ids)
    );
    chrome::drag_label(ctx, pointer, &text);
}

/// Which track the effect listens to: a menu of every other track.
fn sidechain_picker(
    ui: &mut egui::Ui,
    session: &mut Session,
    owner: Option<TrackId>,
    device: &Device,
) {
    let source = device.sidechain.and_then(|t| session.project.track(t));
    let text = match source {
        Some(track) => RichText::new(format!("SC: {}", track.name)).color(color32(track.color)),
        None => RichText::new("Sidechain"),
    };
    let mut choice = None;
    ui.menu_button(text, |ui| {
        if ui.selectable_label(source.is_none(), "None").clicked() {
            choice = Some(None);
            ui.close();
        }
        ui.separator();
        session.project.visit(|track, depth| {
            if Some(track.id) == owner {
                return;
            }
            let name = format!("{}{}", "   ".repeat(depth), track.name);
            let label = RichText::new(name).color(color32(track.color));
            if ui
                .selectable_label(device.sidechain == Some(track.id), label)
                .clicked()
            {
                choice = Some(Some(track.id));
                ui.close();
            }
        });
    })
    .response
    .on_hover_text("The track this effect listens to, after its effects and before its fader");
    if let Some(source) = choice {
        let id = device.id;
        session.edit("Route Sidechain", |p| p.set_device_sidechain(id, source));
    }
}
