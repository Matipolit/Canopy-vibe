//! A loaded plugin, the same for every format: [`PluginInstance`] on the
//! main thread, [`Processor`] on the audio thread.
//!
//! The two halves share the plugin. Activating the instance makes a
//! processor; deactivating takes it back. A processor dropped on the main
//! thread deactivates the plugin itself. One dropped on another thread
//! (which the engine shouldn't do: it sends what it retires back to the UI)
//! is parked and dropped by the next [`crate::Host::idle`], since plugins
//! may only be deactivated and destroyed on the main thread.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::block::{OutEvents, PortBuffers};
use crate::clap::{ClapInstance, ClapProcessor};
use crate::editor::EditorOptions;
use crate::thread::MainThread;
use crate::vst2::{Vst2Instance, Vst2Processor};
use crate::vst3::{Vst3Instance, Vst3Processor};
use crate::{
    AudioConfig, AudioPortInfo, Error, Event, NotePortInfo, ParamId, ParamInfo, PluginEvent,
    PluginInfo, ProcessStatus, Tail, Transport,
};

/// Things dropped off the main thread, to drop on it.
#[derive(Default)]
pub(crate) struct Graveyard(Mutex<Vec<Box<dyn Any + Send>>>);

impl Graveyard {
    fn bury(&self, thing: Box<dyn Any + Send>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(thing);
    }

    /// Drops what's buried; main thread.
    pub fn clear(&self) {
        let things = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        drop(things);
    }
}

pub(crate) enum Backend {
    Clap(Box<ClapInstance>),
    Vst3(Box<Vst3Instance>),
    Vst2(Box<Vst2Instance>),
}

/// Calls the same method on whichever format the instance is.
macro_rules! each {
    ($backend:expr, $plugin:ident => $call:expr) => {
        match $backend {
            Backend::Clap($plugin) => $call,
            Backend::Vst3($plugin) => $call,
            Backend::Vst2($plugin) => $call,
        }
    };
}

/// The main-thread state of an instance, which the host idles.
pub(crate) struct Inner {
    pub info: PluginInfo,
    pub backend: Backend,
    /// Identifies the instance's processors.
    pub serial: u64,
    events: RefCell<Vec<PluginEvent>>,
    config: Cell<Option<AudioConfig>>,
    restart_requested: Cell<bool>,
    /// The latency reported at the last activation.
    latency: Cell<Option<u32>>,
}

impl Inner {
    pub fn new(info: PluginInfo, backend: Backend, serial: u64) -> Inner {
        Inner {
            info,
            backend,
            serial,
            events: RefCell::new(Vec::new()),
            config: Cell::new(None),
            restart_requested: Cell::new(false),
            latency: Cell::new(None),
        }
    }

    /// Runs the plugin's main-thread work; returns its next timer.
    pub fn idle(&self) -> Option<Instant> {
        let mut events = Vec::new();
        let next = each!(&self.backend, b => b.idle(&mut events));
        if events.contains(&PluginEvent::RestartRequested) {
            self.restart_requested.set(true);
        }
        self.events.borrow_mut().extend(events);
        next
    }

    pub fn watched_fds(&self, out: &mut Vec<i32>) {
        each!(&self.backend, b => b.watched_fds(out));
    }
}

/// A plugin on the main thread (the thread its [`crate::Host`] was made
/// on): its parameters, state, activation and editor.
///
/// Dropping it closes its editor and, once its processor is gone too,
/// destroys the plugin.
pub struct PluginInstance {
    pub(crate) inner: Rc<Inner>,
    pub(crate) graveyard: Arc<Graveyard>,
    pub(crate) main: MainThread,
}

impl PluginInstance {
    /// What the scan said about it.
    pub fn info(&self) -> &PluginInfo {
        &self.inner.info
    }

    fn backend(&self) -> &Backend {
        &self.inner.backend
    }

    /// Its audio inputs and outputs, as it has them now.
    pub fn audio_ports(&self) -> (Vec<AudioPortInfo>, Vec<AudioPortInfo>) {
        each!(self.backend(), b => b.audio_ports())
    }

