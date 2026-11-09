//! Real pointer editing, multiple editor instances, and host automation delivery.

use canopy_clap::{
    device::Display,
    editor::{Edits, ShaperEditor},
    values::Values,
};
use effects::shaper::{self, param};
use egui::{Event, Modifiers, PointerButton, pos2};
use egui_kittest::{Harness, kittest::Queryable};
use std::sync::Arc;

fn editor(width: f32, height: f32) -> (Harness<'static>, Arc<Values>, Arc<Edits>) {
    let values = Arc::new(Values::new(shaper::DESCRIPTOR.params));
    let edits = Arc::new(Edits::new(values.len()));
    let mut editor = ShaperEditor::new(values.clone(), Arc::new(Display::default()), edits.clone());
    let mut styled = false;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(width, height))
        .with_max_steps(60)
        .build_ui(move |ui| {
            if !styled {
                canopy_widgets::theme::apply(ui.ctx());
                styled = true;
                ui.ctx().request_discard("fonts loading");
                return;
            }
            editor.show(ui);
        });
    harness.run_steps(3);
    (harness, values, edits)
}
fn pointer(h: &mut Harness<'static>, position: egui::Pos2, down: bool) {
    h.event(Event::PointerMoved(position));
    h.event(Event::PointerButton {
        pos: position,
        button: PointerButton::Primary,
        pressed: down,
        modifiers: Modifiers::NONE,
    });
    h.step();
}

#[test]
fn curves_draw_in_each_module_and_the_editor_fits_at_its_minimum_size() {
    let (mut h, values, edits) = editor(760.0, 480.0);
    let (mut other, other_values, _) = editor(760.0, 480.0);
    for (module, label) in ["Volume", "Filter", "Pan", "Time"].iter().enumerate() {
        h.get_by_label(label).click();
        h.run_steps(3);
        let rect = h
            .ctx
            .data(|d| {
                d.get_temp::<egui::Rect>(egui::Id::new(("shaper curve", module)).with("rect"))
            })
            .unwrap()
            .shrink(6.0);
        pointer(&mut h, pos2(rect.left() + 2.0, rect.bottom() - 2.0), true);
        for step in 1..=12 {
            let t = step as f32 / 12.0;
            h.event(Event::PointerMoved(pos2(
                rect.left() + t * rect.width() * 0.95,
                rect.bottom() - t * rect.height(),
            )));
            h.step();
        }
        pointer(&mut h, pos2(rect.right() - 2.0, rect.top() + 2.0), false);
        h.run_steps(2);
        let first = values.plain(param(module, shaper::POINT + 2)).unwrap();
        let last = values.plain(param(module, shaper::POINT + 27)).unwrap();
        assert!(
            last > first + 0.5,
            "{label} drew a rising curve: {first} → {last}"
        );
        assert!(edits.pending());
        for mix in h.get_all_by_label("Mix").map(|node| node.rect()) {
            assert!(mix.bottom() <= 480.0, "{label} footer is visible: {mix:?}");
        }
    }
    other.get_by_label("Pan").click();
    other.run_steps(2);
    assert_ne!(
        values.plain(param(shaper::PAN, shaper::POINT + 2)),
        other_values.plain(param(shaper::PAN, shaper::POINT + 2)),
        "editing one plugin doesn't change another"
    );
}

#[test]
fn enable_and_curve_presets_work_with_pointer_clicks() {
    let (mut h, values, _) = editor(1000.0, 620.0);
    let on = param(shaper::VOLUME, shaper::ON);
    let before = values.plain(on).unwrap();
    h.get_by_label("Enabled").click();
    h.run_steps(3);
    assert_ne!(values.plain(on).unwrap(), before);
    let rect = h
        .ctx
        .data(|d| {
            d.get_temp::<egui::Rect>(egui::Id::new(("shaper curve", shaper::VOLUME)).with("rect"))
        })
        .unwrap();
    let position = rect.center();
    h.event(Event::PointerMoved(position));
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: position,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(3);
    h.get_by_label("Saw down").click();
    h.run_steps(3);
    assert!(
        values
            .plain(param(shaper::VOLUME, shaper::POINT + 2))
            .unwrap()
            > 0.9
    );
    assert!(
        values
            .plain(param(shaper::VOLUME, shaper::POINT + 30))
            .unwrap()
            < 0.1
    );
}

#[test]
fn host_automation_and_loaded_shapes_are_visible_in_the_editor() {
    let (mut h, values, _) = editor(1000.0, 620.0);
    for i in 0..shaper::POINTS {
        values.send_plain(param(shaper::VOLUME, shaper::POINT + i), 0.25);
    }
    h.run_steps(3);
    assert_eq!(
        values.plain(param(shaper::VOLUME, shaper::POINT + 15)),
        Some(0.25)
    );
    if let Some(dir) = std::env::var_os("CANOPY_SCREENSHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.render()
            .unwrap()
            .save(dir.join("shaper-clap-volume.png"))
            .unwrap();
        for label in ["Filter", "Pan", "Time"] {
            h.get_by_label(label).click();
            h.run_steps(3);
            h.render()
                .unwrap()
                .save(dir.join(format!("shaper-clap-{}.png", label.to_lowercase())))
                .unwrap();
        }
    }
}

#[test]
fn gui_edits_report_gestures_and_retry_host_event_backpressure() {
    use clap_sys::events::*;
    struct Output {
        events: Vec<(u16, f64)>,
        limit: usize,
    }
    unsafe extern "C" fn push(
        list: *const clap_output_events,
        event: *const clap_event_header,
    ) -> bool {
        let output = unsafe { &mut *((*list).ctx as *mut Output) };
        if output.events.len() >= output.limit {
            return false;
        }
        let header = unsafe { &*event };
        let value = if header.type_ == CLAP_EVENT_PARAM_VALUE {
            unsafe { (*(event as *const clap_event_param_value)).value }
        } else {
            0.0
        };
        output.events.push((header.type_, value));
        true
    }
    let values = Values::new(shaper::DESCRIPTOR.params);
    let edits = Edits::new(values.len());
    let index = param(shaper::VOLUME, shaper::DEPTH);
    edits.set(&values, index, 25.0);
    edits.finish(index);
    let mut output = Output {
        events: Vec::with_capacity(8),
        limit: 1,
    };
    let list = clap_output_events {
        ctx: &mut output as *mut _ as *mut _,
        try_push: Some(push),
    };
    unsafe { edits.emit(&values, &list) };
    assert!(edits.pending());
    output.limit = 8;
    unsafe {
        edits.emit(&values, &list);
        edits.emit(&values, &list);
    }
    assert_eq!(
        output.events,
        [
            (CLAP_EVENT_PARAM_GESTURE_BEGIN, 0.0),
            (CLAP_EVENT_PARAM_VALUE, 0.25),
            (CLAP_EVENT_PARAM_GESTURE_END, 0.0)
        ]
    );
    assert!(!edits.pending());
}
