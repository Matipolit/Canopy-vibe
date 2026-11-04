//! The Patcher canvas, driven on its own: a Patcher's inside drawn as a
//! graph, edited with the mouse and keyboard.

use std::path::Path;

use eframe::egui::{
    self, Color32, Event, Key as KeyCode, Modifiers, PointerButton, Pos2, pos2, vec2,
};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use engine::Engine;
use project::{Chain, DeviceId, EffectRef, NodeKind, Split};

use crate::patcher::graph::{End, Key};
use crate::patcher::{self, canvas};
use crate::session::Session;
use crate::timeline::TimelineView;

struct State {
    session: Session,
    timeline: TimelineView,
    chain: Chain,
    /// The theme's fonts are in from the second frame on.
    themed: bool,
}

/// A canvas showing a Patcher's inside: an EQ then a reverb.
struct Bench {
    harness: Harness<'static, State>,
    _engine: Engine,
    patcher: DeviceId,
    eq: DeviceId,
    reverb: DeviceId,
    name: &'static str,
    shots: usize,
}

impl Bench {
    fn new(name: &'static str) -> Bench {
        let (handle, engine) = engine::create(super::RATE);
        let mut session = Session::with_engine(handle, || {});
        let track = session.project.track_ids()[0];
        let (patcher, eq, reverb) = session
            .edit("Set Up", |p| {
                let patcher =
                    p.add_device(Some(track), EffectRef::builtin(project::PATCHER), None)?;
                let chain = Chain::Patch(patcher);
                let eq = p.add_device(chain, EffectRef::builtin("canopy.eq"), None)?;
                let reverb = p.add_device(chain, EffectRef::builtin("canopy.reverb"), None)?;
                Ok((patcher, eq, reverb))
            })
            .expect("set up");
        let state = State {
            session,
            timeline: TimelineView::default(),
            chain: Chain::Patch(patcher),
            themed: false,
        };
        let harness = Harness::builder()
            .with_size(vec2(1000.0, 500.0))
            .with_step_dt(1.0 / 60.0)
            .with_max_steps(60)
            .wgpu_setup(super::quiet_wgpu())
            .build_ui_state(
                |ui, state: &mut State| {
                    if !state.themed {
                        crate::theme::apply(ui.ctx());
                        state.themed = true;
                        return;
                    }
                    let color = Color32::from_rgb(90, 160, 230);
                    canvas(
                        ui,
                        &mut state.session,
                        &mut state.timeline,
                        state.chain,
                        color,
                    );
                },
                state,
            );
        let mut bench = Bench {
            harness,
            _engine: engine,
            patcher,
            eq,
            reverb,
            name,
            shots: 0,
        };
        bench.frames(3);
        bench
    }

    fn chain(&self) -> Chain {
        Chain::Patch(self.patcher)
    }

    fn session(&self) -> &Session {
        &self.harness.state().session
    }

    fn session_mut(&mut self) -> &mut Session {
        &mut self.harness.state_mut().session
    }

    fn steps(&self) -> usize {
        self.session().history().position()
    }

    fn frames(&mut self, n: usize) {
        self.harness.run_steps(n);
    }

    fn event(&mut self, event: Event) {
        self.harness.event(event);
        self.harness.step();
    }

    fn button(&mut self, pos: Pos2, button: PointerButton, pressed: bool) {
        self.event(Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }

    fn click(&mut self, pos: Pos2) {
        self.event(Event::PointerMoved(pos));
        self.button(pos, PointerButton::Primary, true);
        self.button(pos, PointerButton::Primary, false);
        self.frames(1);
    }

    fn right_click(&mut self, pos: Pos2) {
        self.event(Event::PointerMoved(pos));
        self.button(pos, PointerButton::Secondary, true);
        self.button(pos, PointerButton::Secondary, false);
        self.frames(2);
    }

    /// Presses at `from`, moves to `to` in steps, and runs `during` there
    /// before letting go.
    fn drag_then(&mut self, from: Pos2, to: Pos2, during: impl FnOnce(&mut Bench)) {
        self.event(Event::PointerMoved(from));
        self.button(from, PointerButton::Primary, true);
        for i in 1..=8 {
            let t = i as f32 / 8.0;
            self.event(Event::PointerMoved(from + (to - from) * t));
        }
        during(self);
        self.button(to, PointerButton::Primary, false);
        self.frames(2);
    }

    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.drag_then(from, to, |_| {});
    }

    fn shown(&self) -> patcher::canvas::Shown {
        patcher::canvas::shown(&self.harness.ctx, self.chain())
    }

    fn output(&self, key: Key, port: u8) -> Pos2 {
        let end = End { key, port };
        self.shown()
            .outputs
            .iter()
            .find(|(e, _)| *e == end)
            .unwrap_or_else(|| panic!("{}: no output {end:?}", self.name))
            .1
    }

