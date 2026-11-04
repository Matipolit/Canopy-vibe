//! The canvas's right-click menus: empty space (add splits, joins and
//! effects; tidy), a node (a split's kind and crossovers, a device's
//! bypass, removing), and a cable (mute, level, delete).

use effects::{Category, EffectRef};
use eframe::egui::{DragValue, RichText, Slider, Ui, Vec2};
use project::{Chain, Join, MAX_VOLUME_DB, NodeKind, SILENCE_DB, Slope, Split};

use super::canvas::pin;
use super::graph::{self, Graph, Key, ViewNode, WireKey};
use crate::session::Session;

/// The lowest and highest crossover, in Hz.
const CROSSOVER_RANGE: std::ops::RangeInclusive<f32> = 20.0..=20_000.0;

/// The splits the menus offer, by name.
fn splits(slope: Slope) -> [(&'static str, Split); 5] {
    [
        ("2 bands", graph::bands(2, slope)),
        ("3 bands", graph::bands(3, slope)),
        ("4 bands", graph::bands(4, slope)),
        ("L/R", Split::LeftRight),
        ("M/S", Split::MidSide),
    ]
}

const JOINS: [(&str, Join); 2] = [("L/R", Join::LeftRight), ("M/S", Join::MidSide)];

/// Before a structural edit to a graph canopy laid out: its nodes keep the
/// places they're shown at, rather than all going back to the origin once
/// one of them has a place.
fn keep_places(session: &mut Session, chain: Chain, graph: &Graph) {
    if graph.laid_out && !graph.linear {
        pin(session, chain, graph);
    }
}

/// Empty space, right-clicked at `at` on the canvas.
pub(crate) fn canvas(ui: &mut Ui, session: &mut Session, chain: Chain, graph: &Graph, at: Vec2) {
    let pos = [at.x, at.y];
    ui.menu_button("Add Split", |ui| {
        for (label, split) in splits(Slope::default()) {
            if ui.button(label).clicked() {
                keep_places(session, chain, graph);
                session.edit("Add Split", |p| {
                    p.add_node(chain, NodeKind::Split(split), pos).map(|_| ())
                });
                ui.close();
            }
        }
    });
    ui.menu_button("Add Join", |ui| {
        for (label, join) in JOINS {
            if ui
                .button(label)
                .on_hover_text("Puts a split's two parts back together")
                .clicked()
            {
                keep_places(session, chain, graph);
                session.edit("Add Join", |p| {
                    p.add_node(chain, NodeKind::Join(join), pos).map(|_| ())
                });
                ui.close();
            }
        }
    });
    ui.menu_button("Add Effect", |ui| {
        // Plugins first, as "+ Effect" lists them.
        if crate::plugins::effect_menu(ui, session, chain).is_some() {
            return;
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
                    keep_places(session, chain, graph);
                    let effect = EffectRef::builtin(descriptor.id);
                    session.edit("Add Effect", |p| {
                        p.add_device(chain, effect, None).map(|_| ())
                    });
                    ui.close();
                }
            }
        }
    });
    if !graph.linear {
        ui.separator();
        if ui
            .button("Tidy")
            .on_hover_text("Lay the nodes out left to right")
            .clicked()
        {
            pin(session, chain, &graph.laid_out_anew());
            ui.close();
        }
    }
}

/// Whether right-clicking `node` shows a menu: Input and Output have
/// nothing to set.
pub(crate) fn has_node_menu(node: &ViewNode) -> bool {
    !matches!(node.kind, NodeKind::Input | NodeKind::Output)
}

pub(crate) fn node(
    ui: &mut Ui,
    session: &mut Session,
    chain: Chain,
    graph: &Graph,
    node: &ViewNode,
) {
    match (&node.kind, node.key) {
        (NodeKind::Device(_), Key::Device(device)) => {
            let bypassed = node.bypassed == Some(true);
            let (label, step) = if bypassed {
                ("Turn On", "Turn On Effect")
            } else {
                ("Bypass", "Bypass Effect")
            };
            if ui.button(label).clicked() {
                session.edit(step, |p| p.set_device_bypassed(device, !bypassed));
                ui.close();
            }
            ui.separator();
            if ui.button("Remove").clicked() {
                session.edit("Remove Effect", |p| p.remove_device(device));
                ui.close();
            }
        }
        (NodeKind::Split(split), Key::Node(_)) => {
            split_menu(ui, session, chain, graph, node.key, split);
            ui.separator();
            remove_button(ui, session, chain, graph, node.key, "Remove Split");
        }
        (NodeKind::Join(_), Key::Node(_)) => {
            remove_button(ui, session, chain, graph, node.key, "Remove Join");
        }
        _ => {}
    }
}

