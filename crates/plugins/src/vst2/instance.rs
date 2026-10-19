//! One VST2 plugin on the main thread.
//!
//! [`Vst2Plugin`] owns the `AEffect` and is shared (in an `Arc`) by the
//! main-thread [`Vst2Instance`] and, while active, the audio-thread
//! [`Vst2Processor`]; whichever goes last closes the plugin, on the main
//! thread (`crate::Processor` makes sure of that).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::abi::*;
use super::host::{self, Edit, HostShared, Loading};
use super::processor::{ProcessorSetup, Vst2Processor};
use super::{Module, text};
use crate::block::{self, Change, Modulation, OutEvent, Queues};
use crate::editor::{self, EditorOptions, Window, WindowEvent};
use crate::state::{self, Vst2Data};
use crate::thread::MainThread;
use crate::wake::Wake;
use crate::{
    AudioConfig, AudioPortInfo, Error, NotePortInfo, ParamFlags, ParamId, ParamInfo, PluginEvent,
    PluginFormat, PluginInfo, PluginKind, Tail,
};

/// Room for strings the plugin writes. The format's limits are 8 to 64
/// bytes, which plugins often overrun.
const TEXT_BUFFER: usize = 512;
/// How often an open editor gets `effEditIdle`.
const EDITOR_IDLE: Duration = Duration::from_millis(33);
/// The most parameters a plugin is taken to have, whatever it claims.
const MAX_PARAMS: i32 = 1 << 16;
/// The biggest chunk taken from a plugin.
const MAX_CHUNK: usize = 1 << 30;

/// A made and opened VST2 plugin.
pub(crate) struct Vst2Plugin {
    effect: *mut AEffect,
    pub shared: Arc<HostShared>,
    pub active: AtomicBool,
    pub main: MainThread,
    /// Released under the entry/exit lock (see `Module`'s drop).
    module: Option<Arc<Module>>,
}

// SAFETY: the plugin is called as hosts call VST2 plugins: the dispatcher
// and getParameter from the main thread (Vst2Instance is !Send), the
// process calls, effProcessEvents and the block's setParameter from
// whoever owns the one Vst2Processor.
unsafe impl Send for Vst2Plugin {}
unsafe impl Sync for Vst2Plugin {}

impl Vst2Plugin {
    /// Makes the plugin in `module`, checks it's the one `expected` names
    /// (if any), and opens it.
    pub fn new(
        module: Arc<Module>,
        expected: Option<i32>,
        main: MainThread,
        wake: Arc<Wake>,
    ) -> Result<Vst2Plugin, Error> {
        let path = module.path.display().to_string();
        let entry = module
            .entry
            .ok_or_else(|| Error::Load(format!("{path} is not a VST2 plugin")))?;
        let effect = {
            // Entry points aren't all safe to run at once.
            let _guard = crate::library::lock();
            let _loading = Loading::enter(expected.unwrap_or(0));
            // SAFETY: the entry takes the host callback and returns a new
            // plugin, or null.
            unsafe { entry(host::callback) }
        };
        if effect.is_null() {
            return Err(Error::Load(format!("{path}: the plugin wasn't made")));
        }
        // SAFETY: a non-null pointer from the entry, read through the raw
        // pointer. A plugin with the wrong magic is left alone: nothing in
        // it can be trusted, not even its close call.
        let (magic, unique_id) = unsafe { ((*effect).magic, (*effect).unique_id) };
        if magic != kEffectMagic {
            // Whatever it made stays, so its code must too.
            std::mem::forget(module);
            return Err(Error::Load(format!("{path} is not a VST2 plugin")));
        }
        let shared = Arc::new(HostShared::new(wake, unique_id, module.directory()));
        // SAFETY: `resvd1` is the host's; the plugin's calls find our state
        // there from now on. The Arc outlives the plugin (it's dropped after
        // effClose, in Drop).
        unsafe { (*effect).resvd1 = Arc::as_ptr(&shared) as isize };
        let plugin = Vst2Plugin {
            effect,
            shared,
            active: AtomicBool::new(false),
            main,
            module: Some(module),
        };
        if let Some(expected) = expected
            && expected != unique_id
        {
            return Err(Error::NotFound(super::id_to_string(expected)));
        }
        // SAFETY: the first call a plugin gets, then a sample rate and
        // block size to hold until it's activated.
        unsafe {
            plugin.dispatch(effOpen, 0, 0, std::ptr::null_mut(), 0.0);
            plugin.dispatch(effSetSampleRate, 0, 0, std::ptr::null_mut(), 48_000.0);
            plugin.dispatch(effSetBlockSize, 0, 512, std::ptr::null_mut(), 0.0);
        }
        Ok(plugin)
    }