    /// Its note inputs and outputs.
    pub fn note_ports(&self) -> (Vec<NotePortInfo>, Vec<NotePortInfo>) {
        each!(self.backend(), b => b.note_ports())
    }

    /// Its parameters, in the order it lists them.
    pub fn params(&self) -> Vec<ParamInfo> {
        each!(self.backend(), b => b.params())
    }

    /// A parameter's current value, as the plugin's main thread sees it.
    pub fn param_value(&self, id: ParamId) -> Option<f64> {
        each!(self.backend(), b => b.param_value(id))
    }

    /// How the plugin shows `value` of parameter `id` ("-6.0 dB", "Saw").
    /// A VST2 plugin can only show its parameter's current value: for any
    /// other value this is `None`.
    pub fn param_text(&self, id: ParamId, value: f64) -> Option<String> {
        each!(self.backend(), b => b.param_text(id, value))
    }

    /// The value the plugin reads from `text`, if it can. Always `None` for
    /// VST2, whose text conversion sets the parameter instead.
    pub fn param_from_text(&self, id: ParamId, text: &str) -> Option<f64> {
        each!(self.backend(), b => b.param_from_text(id, text))
    }

    /// Sets a parameter from the host (not as automation within a block:
    /// for that send [`Event::Param`] to the processor). While active it
    /// reaches the plugin at the start of the next block; while inactive,
    /// a CLAP or VST2 plugin takes it at once and a VST3 one at its next
    /// activation (its controller, and so its editor, at once).
    pub fn set_param(&mut self, id: ParamId, value: f64) {
        each!(self.backend(), b => b.set_param(id, value));
    }

    /// The plugin's state as bytes, for a project to keep. CLAP: what its
    /// state extension writes. VST3: its component's and controller's
    /// states, in a small container of ours. VST2: its bank chunk, or
    /// (for plugins without chunks) its program number and every
    /// parameter's value, in a small container of ours.
    pub fn save_state(&mut self) -> Result<Vec<u8>, Error> {
        each!(self.backend(), b => b.save_state())
    }

    /// Restores a state [`Self::save_state`] made (active or not).
    /// Parameter values change: [`PluginEvent::ParamsRescanned`] follows.
    pub fn load_state(&mut self, state: &[u8]) -> Result<(), Error> {
        each!(self.backend(), b => b.load_state(state))?;
        self.inner
            .events
            .borrow_mut()
            .push(PluginEvent::ParamsRescanned { infos: false });
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        each!(self.backend(), b => b.is_active())
    }

    /// Prepares the plugin to process at `config`, and returns its
    /// processor for the audio thread.
    pub fn activate(&mut self, config: AudioConfig) -> Result<Processor, Error> {
        let backend = match self.backend() {
            Backend::Clap(b) => ProcessorBackend::Clap(Box::new(b.activate(config)?)),
            Backend::Vst3(b) => ProcessorBackend::Vst3(Box::new(b.activate(config)?)),
            Backend::Vst2(b) => ProcessorBackend::Vst2(Box::new(b.activate(config)?)),
        };
        self.inner.config.set(Some(config));
        self.inner.restart_requested.set(false);
        let latency = self.latency();
        if let Some(old) = self.inner.latency.replace(Some(latency))
            && old != latency
        {
            self.inner
                .events
                .borrow_mut()
                .push(PluginEvent::LatencyChanged(latency));
        }
        Ok(Processor {
            backend: Some(backend),
            graveyard: self.graveyard.clone(),
            main: self.main,
            serial: self.inner.serial,
        })
    }

    /// Stops the plugin, its processor back from the audio thread. A
    /// processor of another instance is dropped and nothing else happens.
    pub fn deactivate(&mut self, mut processor: Processor) {
        if processor.serial != self.inner.serial {
            return;
        }
        match (self.backend(), processor.backend.take()) {
            (Backend::Clap(b), Some(ProcessorBackend::Clap(p))) => b.deactivate(*p),
            (Backend::Vst3(b), Some(ProcessorBackend::Vst3(p))) => b.deactivate(*p),
            (Backend::Vst2(b), Some(ProcessorBackend::Vst2(p))) => b.deactivate(*p),
            _ => {}
        }
    }

