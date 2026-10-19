//! MIDI keyboards and controllers, through `midir`: the ALSA sequencer on
//! Linux, WinMM on Windows. Every device is opened unless switched off, and
//! plugging one in is noticed within a couple of seconds.
//!
//! WinMM lets only one program at a time open a device, unless Windows MIDI
//! Services is on (Windows 11, rolled out from 2026), which shares them. A
//! device another program holds is tried again at every scan, so it opens
//! once that program lets go.
//!
//! Each device's driver thread sorts what it receives: notes, the pitch
//! wheel and the sustain pedal go straight to the audio thread, so playing
//! never waits on the UI; other controllers come to the UI, to move linked
//! controls.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::{LiveEvent, LiveSender};
use midi::{MidiMessage, SUSTAIN_PEDAL};

/// A controller moved: a knob, fader or wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    pub channel: u8,
    pub controller: u8,
    pub value: u8,
}

const SCAN_EVERY: Duration = Duration::from_secs(2);
const CLIENT_NAME: &str = "canopy";

pub struct MidiInputs {
    /// `None` in tests, which mustn't open the user's devices.
    enabled: bool,
    connections: HashMap<String, midir::MidiInputConnection<()>>,
    /// Every device found at the last scan.
    ports: Vec<String>,
    /// Devices switched off in the MIDI menu, by name.
    pub disabled: HashSet<String>,
    /// Where notes go; swapped when audio restarts.
    live: Arc<Mutex<Option<LiveSender>>>,
    controls_in: Sender<Control>,
    controls: Receiver<Control>,
    wake: Arc<dyn Fn() + Send + Sync>,
    last_scan: Option<Instant>,
    /// Milliseconds after `epoch` that the last message came in.
    activity: Arc<AtomicU64>,
    epoch: Instant,
    pub error: Option<String>,
}

impl MidiInputs {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> MidiInputs {
        let (controls_in, controls) = mpsc::channel();
        MidiInputs {
            enabled: true,
            connections: HashMap::new(),
            ports: Vec::new(),
            disabled: HashSet::new(),
            live: Arc::new(Mutex::new(None)),
            controls_in,
            controls,
            wake,
            last_scan: None,
            activity: Arc::new(AtomicU64::new(0)),
            epoch: Instant::now(),
            error: None,
        }
    }

    /// One that never opens a device, for tests.
    pub fn off(wake: Arc<dyn Fn() + Send + Sync>) -> MidiInputs {
        MidiInputs {
            enabled: false,
            ..MidiInputs::new(wake)
        }
    }

    /// Sends notes to this engine from now on.
    pub fn set_live(&mut self, live: Option<LiveSender>) {
        if let Ok(mut slot) = self.live.lock() {
            *slot = live;
        }
    }

    /// Every device found, and whether it's open.
    pub fn ports(&self) -> impl Iterator<Item = (&str, bool)> {
        self.ports
            .iter()
            .map(|name| (name.as_str(), self.connections.contains_key(name)))
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

    /// True if anything came in during the last moment, for a light.
    pub fn active(&self) -> bool {
        let last = self.activity.load(Ordering::Relaxed);
        let now = self.epoch.elapsed().as_millis() as u64;
        last != 0 && now.saturating_sub(last) < 150
    }

    pub fn try_recv(&self) -> Option<Control> {
        self.controls.try_recv().ok()
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
            // Our own ports, if another canopy is running.
            .filter(|(name, _)| !name.starts_with(CLIENT_NAME))
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
        let live = self.live.clone();
        let controls = self.controls_in.clone();
        let wake = self.wake.clone();
        let activity = self.activity.clone();
        let epoch = self.epoch;
        input
            .connect(
                port,
                "canopy-in",
                move |_, bytes, _| {
                    let Some(message) = MidiMessage::parse(bytes) else {
                        return;
                    };
                    activity.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                    let to_engine = match message {
                        MidiMessage::Note { kind, .. } => Some(LiveEvent::Note(kind)),
                        MidiMessage::ControlChange {
                            controller: SUSTAIN_PEDAL,
                            value,
                            ..
                        } => Some(LiveEvent::Sustain(value >= 64)),
                        MidiMessage::ControlChange {
                            channel,
                            controller,
                            value,
                        } => {
                            let _ = controls.send(Control {
                                channel,
                                controller,
                                value,
                            });
                            None
                        }
                    };
                    if let Some(event) = to_engine
                        && let Ok(live) = live.lock()
                        && let Some(live) = live.as_ref()
                    {
                        live.send(event);
                    }
                    wake();
                },
                (),
            )
            .map_err(|err| connect_error(err.to_string()))
    }
}

/// Why a device didn't open, in words.
fn connect_error(err: String) -> String {
    // WinMM's code for a device another program has open.
    #[cfg(windows)]
    if err.contains("MMSYSERR_ALLOCATED") {
        return "in use by another program".to_owned();
    }
    err
}