    /// Calls the plugin's dispatcher.
    ///
    /// # Safety
    ///
    /// `ptr` (and `index` and `value`) must be what `opcode` takes, and the
    /// call made on the thread it belongs on.
    pub unsafe fn dispatch(
        &self,
        opcode: i32,
        index: i32,
        value: isize,
        ptr: *mut c_void,
        opt: f32,
    ) -> isize {
        // SAFETY: by the caller; the dispatcher is read through the raw
        // pointer and may be missing.
        unsafe {
            match (*self.effect).dispatcher {
                Some(dispatcher) => dispatcher(self.effect, opcode, index, value, ptr, opt),
                None => 0,
            }
        }
    }

    pub fn effect(&self) -> *mut AEffect {
        self.effect
    }

    /// The text an opcode that fills a string buffer gives.
    pub fn string(&self, opcode: i32, index: i32) -> String {
        let mut buf = [0u8; TEXT_BUFFER];
        // SAFETY: a main-thread string query with a buffer bigger than the
        // format's limits; text stops at the NUL or the buffer's end.
        unsafe { self.dispatch(opcode, index, 0, buf.as_mut_ptr().cast(), 0.0) };
        text(&buf)
    }

    /// What the plugin answers to `effCanDo` for `what`: 1 yes, -1 no, 0
    /// don't know.
    pub fn can_do(&self, what: &std::ffi::CStr) -> isize {
        // SAFETY: a main-thread query with a C string.
        unsafe { self.dispatch(effCanDo, 0, 0, what.as_ptr().cast_mut().cast(), 0.0) }
    }

    pub fn flags(&self) -> i32 {
        // SAFETY: a field of the live plugin, read through the raw pointer.
        unsafe { (*self.effect).flags }
    }

    pub fn num_params(&self) -> i32 {
        // SAFETY: as above.
        unsafe { (*self.effect).num_params }.clamp(0, MAX_PARAMS)
    }

    pub fn num_programs(&self) -> i32 {
        // SAFETY: as above.
        unsafe { (*self.effect).num_programs }.max(0)
    }

    /// Input and output channel counts, as the plugin has them now.
    pub fn channels(&self) -> (i32, i32) {
        // SAFETY: as above.
        unsafe { ((*self.effect).num_inputs, (*self.effect).num_outputs) }
    }

    pub fn initial_delay(&self) -> u32 {
        // SAFETY: as above.
        u32::try_from(unsafe { (*self.effect).initial_delay }).unwrap_or(0)
    }

    pub fn unique_id(&self) -> i32 {
        // SAFETY: as above.
        unsafe { (*self.effect).unique_id }
    }

    pub fn process_replacing(&self) -> Option<ProcessFn> {
        // SAFETY: as above.
        unsafe { (*self.effect).process_replacing }
    }

    /// A parameter's value, if `index` is one.
    pub fn get_parameter(&self, index: u32) -> Option<f32> {
        let index = i32::try_from(index)
            .ok()
            .filter(|&i| i < self.num_params())?;
        // SAFETY: an index in range; getParameter returns a float.
        unsafe {
            let get = (*self.effect).get_parameter?;
            Some(get(self.effect, index))
        }
    }