fn remove_button(
    ui: &mut Ui,
    session: &mut Session,
    chain: Chain,
    graph: &Graph,
    key: Key,
    step: &str,
) {
    if ui.button("Remove").clicked() {
        keep_places(session, chain, graph);
        session.edit(step, |p| {
            let id = graph::node_id(p, chain, key)?;
            p.remove_node(chain, id)
        });
        ui.close();
    }
}

/// A split's kind, and for a band split its crossovers and slope.
fn split_menu(
    ui: &mut Ui,
    session: &mut Session,
    chain: Chain,
    graph: &Graph,
    key: Key,
    split: &Split,
) {
    let slope = match split {
        Split::Bands { slope, .. } => *slope,
        _ => Slope::default(),
    };
    let set = |session: &mut Session, label: &str, split: Split| {
        keep_places(session, chain, graph);
        session.edit(label, |p| {
            let id = graph::node_id(p, chain, key)?;
            p.set_split(chain, id, split)
        });
    };
    for (label, choice) in splits(slope) {
        let current = std::mem::discriminant(split) == std::mem::discriminant(&choice)
            && match (split, &choice) {
                (Split::Bands { crossovers: a, .. }, Split::Bands { crossovers: b, .. }) => {
                    a.len() == b.len()
                }
                _ => true,
            };
        if ui.selectable_label(current, label).clicked() && !current {
            set(session, "Change Split", choice);
            ui.close();
        }
    }
    let Split::Bands { crossovers, slope } = split else {
        return;
    };
    ui.separator();
    for i in 0..crossovers.len() {
        let low = if i == 0 {
            *CROSSOVER_RANGE.start()
        } else {
            crossovers[i - 1]
        };
        let high = crossovers
            .get(i + 1)
            .copied()
            .unwrap_or(*CROSSOVER_RANGE.end());
        let mut hz = crossovers[i];
        // Steps in proportion, as the ear hears frequency.
        let speed = hz * 0.005;
        let response = ui.add(
            DragValue::new(&mut hz)
                .range(low..=high)
                .speed(speed)
                .suffix(" Hz")
                .max_decimals(0),
        );
        let response = response.on_hover_text("Crossover");
        if response.changed() {
            let mut next = crossovers.clone();
            next[i] = hz;
            let split = Split::Bands {
                crossovers: next,
                slope: *slope,
            };
            session.live_edit(|p| {
                let id = graph::node_id(p, chain, key)?;
                p.set_split(chain, id, split)
            });
        }
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            session.end_live("Set Crossover");
        }
    }
    ui.horizontal(|ui| {
        for (label, choice) in [
            ("12 dB", Slope::Db12),
            ("24 dB", Slope::Db24),
            ("48 dB", Slope::Db48),
        ] {
            if ui
                .selectable_label(*slope == choice, label)
                .on_hover_text("Slope")
                .clicked()
                && *slope != choice
            {
                let split = Split::Bands {
                    crossovers: crossovers.clone(),
                    slope: choice,
                };
                set(session, "Set Slope", split);
            }
        }
    });
}

/// A cable's menu. Returns whether it was deleted.
pub(crate) fn wire(
    ui: &mut Ui,
    session: &mut Session,
    chain: Chain,
    graph: &Graph,
    key: WireKey,
) -> bool {
    let Some(wire) = graph.wire(key) else {
        ui.close();
        return false;
    };
    let (gain, muted) = (wire.gain_db, wire.muted);
    let (label, step) = if muted {
        ("Unmute", "Unmute Cable")
    } else {
        ("Mute", "Mute Cable")
    };
    if ui.button(label).clicked() {
        keep_places(session, chain, graph);
        session.edit(step, |p| graph::set_wire(p, chain, key, gain, !muted));
        ui.close();
    }
    let mut db = gain;
    let response = ui
        .add(
            Slider::new(&mut db, SILENCE_DB..=MAX_VOLUME_DB)
                .suffix(" dB")
                .fixed_decimals(1),
        )
        .on_hover_text("Level");
    if response.changed() {
        if graph.laid_out && !graph.linear && !session.is_live() {
            pin(session, chain, graph);
        }
        session.live_edit(|p| graph::set_wire(p, chain, key, db, muted));
    }
    if response.drag_stopped() || (response.changed() && !response.dragged()) {
        session.end_live("Cable Level");
    }
    if response.double_clicked() {
        session.edit("Cable Level", |p| {
            graph::set_wire(p, chain, key, 0.0, muted)
        });
    }
    ui.separator();
    if ui.button("Delete").clicked() {
        session.edit("Remove Cable", |p| graph::disconnect(p, chain, key));
        ui.close();
        return true;
    }
    false
}
