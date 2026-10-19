//! MIDI keyboards through `midir` (the ALSA sequencer on Linux): every
//! device is opened unless switched off, and plugging one in is noticed
//! within a couple of seconds. Adapted from the DAW's `midi_input`.
//!
//! Each device's driver thread sends notes and the sustain pedal straight
//! to the audio thread, so playing never waits on the UI, and also to the
//! UI, stamped with the clock, for grading. The on-screen and typing
//! keyboards come in through [`MidiDevices::play`] the same way.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::{LiveEvent, LiveSender};
use midi::{MidiMessage, NoteEventKind, Pitch, SUSTAIN_PEDAL, Velocity};

use crate::clock::Clock;

/// Something played, with the clock time it came in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    Note {
        pitch: u8,
        velocity: u8,
        on: bool,
        time: f64,
    },
    Pedal {
        down: bool,
        time: f64,
    },
}

impl Input {
    pub fn time(&self) -> f64 {
        match self {
            Input::Note { time, .. } | Input::Pedal { time, .. } => *time,
        }
    }
}

const SCAN_EVERY: Duration = Duration::from_secs(2);
const CLIENT_NAME: &str = "canopy-learn";

/// What the driver threads share: where notes go.
struct Router {
    live: Mutex<Option<LiveSender>>,
    inputs: Mutex<Sender<Input>>,
    clock: Clock,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Clock milliseconds when a device last sent something, plus one.
    activity: AtomicU64,
}

impl Router {
    /// Raw bytes from a device.
    fn receive(&self, bytes: &[u8]) {
        let Some(message) = MidiMessage::parse(bytes) else {
            return;
        };
        let time = self.clock.now();
        self.activity
            .store((time * 1000.0) as u64 + 1, Ordering::Relaxed);
        let (event, input) = match message {
            MidiMessage::Note { kind, .. } => match kind {
                NoteEventKind::On { pitch, velocity } => (
                    Some(LiveEvent::Note(kind)),
                    Some(Input::Note {
                        pitch: pitch.get(),
                        velocity: velocity.to_midi1(),
                        on: true,
                        time,
                    }),
                ),
                NoteEventKind::Off { pitch, .. } => (
                    Some(LiveEvent::Note(kind)),
                    Some(Input::Note {
                        pitch: pitch.get(),
                        velocity: 0,
                        on: false,
                        time,
                    }),
                ),
                _ => (Some(LiveEvent::Note(kind)), None),
            },
            MidiMessage::ControlChange {
                controller: SUSTAIN_PEDAL,
                value,
                ..
            } => (
                Some(LiveEvent::Sustain(value >= 64)),
                Some(Input::Pedal {
                    down: value >= 64,
                    time,
                }),
            ),
            MidiMessage::ControlChange { .. } => (None, None),
        };
        self.forward(event, input);
    }

    fn forward(&self, event: Option<LiveEvent>, input: Option<Input>) {
        if let Some(event) = event
            && let Ok(live) = self.live.lock()
            && let Some(live) = live.as_ref()
        {
            live.send(event);
        }
        if let Some(input) = input
            && let Ok(inputs) = self.inputs.lock()
        {
            let _ = inputs.send(input);
        }
        (self.wake)();
    }
}

pub struct MidiDevices {
    /// False in tests, which mustn't open the user's devices.
    enabled: bool,
    connections: HashMap<String, midir::MidiInputConnection<()>>,
    ports: Vec<String>,
    /// Devices switched off in the MIDI menu, by name.
    pub disabled: HashSet<String>,
    router: Arc<Router>,
    inputs: Receiver<Input>,
    last_scan: Option<Instant>,
    pub error: Option<String>,
}

impl MidiDevices {
    pub fn new(clock: Clock, wake: Arc<dyn Fn() + Send + Sync>) -> MidiDevices {
        let (send, inputs) = mpsc::channel();
        MidiDevices {
            enabled: true,
            connections: HashMap::new(),
            ports: Vec::new(),
            disabled: HashSet::new(),
            router: Arc::new(Router {
                live: Mutex::new(None),
                inputs: Mutex::new(send),
                clock,
                wake,
                activity: AtomicU64::new(0),
            }),
            inputs,
            last_scan: None,
            error: None,
        }
    }

    /// One that never opens a device, for tests.
    pub fn off(clock: Clock, wake: Arc<dyn Fn() + Send + Sync>) -> MidiDevices {
        MidiDevices {
            enabled: false,
            ..MidiDevices::new(clock, wake)
        }
    }

    /// Sends notes to this engine from now on.
    pub fn set_live(&self, live: Option<LiveSender>) {
        if let Ok(mut slot) = self.router.live.lock() {
            *slot = live;
        }
    }