    /// Sets a parameter, if `index` is one (and `value` a number); on the
    /// main thread while inactive, the audio thread while active.
    pub fn set_parameter(&self, index: u32, value: f32) {
        let Some(index) = i32::try_from(index).ok().filter(|&i| i < self.num_params()) else {
            return;
        };
        if value.is_nan() {
            return;
        }
        // SAFETY: an index in range and a value in 0..=1.
        unsafe {
            if let Some(set) = (*self.effect).set_parameter {
                set(self.effect, index, value.clamp(0.0, 1.0));
            }
        }
    }

    /// Suspends the plugin; main thread, once processing has stopped.
    pub fn deactivate_now(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            // SAFETY: main-thread calls on a resumed plugin, undoing what
            // activation did, in reverse.
            unsafe {
                self.dispatch(effStopProcess, 0, 0, std::ptr::null_mut(), 0.0);
                self.dispatch(effMainsChanged, 0, 0, std::ptr::null_mut(), 0.0);
            }
        }
    }

    /// Refuses what this host can't run: shells, and plugins without
    /// `processReplacing`.
    pub fn check_supported(&self, path: &Path) -> Result<(), Error> {
        // SAFETY: a main-thread query.
        let category =
            unsafe { self.dispatch(effGetPlugCategory, 0, 0, std::ptr::null_mut(), 0.0) };
        if category == kPlugCategShell {
            return Err(Error::Load(format!(
                "{} is a VST2 shell (several plugins in one file); VST2 shells aren't supported",
                path.display()
            )));
        }
        if self.flags() & effFlagsCanReplacing == 0 || self.process_replacing().is_none() {
            return Err(Error::Load(format!(
                "{} only has VST2's old accumulating process call",
                path.display()
            )));
        }
        Ok(())
    }

    /// Whether it takes and sends MIDI.
    fn midi(&self) -> (bool, bool) {
        let input = self.flags() & effFlagsIsSynth != 0
            || self.can_do(c"receiveVstMidiEvent") == 1
            || self.can_do(c"receiveVstEvents") == 1;
        let output = self.can_do(c"sendVstMidiEvent") == 1 || self.can_do(c"sendVstEvents") == 1;
        (input, output)
    }

    /// What a scan says about it.
    pub fn describe(&self, path: &Path) -> PluginInfo {
        let mut name = self.string(effGetEffectName, 0);
        if name.is_empty() {
            name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        // SAFETY: main-thread queries.
        let (category, version) = unsafe {
            (
                self.dispatch(effGetPlugCategory, 0, 0, std::ptr::null_mut(), 0.0),
                self.dispatch(effGetVendorVersion, 0, 0, std::ptr::null_mut(), 0.0),
            )
        };
        let category_name = match category {
            kPlugCategEffect => "Effect",
            kPlugCategSynth => "Synth",
            kPlugCategAnalysis => "Analysis",
            kPlugCategMastering => "Mastering",
            kPlugCategSpacializer => "Spatializer",
            kPlugCategRoomFx => "Room",
            kPlugSurroundFx => "Surround",
            kPlugCategRestoration => "Restoration",
            kPlugCategOfflineProcess => "Offline",
            kPlugCategGenerator => "Generator",
            _ => "",
        };
        let (inputs, outputs) = self.channels();
        let (audio_inputs, audio_outputs) = super::audio_ports(inputs, outputs);
        let (midi_in, midi_out) = self.midi();
        let mut info = PluginInfo {
            format: PluginFormat::Vst2,
            id: super::id_to_string(self.unique_id()),
            name,
            vendor: self.string(effGetVendorString, 0),
            version: super::version_text(version),
            kind: PluginKind::Effect,
            categories: Some(category_name)
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .into_iter()
                .collect(),
            path: path.to_owned(),
            audio_inputs,
            audio_outputs,
            note_inputs: note_ports(midi_in, "MIDI In"),
            note_outputs: note_ports(midi_out, "MIDI Out"),
            bridged: crate::scan::is_bridged(PluginFormat::Vst2, path),
        };
        let instrument = self.flags() & effFlagsIsSynth != 0 || category == kPlugCategSynth;
        let effect = matches!(
            category,
            kPlugCategEffect
                | kPlugCategAnalysis
                | kPlugCategMastering
                | kPlugCategSpacializer
                | kPlugCategRoomFx
                | kPlugSurroundFx
                | kPlugCategRestoration
        );
        info.kind = crate::info::guess_kind(instrument, effect, false, &info);
        info
    }
}