    fn input(&self, key: Key, port: u8) -> Pos2 {
        let end = End { key, port };
        self.shown()
            .inputs
            .iter()
            .find(|(e, _)| *e == end)
            .unwrap_or_else(|| panic!("{}: no input {end:?}", self.name))
            .1
    }

    fn node_rect(&self, key: Key) -> egui::Rect {
        self.shown()
            .nodes
            .iter()
            .find(|(k, _)| *k == key)
            .unwrap_or_else(|| panic!("{}: no node {key:?}", self.name))
            .1
    }

    fn cables(&self) -> usize {
        self.session()
            .project
            .routing(self.chain())
            .map_or(0, |r| r.cables.len())
    }

    /// Somewhere on the canvas with nothing on it.
    fn empty_spot(&self) -> Pos2 {
        let bottom = self
            .shown()
            .nodes
            .iter()
            .map(|(_, r)| r.max.y)
            .fold(f32::MIN, f32::max);
        pos2(self.node_rect(Key::Input).center().x, bottom + 60.0)
    }

    fn screenshot(&mut self, label: &str) {
        let Some(dir) = std::env::var_os("CANOPY_SCREENSHOTS") else {
            return;
        };
        self.shots += 1;
        let image = self.harness.render().expect("render");
        let path = Path::new(&dir).join(format!("{}_{:02}_{label}.png", self.name, self.shots));
        image.save(&path).expect("save screenshot");
    }
}

#[test]
fn a_linear_chains_first_connection_makes_its_graph_in_one_step() {
    let mut bench = Bench::new("patcher_connect");
    bench.screenshot("linear");
    assert!(bench.session().project.routing(bench.chain()).is_none());
    let before = bench.steps();
    // A dry path around both effects.
    let from = bench.output(Key::Input, 0);
    let to = bench.input(Key::Output, 0);
    bench.drag(from, to);
    bench.screenshot("connected");
    let routing = bench
        .session()
        .project
        .routing(bench.chain())
        .expect("a graph now");
    assert_eq!(routing.cables.len(), 4, "the line's three and the new one");
    assert_eq!(bench.steps(), before + 1, "one undo step");

    // Undo puts the line back, all of it.
    bench.session_mut().undo();
    bench.frames(2);
    assert!(bench.session().project.routing(bench.chain()).is_none());
}

#[test]
fn connecting_two_ports_in_a_graph_makes_a_cable() {
    let mut bench = Bench::new("patcher_connect_graph");
    let chain = bench.chain();
    bench
        .session_mut()
        .edit("Add Split", |p| {
            p.add_node(chain, NodeKind::Split(Split::LeftRight), [0.0, 250.0])
        })
        .expect("split");
    bench.frames(2);
    let split = bench
        .shown()
        .nodes
        .iter()
        .find_map(|(k, _)| matches!(k, Key::Node(_)).then_some(*k))
        .expect("the split is drawn");
    let before = (bench.steps(), bench.cables());
    let from = bench.output(Key::Input, 0);
    let to = bench.input(split, 0);
    bench.drag(from, to);
    assert_eq!(bench.cables(), before.1 + 1);
    assert_eq!(bench.steps(), before.0 + 1);

    // Backwards too: from the reverb's input to the split's right.
    let from = bench.input(Key::Device(bench.reverb), 0);
    let to = bench.output(split, 1);
    bench.drag(from, to);
    assert_eq!(bench.cables(), before.1 + 2);
}

#[test]
fn a_loop_is_refused() {
    let mut bench = Bench::new("patcher_loop");
    let before = (bench.steps(), bench.session().project.clone());
    let from = bench.output(Key::Device(bench.reverb), 0);
    let to = bench.input(Key::Device(bench.eq), 0);
    bench.drag_then(from, to, |bench| bench.screenshot("refused"));
    assert_eq!(bench.steps(), before.0);
    assert!(bench.session().project == before.1, "nothing changed");
    assert!(bench.session().message.is_none(), "refused quietly");
}

#[test]
fn click_a_cable_and_delete_it() {
    let mut bench = Bench::new("patcher_delete");
    let chain = bench.chain();
    let (input, output) = (bench.output(Key::Input, 0), bench.input(Key::Output, 0));
    bench.drag(input, output);
    assert_eq!(bench.cables(), 4);

    // The cable into the EQ, then Delete.
    let into_eq = |bench: &Bench| {
        bench
            .shown()
            .wires
            .iter()
            .find(|(w, _)| w.to.key == Key::Device(bench.eq))
            .map(|(_, at)| *at)
    };
    let at = into_eq(&bench).expect("a cable into the EQ");
    bench.click(at);
    bench.screenshot("selected");
    let before = bench.steps();
    bench
        .harness
        .key_press_modifiers(Modifiers::NONE, KeyCode::Delete);
    bench.frames(2);
    assert_eq!(bench.cables(), 3);
    assert_eq!(bench.steps(), before + 1);
    assert!(into_eq(&bench).is_none());

    // The device view's Delete, which comes before the canvas: the
    // cable out of the reverb.
    let out_of_reverb = bench
        .shown()
        .wires
        .iter()
        .find(|(w, _)| w.from.key == Key::Device(bench.reverb))
        .map(|(_, at)| *at)
        .expect("a cable out of the reverb");
    bench.click(out_of_reverb);
    let ctx = bench.harness.ctx.clone();
    assert!(patcher::delete_selected_cable(&ctx, bench.session_mut()));
    assert_eq!(bench.cables(), 2);
    assert!(
        !patcher::delete_selected_cable(&ctx, bench.session_mut()),
        "nothing selected now"
    );
    let routing = bench.session().project.routing(chain).expect("graph");
    assert!(routing.cables.iter().all(|c| {
        routing.node(c.from.node).map(|n| &n.kind) != Some(&NodeKind::Device(bench.reverb))
    }));
}

