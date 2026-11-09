//! Shaper's visual editor and lock-free delivery of GUI edits to the host.

use crate::{
    device::Display,
    values::{Values, to_host},
};
use canopy_widgets::{shape::ShapeEditor, theme};
use clap_sys::events::*;
use effects::shaper::{self, MODULES, POINTS};
use instruments::ParameterKind;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};

const BEGIN: u8 = 1;
const VALUE: u8 = 2;
const END: u8 = 4;

struct Edit {
    flags: AtomicU8,
    plain: AtomicU32,
    active: AtomicBool,
}

/// Coalesces GUI edits per parameter, without allocating or locking in
/// process/flush. Rejected host events stay pending, including gesture ends.
pub struct Edits {
    edits: Box<[Edit]>,
    pending: AtomicBool,
    pub dirty: AtomicBool,
}
impl Edits {
    pub fn new(count: usize) -> Self {
        Self {
            edits: (0..count)
                .map(|_| Edit {
                    flags: AtomicU8::new(0),
                    plain: AtomicU32::new(0),
                    active: AtomicBool::new(false),
                })
                .collect(),
            pending: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
        }
    }
    pub fn set(&self, values: &Values, index: usize, plain: f32) {
        let Some(edit) = self.edits.get(index) else {
            return;
        };
        let param = &values.params()[index];
        let plain = param.clamp(if plain.is_nan() {
            param.default_value()
        } else {
            plain
        });
        values.send_plain(index, plain);
        edit.plain.store(plain.to_bits(), Ordering::Relaxed);
        let begin = if edit.active.swap(true, Ordering::AcqRel) {
            0
        } else {
            BEGIN
        };
        edit.flags.fetch_or(begin | VALUE, Ordering::Release);
        self.pending.store(true, Ordering::Release);
        self.dirty.store(true, Ordering::Release);
    }
    pub fn finish(&self, index: usize) {
        if let Some(edit) = self.edits.get(index)
            && edit.active.swap(false, Ordering::AcqRel)
        {
            edit.flags.fetch_or(END, Ordering::Release);
            self.pending.store(true, Ordering::Release);
        }
    }
    pub fn finish_all(&self) {
        for index in 0..self.edits.len() {
            self.finish(index);
        }
    }
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// # Safety
    /// `out` is null or a valid CLAP output-event list for this callback.
    pub unsafe fn emit(&self, values: &Values, out: *const clap_output_events) {
        let Some(list) = (unsafe { out.as_ref() }) else {
            return;
        };
        let Some(push) = list.try_push else { return };
        if !self.pending.swap(false, Ordering::AcqRel) {
            return;
        }
        for (index, edit) in self.edits.iter().enumerate() {
            let mut flags = edit.flags.swap(0, Ordering::AcqRel);
            for (flag, kind) in [
                (BEGIN, CLAP_EVENT_PARAM_GESTURE_BEGIN),
                (VALUE, CLAP_EVENT_PARAM_VALUE),
                (END, CLAP_EVENT_PARAM_GESTURE_END),
            ] {
                if flags & flag == 0 {
                    continue;
                }
                let header = clap_event_header {
                    size: if flag == VALUE {
                        size_of::<clap_event_param_value>()
                    } else {
                        size_of::<clap_event_param_gesture>()
                    } as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: kind,
                    flags: CLAP_EVENT_IS_LIVE,
                };
                let accepted = if flag == VALUE {
                    let value = f32::from_bits(edit.plain.load(Ordering::Relaxed));
                    let event = clap_event_param_value {
                        header,
                        param_id: index as u32,
                        cookie: std::ptr::null_mut(),
                        note_id: -1,
                        port_index: -1,
                        channel: -1,
                        key: -1,
                        value: to_host(&values.params()[index], value),
                    };
                    unsafe { push(out, &event.header) }
                } else {
                    let event = clap_event_param_gesture {
                        header,
                        param_id: index as u32,
                    };
                    unsafe { push(out, &event.header) }
                };
                if !accepted {
                    break;
                }
                flags &= !flag;
            }
            if flags != 0 {
                edit.flags.fetch_or(flags, Ordering::Release);
                self.pending.store(true, Ordering::Release);
            }
        }
    }
}