fn note_ports(has: bool, name: &str) -> Vec<NotePortInfo> {
    if has {
        vec![NotePortInfo { name: name.into() }]
    } else {
        Vec::new()
    }
}

impl Drop for Vst2Plugin {
    fn drop(&mut self) {
        self.deactivate_now();
        // SAFETY: the plugin's last call, on the main thread (our owners see
        // to that); it frees itself.
        unsafe { self.dispatch(effClose, 0, 0, std::ptr::null_mut(), 0.0) };
        let _guard = crate::library::lock();
        self.module.take();
    }
}

/// An open editor: the plugin's GUI in our window.
struct Editor {
    window: Window,
    size: (u32, u32),
    next_idle: Instant,
}

/// A VST2 plugin on the main thread.
pub(crate) struct Vst2Instance {
    plugin: Arc<Vst2Plugin>,
    name: String,
    /// Parameter values right after opening, as their defaults.
    defaults: Vec<f32>,
    midi: (bool, bool),
    queues: RefCell<Option<Queues>>,
    /// The plugin's own edits made on other threads, for the processor's
    /// emulated modulation to build on.
    bases: RefCell<Option<rtrb::Producer<Change>>>,
    latency: Cell<u32>,
    editor: RefCell<Option<Editor>>,
}

impl Vst2Instance {
    pub fn new(
        module: Arc<Module>,
        info: &PluginInfo,
        main: MainThread,
        wake: Arc<Wake>,
    ) -> Result<Vst2Instance, Error> {
        let expected =
            super::string_to_id(&info.id).ok_or_else(|| Error::NotFound(info.id.clone()))?;
        let plugin = Vst2Plugin::new(module, Some(expected), main, wake)?;
        plugin.check_supported(&info.path)?;
        let defaults = (0..plugin.num_params() as u32)
            .map(|i| plugin.get_parameter(i).unwrap_or(0.0))
            .collect();
        let midi = plugin.midi();
        Ok(Vst2Instance {
            plugin: Arc::new(plugin),
            name: info.name.clone(),
            defaults,
            midi,
            queues: RefCell::new(None),
            bases: RefCell::new(None),
            latency: Cell::new(0),
            editor: RefCell::new(None),
        })
    }

    pub fn audio_ports(&self) -> (Vec<AudioPortInfo>, Vec<AudioPortInfo>) {
        let (inputs, outputs) = self.plugin.channels();
        super::audio_ports(inputs, outputs)
    }

    pub fn note_ports(&self) -> (Vec<NotePortInfo>, Vec<NotePortInfo>) {
        (
            note_ports(self.midi.0, "MIDI In"),
            note_ports(self.midi.1, "MIDI Out"),
        )
    }

    fn properties(&self, index: i32) -> Option<VstParameterProperties> {
        // SAFETY: a plain C struct for which all zeroes is valid.
        let mut properties: VstParameterProperties = unsafe { std::mem::zeroed() };
        // SAFETY: a main-thread query with the struct to fill.
        let filled = unsafe {
            self.plugin.dispatch(
                effGetParameterProperties,
                index,
                0,
                (&mut properties as *mut VstParameterProperties).cast(),
                0.0,
            )
        };
        (filled == 1).then_some(properties)
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        (0..self.plugin.num_params())
            .map(|index| {
                let mut name = self.plugin.string(effGetParamName, index);
                if name.is_empty() {
                    name = format!("Parameter {}", index + 1);
                }
                let mut step_count = 0;
                let mut module = String::new();
                if let Some(p) = self.properties(index) {
                    let has = |flag: i32| p.flags & flag != 0;
                    if has(kVstParameterIsSwitch) {
                        step_count = 1;
                    } else if has(kVstParameterUsesIntegerMinMax) {
                        let range = i64::from(p.max_integer) - i64::from(p.min_integer);
                        let step = if has(kVstParameterUsesIntStep) {
                            i64::from(p.step_integer).max(1)
                        } else {
                            1
                        };
                        step_count = (range / step).clamp(0, i64::from(u32::MAX)) as u32;
                    }
                    if has(kVstParameterSupportsDisplayCategory) && p.category > 0 {
                        module = text(&p.category_label);
                    }
                }
                ParamInfo {
                    id: ParamId(index as u32),
                    name,
                    module,
                    unit: self.plugin.string(effGetParamLabel, index),
                    min: 0.0,
                    max: 1.0,
                    default: f64::from(
                        self.defaults
                            .get(index as usize)
                            .copied()
                            .unwrap_or(0.0)
                            .clamp(0.0, 1.0),
                    ),
                    step_count,
                    flags: ParamFlags {
                        stepped: step_count > 0,
                        automatable: true,
                        ..ParamFlags::default()
                    },
                }
            })
            .collect()
    }