    /// Deactivates and activates again with the same configuration: what
    /// [`PluginEvent::RestartRequested`] asks for. The new processor
    /// reflects the plugin's new latency and ports.
    pub fn restart(&mut self, processor: Processor) -> Result<Processor, Error> {
        let config = processor.config();
        self.deactivate(processor);
        self.activate(config)
    }

    /// The plugin asked for a restart it hasn't had yet.
    pub fn needs_restart(&self) -> bool {
        self.inner.restart_requested.get()
    }

    /// How many samples late its output is, as of its last activation.
    pub fn latency(&self) -> u32 {
        each!(self.backend(), b => b.latency())
    }

    /// How long it rings after its input goes silent.
    pub fn tail(&self) -> Tail {
        each!(self.backend(), b => b.tail())
    }

    /// What it did since the last call (after [`crate::Host::idle`]).
    pub fn take_events(&mut self) -> Vec<PluginEvent> {
        std::mem::take(&mut *self.inner.events.borrow_mut())
    }

    /// It has an editor this platform can show.
    pub fn has_editor(&self) -> bool {
        each!(self.backend(), b => b.has_editor())
    }

    /// Opens its editor in a window of its own (does nothing if open).
    pub fn open_editor(&mut self, options: &EditorOptions) -> Result<(), Error> {
        each!(self.backend(), b => b.open_editor(options))
    }

    pub fn close_editor(&mut self) {
        each!(self.backend(), b => b.close_editor());
    }

    pub fn editor_is_open(&self) -> bool {
        each!(self.backend(), b => b.editor_is_open())
    }

    /// Keeps the open editor above other windows, or stops doing so.
    pub fn set_editor_keep_on_top(&mut self, on_top: bool) {
        each!(self.backend(), b => b.set_editor_keep_on_top(on_top));
    }
}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        self.close_editor();
    }
}

enum ProcessorBackend {
    Clap(Box<ClapProcessor>),
    Vst3(Box<Vst3Processor>),
    Vst2(Box<Vst2Processor>),
}

/// Calls the same method on whichever format the processor is.
macro_rules! each_processor {
    ($backend:expr, $p:ident => $call:expr) => {
        match $backend {
            Some(ProcessorBackend::Clap($p)) => $call,
            Some(ProcessorBackend::Vst3($p)) => $call,
            Some(ProcessorBackend::Vst2($p)) => $call,
            None => unreachable!("a processor has its backend until it's dropped"),
        }
    };
}

/// An active plugin's audio half: `Send`, for the audio thread, where
/// every method is real-time safe as far as the host is concerned (no
/// allocation, locks or I/O; what the plugin does is up to it).
///
/// The processor owns buffers for every audio port of the plugin,
/// [`AudioConfig::max_block`] samples long. [`Self::process`] and
/// [`Self::process_stereo`] copy the caller's audio in and out of them;
/// [`Self::process_ports`] processes them as they are, for callers that
/// fill [`Self::input_mut`] and read [`Self::output`] themselves (multi-out
/// instruments).
///
/// Processing starts by itself on the first block. Call
/// [`Self::stop_processing`] on the audio thread before sending the
/// processor back to the main thread; the main thread does it otherwise.
pub struct Processor {
    backend: Option<ProcessorBackend>,
    graveyard: Arc<Graveyard>,
    main: MainThread,
    serial: u64,
}

impl Processor {
    pub fn config(&self) -> AudioConfig {
        each_processor!(&self.backend, p => p.config())
    }

    fn buffers(&mut self) -> (&mut PortBuffers, &mut PortBuffers) {
        each_processor!(&mut self.backend, p => p.buffers())
    }

    fn port_buffers(&self) -> (&PortBuffers, &PortBuffers) {
        each_processor!(&self.backend, p => p.port_buffers())
    }

    fn ports(&self) -> (Option<usize>, Option<usize>, Option<usize>) {
        each_processor!(&self.backend, p => p.ports())
    }