    /// Bytes as a device would send them, through the same path: what
    /// tests play.
    pub fn simulate(&self, bytes: &[u8]) {
        self.router.receive(bytes);
    }

    /// A note from the on-screen or typing keyboard.
    pub fn play(&self, pitch: u8, on: bool, velocity: u8) {
        let Some(p) = Pitch::new(pitch) else { return };
        let kind = if on {
            NoteEventKind::On {
                pitch: p,
                velocity: Velocity::from_midi1(velocity.max(1)),
            }
        } else {
            NoteEventKind::Off {
                pitch: p,
                velocity: Velocity::CENTER,
            }
        };
        let input = Input::Note {
            pitch,
            velocity: if on { velocity } else { 0 },
            on,
            time: self.router.clock.now(),
        };
        self.router
            .forward(Some(LiveEvent::Note(kind)), Some(input));
    }

    pub fn try_recv(&self) -> Option<Input> {
        self.inputs.try_recv().ok()
    }

    /// Every device found, and whether it's open.
    pub fn ports(&self) -> impl Iterator<Item = (&str, bool)> {
        self.ports
            .iter()
            .map(|name| (name.as_str(), self.connections.contains_key(name)))
    }

    pub fn open_count(&self) -> usize {
        self.connections.len()
    }

    pub fn set_enabled(&mut self, name: &str, on: bool) {
        if on {
            self.disabled.remove(name);
        } else {
            self.disabled.insert(name.to_owned());
            self.connections.remove(name);
        }
        self.last_scan = None;
    }

    /// True if a device sent something in the last moment, for a light.
    pub fn active(&self) -> bool {
        let last = self.router.activity.load(Ordering::Relaxed);
        let now = (self.router.clock.now() * 1000.0) as u64 + 1;
        last != 0 && now.saturating_sub(last) < 150
    }

    /// Call every frame: every few seconds, opens devices that appeared and
    /// forgets ones that went away.
    pub fn update(&mut self) {
        if !self.enabled || self.last_scan.is_some_and(|t| t.elapsed() < SCAN_EVERY) {
            return;
        }
        self.last_scan = Some(Instant::now());
        let input = match midir::MidiInput::new(CLIENT_NAME) {
            Ok(input) => input,
            Err(err) => {
                self.error = Some(format!("MIDI: {err}"));
                return;
            }
        };
        let ports: Vec<(String, midir::MidiInputPort)> = input
            .ports()
            .into_iter()
            .filter_map(|port| Some((input.port_name(&port).ok()?, port)))
            // Our own ports, and the DAW's if it's running.
            .filter(|(name, _)| !name.starts_with(CLIENT_NAME) && !name.starts_with("canopy"))
            .collect();
        self.ports = ports.iter().map(|(name, _)| name.clone()).collect();
        self.connections
            .retain(|name, _| ports.iter().any(|(n, _)| n == name));
        for (name, port) in ports {
            if self.connections.contains_key(&name) || self.disabled.contains(&name) {
                continue;
            }
            match self.connect(&port) {
                Ok(connection) => {
                    self.connections.insert(name, connection);
                    self.error = None;
                }
                Err(err) => self.error = Some(format!("MIDI: {name}: {err}")),
            }
        }
    }

    fn connect(
        &self,
        port: &midir::MidiInputPort,
    ) -> Result<midir::MidiInputConnection<()>, String> {
        let mut input = midir::MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
        input.ignore(midir::Ignore::All);
        let router = self.router.clone();
        input
            .connect(
                port,
                "canopy-learn-in",
                move |_, bytes, _| router.receive(bytes),
                (),
            )
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_become_timed_inputs() {
        let clock = Clock::manual();
        let devices = MidiDevices::off(clock.clone(), Arc::new(|| {}));
        clock.advance(1.5);
        devices.simulate(&[0x90, 60, 100]);
        devices.simulate(&[0x90, 60, 0]);
        devices.simulate(&[0xB0, 64, 127]);
        devices.simulate(&[0xB0, 1, 20]);
        devices.simulate(&[0xF8]);
        devices.play(64, true, 90);
        let got: Vec<Input> = std::iter::from_fn(|| devices.try_recv()).collect();
        assert_eq!(
            got,
            [
                Input::Note {
                    pitch: 60,
                    velocity: 100,
                    on: true,
                    time: 1.5
                },
                Input::Note {
                    pitch: 60,
                    velocity: 0,
                    on: false,
                    time: 1.5
                },
                Input::Pedal {
                    down: true,
                    time: 1.5
                },
                Input::Note {
                    pitch: 64,
                    velocity: 90,
                    on: true,
                    time: 1.5
                },
            ]
        );
        assert!(devices.active());
        clock.advance(1.0);
        assert!(!devices.active());
    }
}