    pub fn param_value(&self, id: ParamId) -> Option<f64> {
        self.plugin.get_parameter(id.0).map(f64::from)
    }

    /// The plugin's text for the parameter's current value: VST2 can't
    /// show any other, so other values have none.
    pub fn param_text(&self, id: ParamId, value: f64) -> Option<String> {
        let current = self.plugin.get_parameter(id.0)?;
        let is_current = ((value as f32) - current).abs() <= 1e-6;
        if !is_current {
            return None;
        }
        let index = id.0 as i32;
        let display = self.plugin.string(effGetParamDisplay, index);
        if display.is_empty() {
            return None;
        }
        let label = self.plugin.string(effGetParamLabel, index);
        Some(if label.is_empty() || display.ends_with(&label) {
            display
        } else {
            format!("{display} {label}")
        })
    }

    /// Always `None`: VST2's `effString2Parameter` sets the parameter from
    /// the text rather than converting it, so it can't answer without
    /// changing the sound.
    pub fn param_from_text(&self, _id: ParamId, _text: &str) -> Option<f64> {
        None
    }

    /// Sets a parameter from the host: through the audio thread while
    /// active, directly otherwise.
    pub fn set_param(&self, id: ParamId, value: f64) {
        let value = if value.is_nan() {
            0.0
        } else {
            value.clamp(0.0, 1.0)
        };
        if self.is_active() {
            if let Some(queues) = self.queues.borrow_mut().as_mut() {
                let _ = queues.to_audio.push(Change { id, value });
            }
        } else {
            self.plugin.set_parameter(id.0, value as f32);
        }
    }