    fn out_events(&mut self) -> &mut OutEvents {
        each_processor!(&mut self.backend, p => p.out_events())
    }

    fn run(
        &mut self,
        frames: u32,
        events: &[Event],
        offset: u32,
        transport: &Transport,
    ) -> ProcessStatus {
        each_processor!(&mut self.backend, p => p.run(frames, events, offset, transport))
    }

    /// The plugin's audio inputs' and outputs' channel counts, port by port.
    pub fn channel_counts(&self) -> (Vec<usize>, Vec<usize>) {
        let (inputs, outputs) = self.port_buffers();
        let counts = |b: &PortBuffers| (0..b.port_count()).map(|p| b.channel_count(p)).collect();
        (counts(inputs), counts(outputs))
    }

    /// Input `port`'s `channel`, to fill before [`Self::process_ports`].
    pub fn input_mut(&mut self, port: usize, channel: usize) -> Option<&mut [f32]> {
        self.buffers().0.channel_mut(port, channel)
    }

    /// Output `port`'s `channel` after [`Self::process_ports`].
    pub fn output(&self, port: usize, channel: usize) -> Option<&[f32]> {
        self.port_buffers().1.channel(port, channel)
    }

    /// Processes one block: `inputs` into the plugin's main input,
    /// `aux` into its sidechain input (if it has one), and its main output
    /// into `outputs`; `events` sorted by frame. The block is as long as
    /// the shortest output (or input, with no outputs), split into blocks
    /// of at most `max_block`.
    ///
    /// Channels are matched as well as they go: one input channel feeds
    /// every plugin channel, two feed a mono plugin mixed; a mono plugin
    /// output goes to every output channel. If the plugin fails, outputs
    /// get the inputs unchanged (bypassed) or silence if there are none.
    pub fn process(
        &mut self,
        inputs: &[&[f32]],
        aux: &[&[f32]],
        outputs: &mut [&mut [f32]],
        events: &[Event],
        transport: &Transport,
    ) -> ProcessStatus {
        let frames = outputs
            .iter()
            .map(|o| o.len())
            .min()
            .or_else(|| inputs.iter().map(|i| i.len()).min())
            .unwrap_or(0);
        let mut status = ProcessStatus::Continue;
        self.out_events().clear();
        for (start, n) in chunks(frames, self.config().max_block as usize) {
            let (main_input, aux_input, main_output) = self.ports();
            let (ins, _) = self.buffers();
            if let Some(port) = main_input {
                ins.write_from(port, inputs, start, n);
            }
            if let Some(port) = aux_input {
                ins.write_from(port, aux, start, n);
            }
            let chunk = self.run(n as u32, events, start as u32, transport);
            let (_, outs) = self.buffers();
            if chunk == ProcessStatus::Error {
                bypass(inputs, outputs, start, n);
            } else if let Some(port) = main_output {
                outs.read_into(port, outputs, start, n);
            } else {
                silence(outputs, start, n);
            }
            status = worse(status, chunk);
        }
        self.out_events().sort();
        status
    }

    /// Processes a stereo signal in place (an effect's, or an instrument
    /// rendering into it), with an optional sidechain; as
    /// [`Self::process`] otherwise. If the plugin fails, `left` and `right`
    /// are left as they were for an effect, silenced for an instrument.
    pub fn process_stereo(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        sidechain: Option<(&[f32], &[f32])>,
        events: &[Event],
        transport: &Transport,
    ) -> ProcessStatus {
        let frames = left.len().min(right.len());
        let mut status = ProcessStatus::Continue;
        self.out_events().clear();
        for (start, n) in chunks(frames, self.config().max_block as usize) {
            let (main_input, aux_input, main_output) = self.ports();
            let (ins, _) = self.buffers();
            if let Some(port) = main_input {
                ins.write_from(port, &[&left[..], &right[..]], start, n);
            }
            if let (Some(port), Some((l, r))) = (aux_input, sidechain) {
                ins.write_from(port, &[l, r], start, n);
            }
            let chunk = self.run(n as u32, events, start as u32, transport);
            let (_, outs) = self.buffers();
            let mut outputs = [&mut left[..], &mut right[..]];
            match (chunk, main_output) {
                (ProcessStatus::Error, _) if main_input.is_some() => {}
                (ProcessStatus::Error, _) | (_, None) => silence(&mut outputs, start, n),
                (_, Some(port)) => outs.read_into(port, &mut outputs, start, n),
            }
            status = worse(status, chunk);
        }
        self.out_events().sort();
        status
    }

