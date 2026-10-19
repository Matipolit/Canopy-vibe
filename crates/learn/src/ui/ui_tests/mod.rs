//! End-to-end tests: the real app, driven headlessly, with notes coming in
//! through the same path a MIDI keyboard's do, a clock the test moves, and
//! an engine the test renders.
//!
//! Set `LEARN_SCREENSHOTS=<dir>` to also save what each step looks like.

mod drills;
mod keyboard;
mod reading;
mod songs;

use std::path::{Path, PathBuf};

use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use engine::Engine;

use crate::clock::Clock;
use crate::ui::app::{LearnApp, View};

const RATE: f32 = 48_000.0;
const FRAME: f64 = 1.0 / 60.0;

pub(super) struct Rig {
    pub(super) harness: Harness<'static, LearnApp>,
    engine: Engine,
    clock: Clock,
    name: &'static str,
    shots: usize,
    /// The left channel of everything rendered since the last `take_audio`.
    audio: Vec<f32>,
}

/// kittest's software-Vulkan setup without wgpu's debug names and
/// validation, which crash the Vulkan loader when tests run in parallel
/// (see the DAW's ui_tests).
fn quiet_wgpu() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::egui_wgpu::{WgpuSetup, wgpu::InstanceFlags};
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let WgpuSetup::CreateNew(new) = &mut setup {
        new.instance_descriptor
            .flags
            .remove(InstanceFlags::DEBUG | InstanceFlags::VALIDATION);
    }
    setup
}

impl Rig {
    pub(super) fn new(name: &'static str) -> Rig {
        Rig::with_size(name, vec2(1280.0, 800.0))
    }

    pub(super) fn with_size(name: &'static str, size: egui::Vec2) -> Rig {
        let (handle, engine) = engine::create(RATE);
        let clock = Clock::manual();
        let app_clock = clock.clone();
        let harness = Harness::builder()
            .with_size(size)
            .with_step_dt(FRAME as f32)
            .wgpu_setup(quiet_wgpu())
            .build_eframe(move |_cc| LearnApp::for_tests(handle, app_clock, 7));
        let mut rig = Rig {
            harness,
            engine,
            clock,
            name,
            shots: 0,
            audio: Vec::new(),
        };
        rig.frames(3);
        rig
    }

    pub(super) fn app(&self) -> &LearnApp {
        self.harness.state()
    }

    pub(super) fn app_mut(&mut self) -> &mut LearnApp {
        self.harness.state_mut()
    }

    /// `n` frames: the clock moves on a frame, the engine renders a
    /// frame's audio, the app runs.
    pub(super) fn frames(&mut self, n: usize) {
        let samples = (f64::from(RATE) * FRAME).round() as usize;
        for _ in 0..n {
            self.clock.advance(FRAME);
            let mut out = vec![0.0; samples * 2];
            self.engine.process_interleaved(&mut out, 2);
            self.audio.extend(out.chunks(2).map(|f| f[0]));
            self.harness.step();
        }
    }

    pub(super) fn wait(&mut self, seconds: f64) {
        self.frames((seconds / FRAME).round() as usize);
    }

    /// Everything rendered since last asked.
    pub(super) fn take_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio)
    }

    /// A MIDI keyboard's Note On, as the device sends it.
    pub(super) fn note_on(&mut self, pitch: u8) {
        self.app().shared.devices.simulate(&[0x90, pitch, 100]);
    }

    pub(super) fn note_off(&mut self, pitch: u8) {
        self.app().shared.devices.simulate(&[0x80, pitch, 64]);
    }

    /// Presses a key and lets go of it a moment later.
    pub(super) fn tap(&mut self, pitch: u8) {
        self.note_on(pitch);
        self.frames(3);
        self.note_off(pitch);
        self.frames(2);
    }

    /// Several keys at once, then let go.
    pub(super) fn chord(&mut self, pitches: &[u8]) {
        for &p in pitches {
            self.note_on(p);
        }
        self.frames(3);
        for &p in pitches {
            self.note_off(p);
        }
        self.frames(2);
    }

    pub(super) fn view(&mut self, view: View) {
        self.app_mut().set_view(view);
        self.frames(3);
    }

    pub(super) fn event(&mut self, event: Event) {
        self.harness.event(event);
        self.frames(1);
    }

    pub(super) fn press(&mut self, pos: Pos2, pressed: bool) {
        self.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }

    pub(super) fn click(&mut self, pos: Pos2) {
        self.event(Event::PointerMoved(pos));
        self.press(pos, true);
        self.press(pos, false);
        self.frames(1);
    }

    pub(super) fn key(&mut self, modifiers: Modifiers, key: Key) {
        self.harness.key_press_modifiers(modifiers, key);
        self.frames(2);
    }

    /// Where a key of the on-screen piano is.
    pub(super) fn piano_key(&self, pitch: u8) -> Pos2 {
        let layout = self.app().layout.expect("the piano was drawn");
        let rect = layout.key_rect(pitch).expect("the key is shown");
        rect.center_bottom() - vec2(0.0, rect.height() * 0.2)
    }

    /// Saves a screenshot if `LEARN_SCREENSHOTS` is set.
    pub(super) fn screenshot(&mut self, label: &str) {
        let Some(dir) = std::env::var_os("LEARN_SCREENSHOTS") else {
            return;
        };
        self.shots += 1;
        let image = self.harness.render().expect("render");
        let path = Path::new(&dir).join(format!("{}_{:02}_{label}.png", self.name, self.shots));
        image.save(&path).expect("save screenshot");
    }
}

pub(super) fn peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0, |m, s| m.max(s.abs()))
}

pub(super) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("canopy-learn-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