    /// The plugin's bank chunk, if it keeps its state in one.
    fn chunk(&self) -> Option<Vec<u8>> {
        if self.plugin.flags() & effFlagsProgramChunks == 0 {
            return None;
        }
        let mut data: *mut c_void = std::ptr::null_mut();
        // SAFETY: a main-thread call that points `data` at the plugin's
        // chunk and returns its size.
        let size = unsafe {
            self.plugin.dispatch(
                effGetChunk,
                0,
                0,
                (&mut data as *mut *mut c_void).cast(),
                0.0,
            )
        };
        let size = usize::try_from(size)
            .ok()
            .filter(|&s| s > 0 && s <= MAX_CHUNK)?;
        if data.is_null() {
            return None;
        }
        // SAFETY: the plugin's chunk of `size` bytes, valid until its next
        // call; copied at once.
        Some(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) }.to_vec())
    }

    pub fn save_state(&self) -> Result<Vec<u8>, Error> {
        // SAFETY: a main-thread query.
        let program = unsafe {
            self.plugin
                .dispatch(effGetProgram, 0, 0, std::ptr::null_mut(), 0.0)
        };
        let program = program.clamp(0, i32::MAX as isize) as i32;
        let data = match self.chunk() {
            Some(chunk) => {
                return Ok(state::vst2(
                    self.plugin.unique_id(),
                    program,
                    &Vst2Data::Chunk(&chunk),
                ));
            }
            None => Vst2Data::Params(
                (0..self.plugin.num_params() as u32)
                    .map(|index| self.plugin.get_parameter(index).unwrap_or(0.0))
                    .collect(),
            ),
        };
        Ok(state::vst2(self.plugin.unique_id(), program, &data))
    }

    pub fn load_state(&self, state: &[u8]) -> Result<(), Error> {
        let bad = || Error::State(format!("not a VST2 state saved by canopy ({})", self.name));
        let (unique_id, program, data) = state::parse_vst2(state).ok_or_else(bad)?;
        if unique_id != self.plugin.unique_id() {
            return Err(Error::State(format!(
                "that state is another plugin's (not {})",
                self.name
            )));
        }
        match data {
            Vst2Data::Chunk(chunk) => {
                let mut chunk = chunk.to_vec();
                if !chunk.is_empty() {
                    // SAFETY: a main-thread call with the chunk and its
                    // size; the plugin may keep reading it during the call
                    // only. Plugins differ in what they return, so it's not
                    // checked.
                    unsafe {
                        self.plugin.dispatch(
                            effSetChunk,
                            0,
                            chunk.len() as isize,
                            chunk.as_mut_ptr().cast(),
                            0.0,
                        )
                    };
                }
            }
            Vst2Data::Params(values) => {
                if (0..self.plugin.num_programs()).contains(&program) {
                    // SAFETY: main-thread program calls with a program in
                    // range, bracketed as the format asks.
                    unsafe {
                        let null = std::ptr::null_mut();
                        self.plugin.dispatch(effBeginSetProgram, 0, 0, null, 0.0);
                        self.plugin
                            .dispatch(effSetProgram, 0, program as isize, null, 0.0);
                        self.plugin.dispatch(effEndSetProgram, 0, 0, null, 0.0);
                    }
                }
                for (index, value) in values.into_iter().enumerate() {
                    self.plugin.set_parameter(index as u32, value);
                }
            }
        }
        // While active, the processor's emulated modulation builds on the
        // values just loaded.
        if self.is_active()
            && let Some(bases) = self.bases.borrow_mut().as_mut()
        {
            for index in 0..self.plugin.num_params() as u32 {
                if let Some(value) = self.plugin.get_parameter(index) {
                    let _ = bases.push(Change {
                        id: ParamId(index),
                        value: f64::from(value),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.plugin.active.load(Ordering::Acquire)
    }

    pub fn activate(&self, config: AudioConfig) -> Result<Vst2Processor, Error> {
        if self.is_active() {
            return Err(Error::Plugin(format!("{} is already active", self.name)));
        }
        let max_block = config.max_block.max(1);
        let plugin = &self.plugin;
        plugin
            .shared
            .set_config(config.sample_rate, max_block, config.offline);
        // SAFETY: main-thread setup calls on a suspended plugin, in the
        // order hosts make them: rate, block size and precision while
        // suspended, then resume and start.
        unsafe {
            let null = std::ptr::null_mut();
            plugin.dispatch(effSetSampleRate, 0, 0, null, config.sample_rate as f32);
            plugin.dispatch(effSetBlockSize, 0, max_block as isize, null, 0.0);
            plugin.dispatch(effSetProcessPrecision, 0, kVstProcessPrecision32, null, 0.0);
            plugin.dispatch(effMainsChanged, 0, 1, null, 0.0);
            plugin.dispatch(effStartProcess, 0, 0, null, 0.0);
        }
        plugin.active.store(true, Ordering::Release);
        // Whatever it said before it was resumed is answered by resuming.
        plugin.shared.io_changed.store(false, Ordering::Release);
        self.latency.set(plugin.initial_delay());

        let (inputs, outputs) = plugin.channels();
        let modulation = Modulation::new((0..plugin.num_params() as u32).map(|index| {
            let value = plugin.get_parameter(index).unwrap_or(0.0);
            (index, f64::from(value), 0.0, 1.0, false)
        }));
        let (queues, audio_queues) = block::queues();
        let (bases, from_bases) = rtrb::RingBuffer::new(block::TO_AUDIO_CAPACITY);
        *self.queues.borrow_mut() = Some(queues);
        *self.bases.borrow_mut() = Some(bases);
        let input_channels = super::channel_groups(inputs);
        let output_channels = super::channel_groups(outputs);
        let setup = ProcessorSetup {
            config: AudioConfig {
                max_block,
                min_block: config.min_block.clamp(1, max_block),
                ..config
            },
            main_input: (!input_channels.is_empty()).then_some(0),
            aux_input: (input_channels.len() > 1).then_some(1),
            main_output: (!output_channels.is_empty()).then_some(0),
            input_channels,
            output_channels,
            modulation,
        };
        Ok(Vst2Processor::new(
            plugin.clone(),
            setup,
            audio_queues,
            from_bases,
        ))
    }

    /// Deactivates the plugin, its processor back from the audio thread.
    pub fn deactivate(&self, mut processor: Vst2Processor) {
        let unsent = processor.take_queued();
        drop(processor);
        // Dropped on the main thread, the processor suspended the plugin
        // already; this covers a processor dropped elsewhere.
        self.plugin.deactivate_now();
        self.queues.borrow_mut().take();
        self.bases.borrow_mut().take();
        // Inactive now, the plugin takes them at once.
        for change in unsent {
            self.set_param(change.id, change.value);
        }
    }

    pub fn latency(&self) -> u32 {
        self.latency.get()
    }

    pub fn tail(&self) -> Tail {
        // SAFETY: a main-thread query.
        let tail = unsafe {
            self.plugin
                .dispatch(effGetTailSize, 0, 0, std::ptr::null_mut(), 0.0)
        };
        // 0 means the plugin doesn't say and 1 that it has none.
        match u32::try_from(tail) {
            Ok(0 | 1) => Tail::Samples(0),
            Ok(samples) if samples < i32::MAX as u32 => Tail::Samples(samples),
            _ => Tail::Infinite,
        }
    }

    /// Runs the editor and reports what the plugin did. Returns when it
    /// next needs to run (the editor's next idle).
    pub fn idle(&self, events: &mut Vec<PluginEvent>) -> Option<Instant> {
        let next = self.idle_editor(events);
        let shared = &self.plugin.shared;
        let edits = std::mem::take(&mut *shared.edits.lock().unwrap_or_else(|e| e.into_inner()));
        for edit in edits {
            events.push(match edit {
                Edit::Begin(id) => PluginEvent::ParamGesture {
                    id: ParamId(id),
                    begin: true,
                },
                Edit::End(id) => PluginEvent::ParamGesture {
                    id: ParamId(id),
                    begin: false,
                },
                Edit::Value(id, value) => {
                    let change = Change {
                        id: ParamId(id),
                        value: f64::from(value),
                    };
                    if let Some(bases) = self.bases.borrow_mut().as_mut() {
                        let _ = bases.push(change);
                    }
                    PluginEvent::ParamValue {
                        id: change.id,
                        value: change.value,
                    }
                }
            });
        }
        if let Some(queues) = self.queues.borrow_mut().as_mut() {
            while let Ok(event) = queues.from_audio.pop() {
                events.push(match event {
                    OutEvent::Param(id, value) => PluginEvent::ParamValue { id, value },
                    OutEvent::Gesture(id, begin) => PluginEvent::ParamGesture { id, begin },
                });
            }
        }
        // Latency and channel counts only change at activation.
        if shared.io_changed.swap(false, Ordering::AcqRel) && self.is_active() {
            events.push(PluginEvent::RestartRequested);
        }
        if shared.update_display.swap(false, Ordering::AcqRel) {
            events.push(PluginEvent::ParamsRescanned { infos: true });
        }
        next
    }

    pub fn watched_fds(&self, out: &mut Vec<i32>) {
        if let Some(editor) = self.editor.borrow().as_ref() {
            out.extend(editor.window.fd());
        }
    }

    pub fn has_editor(&self) -> bool {
        editor::VST2_EDITOR && self.plugin.flags() & effFlagsHasEditor != 0
    }

    pub fn editor_is_open(&self) -> bool {
        self.editor.borrow().is_some()
    }

    /// The editor's size, if the plugin gives a sensible one.
    fn editor_rect(&self) -> Option<(u32, u32)> {
        let mut rect: *mut ERect = std::ptr::null_mut();
        // SAFETY: a main-thread call that points `rect` at the plugin's
        // rectangle, read at once.
        let rect = unsafe {
            self.plugin.dispatch(
                effEditGetRect,
                0,
                0,
                (&mut rect as *mut *mut ERect).cast(),
                0.0,
            );
            rect.as_ref().copied()?
        };
        let width = u32::try_from(i32::from(rect.right) - i32::from(rect.left)).ok()?;
        let height = u32::try_from(i32::from(rect.bottom) - i32::from(rect.top)).ok()?;
        (width > 0 && height > 0).then_some((width, height))
    }

    pub fn open_editor(&self, options: &EditorOptions) -> Result<(), Error> {
        if self.editor_is_open() {
            return Ok(());
        }
        if !self.has_editor() {
            return Err(Error::Editor(format!(
                "{} has no editor we can show",
                self.name
            )));
        }
        // Some plugins only know their size once open.
        let (width, height) = self.editor_rect().unwrap_or((400, 300));
        let window = Window::open(options, width, height, false)?;
        *self
            .plugin
            .shared
            .resize
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // SAFETY: a main-thread call with our window's native handle, which
        // outlives the editor (close_editor closes the editor first). What
        // plugins return differs too much to check.
        unsafe {
            self.plugin
                .dispatch(effEditOpen, 0, 0, window.parent().as_ptr(), 0.0)
        };
        let size = self.editor_rect().unwrap_or((width, height));
        if size != (width, height) {
            window.set_size(size.0, size.1, false);
        }
        *self.editor.borrow_mut() = Some(Editor {
            window,
            size,
            next_idle: Instant::now(),
        });
        Ok(())
    }

    pub fn set_editor_keep_on_top(&self, on_top: bool) {
        if let Some(editor) = self.editor.borrow().as_ref() {
            editor.window.set_keep_on_top(on_top);
        }
    }

    pub fn close_editor(&self) {
        let Some(_editor) = self.editor.borrow_mut().take() else {
            return;
        };
        // SAFETY: a main-thread call closing the editor we opened.
        unsafe {
            self.plugin
                .dispatch(effEditClose, 0, 0, std::ptr::null_mut(), 0.0)
        };
        // The window closes as `_editor` goes, after the plugin let go of it.
    }

    /// Gives the editor its idle call when due, applies the size the plugin
    /// asked for and handles the window's events. Returns when the editor
    /// next needs idling.
    fn idle_editor(&self, events: &mut Vec<PluginEvent>) -> Option<Instant> {
        let now = Instant::now();
        let due = self.editor.borrow().as_ref()?.next_idle <= now;
        if due {
            // SAFETY: a main-thread call on an open editor.
            unsafe {
                self.plugin
                    .dispatch(effEditIdle, 0, 0, std::ptr::null_mut(), 0.0)
            };
        }
        let resize = self
            .plugin
            .shared
            .resize
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let window_events = {
            let mut slot = self.editor.borrow_mut();
            let editor = slot.as_mut()?;
            if due {
                editor.next_idle += EDITOR_IDLE;
                if editor.next_idle <= now {
                    editor.next_idle = now + EDITOR_IDLE;
                }
            }
            if let Some((width, height)) = resize {
                editor.size = (width, height);
                editor.window.set_size(width, height, false);
            }
            editor.window.poll()
        };
        for event in window_events {
            match event {
                WindowEvent::CloseRequested => {
                    self.close_editor();
                    events.push(PluginEvent::EditorClosed);
                    return None;
                }
                // VST2 editors size themselves: undo the user's resizing.
                WindowEvent::Resized(width, height) => {
                    if let Some(editor) = self.editor.borrow().as_ref()
                        && editor.size != (width, height)
                    {
                        editor.window.set_size(editor.size.0, editor.size.1, false);
                    }
                }
            }
        }
        self.editor.borrow().as_ref().map(|e| e.next_idle)
    }
}

impl Drop for Vst2Instance {
    fn drop(&mut self) {
        self.close_editor();
    }
}