#[test]
fn add_a_split_from_the_canvas_menu() {
    let mut bench = Bench::new("patcher_split");
    let before = bench.steps();
    let spot = bench.empty_spot();
    bench.right_click(spot);
    bench.harness.get_by_label_contains("Add Split").hover();
    bench.frames(2);
    bench.screenshot("menu");
    bench.harness.get_by_label("3 bands").click();
    bench.frames(2);
    let routing = bench
        .session()
        .project
        .routing(bench.chain())
        .expect("a graph now");
    let split = routing
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Split(Split::Bands { ref crossovers, .. }) if crossovers.len() == 2))
        .expect("a 3-band split");
    assert_eq!(bench.steps(), before + 1, "one undo step");
    // Where it was asked for.
    let drawn = bench.node_rect(Key::Node(split.id));
    assert!((drawn.min - spot).length() < 2.0, "{drawn:?} vs {spot:?}");
    bench.screenshot("split");
    bench.harness.get_by_label("Split 3 bands");
}

#[test]
fn moving_a_node_is_not_an_undo_step() {
    let mut bench = Bench::new("patcher_move");
    // A linear chain's nodes stay in their row.
    let eq = bench.node_rect(Key::Device(bench.eq));
    bench.drag(eq.center(), eq.center() + vec2(0.0, 80.0));
    assert_eq!(bench.node_rect(Key::Device(bench.eq)), eq);

    let (input, output) = (bench.output(Key::Input, 0), bench.input(Key::Output, 0));
    bench.drag(input, output);
    let before = bench.steps();
    let eq = bench.node_rect(Key::Device(bench.eq));
    let reverb = bench.node_rect(Key::Device(bench.reverb));
    let grab = eq.center_top() + vec2(0.0, 6.0);
    bench.drag(grab, grab + vec2(30.0, 80.0));
    let moved = bench.node_rect(Key::Device(bench.eq));
    assert!(
        (moved.min - (eq.min + vec2(30.0, 80.0))).length() < 2.0,
        "{moved:?} from {eq:?}"
    );
    assert_eq!(
        bench.node_rect(Key::Device(bench.reverb)),
        reverb,
        "the others stay"
    );
    assert_eq!(bench.steps(), before, "a view setting, not an undo step");
    let chain = bench.chain();
    let node = bench
        .session()
        .project
        .routing(chain)
        .and_then(|r| r.device_node(bench.eq))
        .expect("node");
    let pos = bench
        .session()
        .project
        .routing(chain)
        .unwrap()
        .node(node)
        .unwrap()
        .pos;
    assert!(pos[1] > 50.0, "{pos:?}");
}

#[test]
fn mute_a_cable_and_set_its_level_from_its_menu() {
    let mut bench = Bench::new("patcher_cable_menu");
    let (input, output) = (bench.output(Key::Input, 0), bench.input(Key::Output, 0));
    bench.drag(input, output);
    let dry = bench
        .shown()
        .wires
        .iter()
        .find(|(w, _)| w.from.key == Key::Input && w.to.key == Key::Output)
        .map(|(_, at)| *at)
        .expect("the dry cable");
    bench.right_click(dry);
    bench.harness.get_by_label("Mute").click();
    bench.frames(2);
    let routing = bench
        .session()
        .project
        .routing(bench.chain())
        .expect("graph");
    let muted: Vec<bool> = routing.cables.iter().map(|c| c.muted).collect();
    assert_eq!(muted.iter().filter(|m| **m).count(), 1, "{muted:?}");
    bench.screenshot("muted");
}

#[test]
fn bypass_a_device_from_its_node() {
    let mut bench = Bench::new("patcher_bypass");
    let before = bench.steps();
    let switches = bench.harness.get_all_by_label("On").count();
    assert_eq!(switches, 2);
    bench.harness.get_all_by_label("On").next().unwrap().click();
    bench.frames(2);
    let eq = bench.session().project.device(bench.eq).unwrap();
    assert!(eq.bypassed);
    assert_eq!(bench.steps(), before + 1);
}