pub struct ShaperEditor {
    pub values: Arc<Values>,
    pub display: Arc<Display>,
    pub edits: Arc<Edits>,
    module: usize,
}
impl ShaperEditor {
    pub fn new(values: Arc<Values>, display: Arc<Display>, edits: Arc<Edits>) -> Self {
        Self {
            values,
            display,
            edits,
            module: 0,
        }
    }
    fn set(&self, index: usize, value: f32, finish: bool) {
        self.edits.set(&self.values, index, value);
        if finish {
            self.edits.finish(index);
        }
    }
    fn control(&self, ui: &mut egui::Ui, index: usize, label: &str) -> bool {
        ui.push_id(("shaper parameter", index), |ui| {
            self.control_inner(ui, index, label)
        })
        .inner
    }
    fn control_inner(&self, ui: &mut egui::Ui, index: usize, label: &str) -> bool {
        let param = &self.values.params()[index];
        let mut plain = self
            .values
            .plain(index)
            .unwrap_or_else(|| param.default_value());
        match param.kind {
            ParameterKind::Enum { labels, .. } => {
                let mut changed = false;
                egui::ComboBox::from_id_salt(("parameter", index))
                    .selected_text(labels[plain as usize])
                    .show_ui(ui, |ui| {
                        for (i, text) in labels.iter().enumerate() {
                            if ui.selectable_value(&mut plain, i as f32, *text).changed() {
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text(param.name);
                if changed {
                    self.set(index, plain, true);
                }
                changed
            }
            ParameterKind::Float { .. } => {
                let mut normalized = param.to_normalized(plain);
                let response = ui
                    .add(
                        egui::Slider::new(&mut normalized, 0.0..=1.0)
                            .text(label)
                            .custom_formatter(move |v, _| {
                                param.format(param.from_normalized(v as f32))
                            })
                            .custom_parser(move |text| {
                                param.parse(text).map(|v| f64::from(param.to_normalized(v)))
                            }),
                    )
                    .on_hover_text(param.name);
                if response.changed() {
                    self.set(index, param.from_normalized(normalized), false);
                }
                if response.drag_stopped() || (response.changed() && !response.dragged()) {
                    self.edits.finish(index);
                }
                response.changed() || response.drag_stopped()
            }
        }
    }
    /// Returns whether GUI changes need a host callback. Host automation and
    /// restored state are read from the same parameter store every frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.heading("Canopy Shaper");
        ui.label("Draw a shape · right-click the curve for presets");
        ui.add_space(theme::space::M);
        ui.horizontal(|ui| {
            for (module, name) in ["Volume", "Filter", "Pan", "Time"]
                .iter()
                .enumerate()
                .take(MODULES)
            {
                ui.selectable_value(&mut self.module, module, *name);
            }
            ui.separator();
            let index = shaper::param(self.module, shaper::ON);
            let mut on = self.values.plain(index).unwrap_or_default() >= 0.5;
            if ui.checkbox(&mut on, "Enabled").changed() {
                self.set(index, if on { 1.0 } else { 0.0 }, true);
                changed = true;
            }
            ui.label("Rate");
            changed |= self.control(ui, shaper::param(self.module, shaper::RATE), "Rate");
        });
        let values: Vec<f32> = (0..self.values.len())
            .map(|i| self.values.plain(i).unwrap_or_default())
            .collect();
        let points: Vec<f32> = (0..POINTS)
            .map(|i| values[shaper::param(self.module, shaper::POINT + i)])
            .collect();
        let curve = |phase| shaper::shape_at(&values, self.module, phase);
        let phase_index = if self.module == shaper::TIME {
            9
        } else {
            2 * self.module
        };
        let phase = f32::from_bits(self.display.meters[phase_index].load(Ordering::Relaxed));
        let height = (ui.available_height() - 150.0).clamp(140.0, 340.0);
        let response = ShapeEditor::new(
            egui::Id::new(("shaper curve", self.module)),
            &points,
            &curve,
        )
        .size(egui::vec2(ui.available_width(), height))
        .color(theme::accent(ui.visuals()))
        .playhead((values[shaper::param(self.module, shaper::ON)] >= 0.5).then_some(phase))
        .show(ui);
        for (point, value) in response.changes {
            self.set(
                shaper::param(self.module, shaper::POINT + point),
                value,
                false,
            );
            changed = true;
        }
        if response.finished {
            self.edits.finish_all();
            changed = true;
        }
        ui.add_space(theme::space::M);
        ui.push_id(("module controls", self.module), |ui| {
            ui.columns(3, |columns| {
                for (column, offset, name) in [
                    (0, shaper::DEPTH, "Depth"),
                    (1, shaper::PHASE, "Phase"),
                    (2, shaper::SMOOTHING, "Curve smooth"),
                ] {
                    changed |= self.control(
                        &mut columns[column],
                        shaper::param(self.module, offset),
                        name,
                    );
                }
            });
        });
        if self.module == shaper::FILTER {
            ui.horizontal(|ui| {
                changed |= self.control(ui, shaper::FILTER_TYPE, "Type");
            });
            ui.columns(3, |columns| {
                for (column, index, label) in [
                    (0, shaper::FILTER_LOW, "From"),
                    (1, shaper::FILTER_HIGH, "To"),
                    (2, shaper::RESONANCE, "Resonance"),
                ] {
                    changed |= self.control(&mut columns[column], index, label);
                }
            });
        } else if self.module == shaper::TIME {
            ui.columns(2, |columns| {
                changed |= self.control(&mut columns[0], shaper::TIME_RANGE, "Range");
                changed |= self.control(&mut columns[1], shaper::TIME_OFFSET, "Offset");
            });
        }
        ui.separator();
        ui.push_id("output controls", |ui| {
            ui.columns(2, |columns| {
                changed |= self.control(&mut columns[0], shaper::MIX, "Mix");
                changed |= self.control(&mut columns[1], shaper::OUTPUT_SMOOTHING, "Output smooth");
            });
        });
        changed
    }
}
