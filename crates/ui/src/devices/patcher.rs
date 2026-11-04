//! The Patcher's panel: its outside, the eight macro knobs (named as the
//! user names them, dim while they drive nothing), and Open, which shows
//! the effects inside in the device view (as a rack or as a graph).
//!
//! Macros are linked from the knobs inside: right-click one, Map to Macro.
//! A macro's own right-click menu names it and lists what it drives, each
//! link with the range its knob sweeps as the macro goes from 0 to 100%.

use eframe::egui::{self, Ui};
use instruments::ParamDescriptor;
use project::{Chain, DeviceId, MACROS, macro_index, macro_param};

use super::Panel;
use super::panel::Owner;
use crate::session::Session;
use crate::theme::space;

pub(super) fn show(ui: &mut Ui, panel: &mut Panel) {
    let Owner::Device(id) = panel.owner else {
        return;
    };
    let Some(patch) = panel.session.project.patch(id).cloned() else {
        return;
    };
    ui.horizontal(|ui| {
        let inside = match patch.devices.len() {
            0 => "Empty".to_owned(),
            1 => "1 effect".to_owned(),
            n => format!("{n} effects"),
        };
        ui.weak(inside);
        if ui
            .button("Open")
            .on_hover_text("Show the effects inside, in a row or as a graph")
            .clicked()
        {
            panel.timeline.open_patcher = Some(id);
            panel.timeline.selection.devices.clear();
        }
    });
    ui.add_space(space::XS);
    let used = patch.used_macros();
    for row in [0..MACROS / 2, MACROS / 2..MACROS] {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            for index in row {
                let name = patch.macro_name(index);
                let mut child = ui.new_child(egui::UiBuilder::new());
                if !used.contains(&index) {
                    child.multiply_opacity(0.5);
                }
                let response = panel.knob_at(&mut child, index, Some(&name), None);
                let rect = child.min_rect();
                ui.allocate_rect(rect, egui::Sense::hover());
                if !used.contains(&index) {
                    response.on_hover_text("Right-click a knob inside, Map to Macro");
                }
            }
        });
    }
}

/// What a knob's right-click menu adds for Patchers: on a macro, its name
/// and its links; on a knob inside a Patcher, Map to Macro.
pub(crate) fn menu_items(ui: &mut Ui, session: &mut Session, owner: Owner, param: &ParamDescriptor) {
    let Owner::Device(device) = owner else {
        return;
    };
    if let Some(index) = macro_index(param.id)
        && session.project.patch(device).is_some()
    {
        macro_items(ui, session, device, index);
        ui.separator();
    }
    if let Some((Chain::Patch(patcher), _)) = session.project.locate_device(device) {
        ui.menu_button("Map to Macro", |ui| {
            map_items(ui, session, patcher, device, param);
        });
        ui.separator();
    }
}

/// Macro `index` of `patcher`: its name, and each link with its range.
fn macro_items(ui: &mut Ui, session: &mut Session, patcher: DeviceId, index: usize) {
    let Some(patch) = session.project.patch(patcher).cloned() else {
        return;
    };
    let name_id = egui::Id::new(("macro name", patcher, index));
    let mut name: String = ui
        .data(|d| d.get_temp(name_id))
        .unwrap_or_else(|| patch.names.get(index).cloned().unwrap_or_default());
    let field = ui.add(
        egui::TextEdit::singleline(&mut name)
            .hint_text(format!("Macro {}", index + 1))
            .desired_width(140.0),
    );
    // Ready to type as the menu opens.
    if ui.memory(|m| m.focused().is_none()) && !field.lost_focus() {
        field.request_focus();
    }
    if field.changed() {
        ui.data_mut(|d| d.insert_temp(name_id, name.clone()));
    }
    if field.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(name_id));
        if patch.names.get(index).map_or("", String::as_str) != name.trim() {
            session.edit("Rename Macro", |p| p.rename_macro(patcher, index, &name));
        }
    }
    let links: Vec<_> = patch
        .links
        .iter()
        .filter(|l| usize::from(l.index) == index)
        .cloned()
        .collect();
    if links.is_empty() {
        ui.weak("Drives nothing yet");
        return;
    }
    for link in links {
        let Some(device) = session.project.device(link.device) else {
            continue;
        };
        let knob = device
            .effect
            .descriptor()
            .and_then(|d| d.params.iter().find(|p| p.id == link.param))
            .map_or(link.param.as_str(), |p| p.name);
        let label = format!("{}: {knob}", device.name());
        ui.horizontal(|ui| {
            ui.label(label);
            let (mut min, mut max) = (link.min * 100.0, link.max * 100.0);
            let from = ui
                .add(egui::DragValue::new(&mut min).range(0.0..=100.0).suffix("%").speed(0.5))
                .on_hover_text("Where the knob is with the macro at 0%");
            ui.weak("to");
            let to = ui
                .add(egui::DragValue::new(&mut max).range(0.0..=100.0).suffix("%").speed(0.5))
                .on_hover_text("Where the knob is with the macro at 100%");
            if from.changed() || to.changed() {
                session.live_macro_link(
                    patcher,
                    index,
                    link.device,
                    &link.param,
                    (min / 100.0, max / 100.0),
                );
            }
            let done = |r: &egui::Response| r.drag_stopped() || r.lost_focus();
            if done(&from) || done(&to) {
                session.end_live("Macro Range");
            }
            if ui.small_button("Unmap").clicked() {
                let (device, param) = (link.device, link.param.clone());
                session.edit("Unmap Macro", |p| {
                    p.unlink_macro(patcher, index, device, &param)
                });
                ui.close();
            }
        });
    }
}

/// The macros of the Patcher a knob is in, to link it to (or unlink it
/// from). A macro that drives nothing yet takes the knob's place, so the
/// knob doesn't jump.
fn map_items(
    ui: &mut Ui,
    session: &mut Session,
    patcher: DeviceId,
    device: DeviceId,
    param: &ParamDescriptor,
) {
    let Some(patch) = session.project.patch(patcher).cloned() else {
        return;
    };
    let used = patch.used_macros();
    for index in 0..MACROS {
        let linked = patch
            .links
            .iter()
            .any(|l| usize::from(l.index) == index && l.device == device && l.param == param.id);
        if ui.selectable_label(linked, patch.macro_name(index)).clicked() {
            let id = param.id;
            if linked {
                session.edit("Unmap Macro", |p| p.unlink_macro(patcher, index, device, id));
            } else {
                let place = session
                    .project
                    .device(device)
                    .and_then(|d| d.param(id))
                    .map_or(0.0, |v| param.to_normalized(v));
                let fresh = !used.contains(&index);
                session.edit("Map to Macro", |p| {
                    if fresh {
                        p.set_device_param(patcher, macro_param(index), place * 100.0)?;
                    }
                    p.link_macro(patcher, index, device, id, 0.0, 1.0)
                });
            }
            ui.close();
        }
    }
}