    /// Processes `frames` samples (at most `max_block`) of the processor's
    /// own buffers: fill inputs with [`Self::input_mut`] first, read
    /// outputs with [`Self::output`] after.
    pub fn process_ports(
        &mut self,
        frames: usize,
        events: &[Event],
        transport: &Transport,
    ) -> ProcessStatus {
        let frames = frames.min(self.config().max_block as usize);
        self.out_events().clear();
        let status = self.run(frames as u32, events, 0, transport);
        self.out_events().sort();
        status
    }

    /// Notes and MIDI the plugin sent during the last block (a note
    /// effect's output), sorted by frame.
    pub fn output_events(&self) -> &[Event] {
        each_processor!(&self.backend, p => p.sent_events()).as_slice()
    }

    /// Starts processing now rather than at the first block; false if the
    /// plugin refused (it then can't process).
    pub fn start_processing(&mut self) -> bool {
        each_processor!(&mut self.backend, p => p.start_processing())
    }

    /// Stops processing; the next block starts it again. Call it on the
    /// audio thread before the processor leaves it.
    pub fn stop_processing(&mut self) {
        each_processor!(&mut self.backend, p => p.stop_processing());
    }

    /// Silences whatever is ringing (delay lines, voices), as after a seek.
    /// VST2 has no such call: its plugins get their held notes ended and
    /// All Sound Off on every channel at the next block, which stops
    /// voices but not an effect's tail.
    pub fn reset(&mut self) {
        each_processor!(&mut self.backend, p => p.reset());
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        let Some(backend) = self.backend.take() else {
            return;
        };
        if self.main.is_current() {
            drop(backend);
        } else {
            // Stop on this thread (it's the one that processed), deactivate
            // and destroy on the main thread.
            let mut backend = backend;
            match &mut backend {
                ProcessorBackend::Clap(p) => p.stop_processing(),
                ProcessorBackend::Vst3(p) => p.stop_processing(),
                ProcessorBackend::Vst2(p) => p.stop_processing(),
            }
            self.graveyard.bury(Box::new(backend));
        }
    }
}

/// `(start, length)` of each part of a block of `frames` split at `max`.
fn chunks(frames: usize, max: usize) -> impl Iterator<Item = (usize, usize)> {
    let max = max.max(1);
    (0..frames.div_ceil(max)).map(move |i| (i * max, max.min(frames - i * max)))
}

fn silence(outputs: &mut [&mut [f32]], start: usize, n: usize) {
    for output in outputs.iter_mut() {
        if let Some(part) = output.get_mut(start..start + n) {
            part.fill(0.0);
        }
    }
}

/// Inputs to outputs as they are, for a plugin that failed.
fn bypass(inputs: &[&[f32]], outputs: &mut [&mut [f32]], start: usize, n: usize) {
    for (i, output) in outputs.iter_mut().enumerate() {
        let Some(part) = output.get_mut(start..start + n) else {
            continue;
        };
        match inputs
            .get(i)
            .or(inputs.last())
            .and_then(|input| input.get(start..start + n))
        {
            Some(input) => part.copy_from_slice(input),
            None => part.fill(0.0),
        }
    }
}

/// The status a block split into parts reports.
fn worse(a: ProcessStatus, b: ProcessStatus) -> ProcessStatus {
    if a == ProcessStatus::Error || b == ProcessStatus::Error {
        ProcessStatus::Error
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_blocks_split_into_whole_parts() {
        assert_eq!(
            chunks(10, 4).collect::<Vec<_>>(),
            vec![(0, 4), (4, 4), (8, 2)]
        );
        assert_eq!(chunks(8, 4).count(), 2);
        assert_eq!(chunks(0, 4).count(), 0);
    }
}
