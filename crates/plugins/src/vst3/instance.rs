//! One VST3 plugin on the main thread: its component and controller,
//! connected and kept in sync.
//!
//! [`Vst3Plugin`] owns both objects and is shared (in an `Arc`) by the
//! main-thread [`Vst3Instance`] and, while active, the audio-thread
//! [`Vst3Processor`]; whichever goes last terminates the plugin, on the main
//! thread (`crate::Processor` makes sure of that).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper, Interface};

use super::com::{
    self, ComponentHandler, ConnectionProxy, ForeignEdit, HandlerShared, HostApplication,
    MemoryStream, PlugFrame, RunLoop,
};
use super::processor::{ProcessorSetup, Vst3Processor};
use super::{Module, find_class};
use crate::block::{self, Change, Modulation, OutEvent};
use crate::editor::{self, EditorOptions, Window, WindowEvent};
use crate::thread::{AudioThreadScope, MainThread};
use crate::wake::Wake;
use crate::{
    AudioConfig, AudioPortInfo, Error, NotePortInfo, ParamFlags, ParamId, ParamInfo, PluginEvent,
    PluginInfo, Tail,
};

fn ok(result: tresult) -> bool {
    result == kResultOk
}

/// Creates class `cid` from the module's factory, as interface `I`.
fn create<I: Interface>(module: &Module, cid: &TUID) -> Option<ComPtr<I>> {
    let mut object: *mut c_void = std::ptr::null_mut();
    // SAFETY: a class id and interface id of 16 bytes each, and somewhere to
    // write the new object, which we then own.
    unsafe {
        let result =
            module
                .factory()
                .createInstance(cid.as_ptr(), I::IID.as_ptr().cast(), &mut object);
        if !ok(result) {
            return None;
        }
        ComPtr::from_raw(object.cast::<I>())
    }
}

/// An initialized component and controller.
pub(crate) struct Vst3Plugin {
    pub component: ComPtr<IComponent>,
    pub processor: ComPtr<IAudioProcessor>,
    pub controller: Option<ComPtr<IEditController>>,
    /// The controller is its own object (not the component itself).
    separate: bool,
    /// Each connection point and the proxy we connected it to.
    connections: Vec<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    pub active: AtomicBool,
    pub processing: AtomicBool,
    pub main: MainThread,
    pub wake: Arc<Wake>,
    _handler: ComWrapper<ComponentHandler>,
    _host: ComWrapper<HostApplication>,
    pub module: Arc<Module>,
}

// SAFETY: the component and controller are called as VST3's thread rules
// allow: everything but `process` and `setProcessing` from the main thread
// (Vst3Instance is !Send), those two from whoever owns the one
// Vst3Processor.
unsafe impl Send for Vst3Plugin {}
unsafe impl Sync for Vst3Plugin {}

impl Vst3Plugin {
    /// Calls `setActive(false)`; main thread, once processing has stopped.
    pub fn deactivate_now(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            // SAFETY: main-thread call on an active component.
            unsafe { self.component.setActive(0) };
        }
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        // SAFETY: the teardown order the SDK's hosting code uses: stop,
        // deactivate, disconnect, then terminate controller and component.
        unsafe {
            if self.processing.swap(false, Ordering::AcqRel) {
                let _audio = AudioThreadScope::enter();
                self.processor.setProcessing(0);
            }
            self.deactivate_now();
            for (point, proxy) in &self.connections {
                point.disconnect(proxy.as_ptr());
            }
            if let Some(controller) = &self.controller {
                controller.setComponentHandler(std::ptr::null_mut());
                if self.separate {
                    controller.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

/// Bus infos of one media type and direction.
fn buses(
    component: &ComPtr<IComponent>,
    media: MediaTypes,
    direction: BusDirections,
) -> Vec<BusInfo> {
    // SAFETY: main-thread bus queries with structs to fill.
    unsafe {
        let count = component.getBusCount(media as MediaType, direction as BusDirection);
        (0..count)
            .filter_map(|i| {
                let mut info: BusInfo = std::mem::zeroed();
                ok(component.getBusInfo(
                    media as MediaType,
                    direction as BusDirection,
                    i,
                    &mut info,
                ))
                .then_some(info)
            })
            .collect()
    }
}

type Buses = (
    Vec<AudioPortInfo>,
    Vec<AudioPortInfo>,
    Vec<NotePortInfo>,
    Vec<NotePortInfo>,
);

fn describe_buses(component: &ComPtr<IComponent>) -> Buses {
    let audio = |direction| {
        buses(component, MediaTypes_::kAudio, direction)
            .iter()
            .map(|b| AudioPortInfo {
                name: com::wide(&b.name),
                channels: b.channelCount.max(0) as u32,
                main: b.busType == BusTypes_::kMain as BusType,
            })
            .collect()
    };
    let notes = |direction| {
        buses(component, MediaTypes_::kEvent, direction)
            .iter()
            .map(|b| NotePortInfo {
                name: com::wide(&b.name),
            })
            .collect()
    };
    (
        audio(BusDirections_::kInput),
        audio(BusDirections_::kOutput),
        notes(BusDirections_::kInput),
        notes(BusDirections_::kOutput),
    )
}

/// A class's buses, from a component made just to ask (for scanning).
pub(crate) fn read_buses(module: &Module, cid: &TUID) -> Result<Buses, Error> {
    let component = create::<IComponent>(module, cid)
        .ok_or_else(|| Error::Plugin("createInstance failed".into()))?;
    let host = HostApplication::new(Arc::new(RunLoop::new(MainThread::current())));
    let context = host.to_com_ptr::<FUnknown>().expect("host has FUnknown");
    // SAFETY: initialize, query, terminate: the component's whole life.
    unsafe {
        if !ok(component.initialize(context.as_ptr())) {
            return Err(Error::Plugin("initialize failed".into()));
        }
        let buses = describe_buses(&component);
        component.terminate();
        Ok(buses)
    }
}

/// An open editor: the plugin's view in our window.
struct Editor {
    view: ComPtr<IPlugView>,
    frame: ComWrapper<PlugFrame>,
    window: Arc<Window>,
}

/// A VST3 plugin on the main thread.
pub(crate) struct Vst3Instance {
    plugin: Arc<Vst3Plugin>,
    name: String,
    run_loop: Arc<RunLoop>,
    handler: Arc<HandlerShared>,
    from_audio: RefCell<Option<rtrb::Consumer<OutEvent>>>,
    latency: Cell<u32>,
    editor: RefCell<Option<Editor>>,
    /// Whether it has an editor we can show, once asked.
    has_editor: Cell<Option<bool>>,
}

impl Vst3Instance {
    pub fn new(
        module: Arc<Module>,
        info: &PluginInfo,
        main: MainThread,
        wake: Arc<Wake>,
    ) -> Result<Vst3Instance, Error> {
        let name = info.name.clone();
        let cid = find_class(&module, &info.id).ok_or_else(|| Error::NotFound(info.id.clone()))?;
        let run_loop = Arc::new(RunLoop::new(main));
        let host = HostApplication::new(run_loop.clone());
        let context = host.to_com_ptr::<FUnknown>().expect("host has FUnknown");
        let failed = |what: &str| Error::Plugin(format!("{name}: {what} failed"));
        let component =
            create::<IComponent>(&module, &cid).ok_or_else(|| failed("createInstance"))?;
        // SAFETY: main-thread setup calls, in the order the SDK documents:
        // initialize the component, find or make the controller, connect
        // them, give the controller our handler and the component's state.
        unsafe {
            if !ok(component.initialize(context.as_ptr())) {
                return Err(failed("initialize"));
            }
            let Some(processor) = component.cast::<IAudioProcessor>() else {
                component.terminate();
                return Err(Error::Plugin(format!("{name} has no audio processor")));
            };
            let (controller, separate) = match component.cast::<IEditController>() {
                Some(controller) => (Some(controller), false),
                None => {
                    let mut controller_cid: TUID = [0; 16];
                    let controller = (ok(component.getControllerClassId(&mut controller_cid))
                        && controller_cid != [0; 16])
                        .then(|| create::<IEditController>(&module, &controller_cid))
                        .flatten()
                        .filter(|c| ok(c.initialize(context.as_ptr())));
                    (controller, true)
                }
            };
            let mut connections = Vec::new();
            if separate
                && let Some(controller) = &controller
                && let (Some(a), Some(b)) = (
                    component.cast::<IConnectionPoint>(),
                    controller.cast::<IConnectionPoint>(),
                )
            {
                for (point, peer) in [(a.clone(), b.clone()), (b, a)] {
                    let proxy = ConnectionProxy::new(peer, main)
                        .to_com_ptr::<IConnectionPoint>()
                        .expect("proxy is a connection point");
                    point.connect(proxy.as_ptr());
                    connections.push((point, proxy));
                }
            }
            let shared = HandlerShared::new(main, wake.clone());
            let handler = ComponentHandler::new(shared.clone());
            if let Some(controller) = &controller {
                let handler_ptr = handler.to_com_ptr::<IComponentHandler>().expect("handler");
                controller.setComponentHandler(handler_ptr.as_ptr());
                let stream = MemoryStream::new(Vec::new());
                let stream_ptr = stream.to_com_ptr::<IBStream>().expect("stream");
                if ok(component.getState(stream_ptr.as_ptr())) {
                    stream.rewind();
                    controller.setComponentState(stream_ptr.as_ptr());
                }
            }
            let instance = Vst3Instance {
                plugin: Arc::new(Vst3Plugin {
                    component,
                    processor,
                    controller,
                    separate,
                    connections,
                    active: AtomicBool::new(false),
                    processing: AtomicBool::new(false),
                    main,
                    wake,
                    _handler: handler,
                    _host: host,
                    module,
                }),
                name,
                run_loop,
                handler: shared,
                from_audio: RefCell::new(None),
                latency: Cell::new(0),
                editor: RefCell::new(None),
                has_editor: Cell::new(None),
            };
            Ok(instance)
        }
    }

    fn controller(&self) -> Option<&ComPtr<IEditController>> {
        self.plugin.controller.as_ref()
    }

    pub fn audio_ports(&self) -> (Vec<AudioPortInfo>, Vec<AudioPortInfo>) {
        let (inputs, outputs, _, _) = describe_buses(&self.plugin.component);
        (inputs, outputs)
    }

    pub fn note_ports(&self) -> (Vec<NotePortInfo>, Vec<NotePortInfo>) {
        let (_, _, inputs, outputs) = describe_buses(&self.plugin.component);
        (inputs, outputs)
    }

    /// Unit ids and their paths ("Osc 1/Shape"), from `IUnitInfo`.
    fn unit_paths(&self) -> Vec<(i32, String)> {
        let Some(units) = self.controller().and_then(|c| c.cast::<IUnitInfo>()) else {
            return Vec::new();
        };
        // SAFETY: main-thread unit queries with structs to fill.
        let infos: Vec<UnitInfo> = unsafe {
            (0..units.getUnitCount())
                .filter_map(|i| {
                    let mut info: UnitInfo = std::mem::zeroed();
                    ok(units.getUnitInfo(i, &mut info)).then_some(info)
                })
                .collect()
        };
        let path = |mut id: i32| {
            let mut parts = Vec::new();
            while id != kRootUnitId && parts.len() < 16 {
                let Some(unit) = infos.iter().find(|u| u.id == id) else {
                    break;
                };
                parts.push(com::wide(&unit.name));
                id = unit.parentUnitId;
            }
            parts.reverse();
            parts.join("/")
        };
        infos.iter().map(|u| (u.id, path(u.id))).collect()
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        let Some(controller) = self.controller() else {
            return Vec::new();
        };
        let units = self.unit_paths();
        // SAFETY: main-thread parameter queries with structs to fill.
        let infos: Vec<ParameterInfo> = unsafe {
            (0..controller.getParameterCount())
                .filter_map(|i| {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    ok(controller.getParameterInfo(i, &mut info)).then_some(info)
                })
                .collect()
        };
        use ParameterInfo_::ParameterFlags_::*;
        infos
            .iter()
            .map(|p| {
                let has = |flag: ParameterInfo_::ParameterFlags| p.flags & flag != 0;
                let step_count = p.stepCount.max(0) as u32;
                ParamInfo {
                    id: ParamId(p.id),
                    name: com::wide(&p.title),
                    module: units
                        .iter()
                        .find(|u| u.0 == p.unitId)
                        .map(|u| u.1.clone())
                        .unwrap_or_default(),
                    unit: com::wide(&p.units),
                    min: 0.0,
                    max: 1.0,
                    default: p.defaultNormalizedValue.clamp(0.0, 1.0),
                    step_count,
                    flags: ParamFlags {
                        stepped: step_count > 0,
                        enumerated: has(kIsList),
                        automatable: has(kCanAutomate),
                        modulatable: false,
                        periodic: has(kIsWrapAround),
                        read_only: has(kIsReadOnly),
                        hidden: has(kIsHidden) || has(kIsProgramChange),
                        bypass: has(kIsBypass),
                    },
                }
            })
            .collect()
    }

    pub fn param_value(&self, id: ParamId) -> Option<f64> {
        // SAFETY: main-thread query.
        self.controller()
            .map(|c| unsafe { c.getParamNormalized(id.0) })
    }

    pub fn param_text(&self, id: ParamId, value: f64) -> Option<String> {
        let controller = self.controller()?;
        let mut text: String128 = [0; 128];
        // SAFETY: main-thread query with a String128 to fill.
        ok(unsafe { controller.getParamStringByValue(id.0, value, &mut text) })
            .then(|| com::wide(&text))
    }

    pub fn param_from_text(&self, id: ParamId, text: &str) -> Option<f64> {
        let controller = self.controller()?;
        let mut wide: Vec<TChar> = text.encode_utf16().chain(Some(0)).collect();
        let mut value = 0.0;
        // SAFETY: main-thread query with a NUL-terminated string.
        ok(unsafe { controller.getParamValueByString(id.0, wide.as_mut_ptr(), &mut value) })
            .then_some(value)
    }

    /// Sets a parameter from the host: on the controller (so the editor
    /// shows it) and on its way to the processor.
    pub fn set_param(&self, id: ParamId, value: f64) {
        let value = value.clamp(0.0, 1.0);
        if let Some(controller) = self.controller() {
            // SAFETY: main-thread call.
            unsafe { controller.setParamNormalized(id.0, value) };
        }
        if let Some(main) = self.handler.main.get() {
            main.borrow_mut().send(Change { id, value });
        }
    }

    /// Changes made while inactive only reached the controller. Before
    /// saving, give them to the component the one way VST3 allows: a short
    /// activation with a block of no samples.
    // VST3's enum constants are u32 on Linux and i32 on Windows.
    #[allow(clippy::unnecessary_cast)]
    fn flush_while_inactive(&self) {
        let Some(main) = self.handler.main.get() else {
            return;
        };
        if self.is_active() || main.borrow().pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut main.borrow_mut().pending);
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: 512,
            sampleRate: 48_000.0,
        };
        // SAFETY: main-thread setup calls on an inactive component, undone
        // at once.
        unsafe {
            let (processor, component) = (&self.plugin.processor, &self.plugin.component);
            let activated = ok(processor.setupProcessing(&mut setup)) && {
                let result = component.setActive(1);
                result == kResultOk || result == kNotImplemented
            };
            if !activated {
                main.borrow_mut().pending = pending;
                return;
            }
            super::processor::flush(&self.plugin, &pending);
            component.setActive(0);
        }
        // The restart it may have asked for is answered by the next
        // activation.
        self.handler.restart.store(0, Ordering::Release);
    }

    pub fn save_state(&self) -> Result<Vec<u8>, Error> {
        self.flush_while_inactive();
        let fail = || Error::State(format!("{} failed to save its state", self.name));
        let component = MemoryStream::new(Vec::new());
        let controller = MemoryStream::new(Vec::new());
        // SAFETY: main-thread state calls with streams to write to.
        unsafe {
            let stream = component.to_com_ptr::<IBStream>().ok_or_else(fail)?;
            if !ok(self.plugin.component.getState(stream.as_ptr())) {
                return Err(fail());
            }
            if let Some(c) = self.controller() {
                let stream = controller.to_com_ptr::<IBStream>().ok_or_else(fail)?;
                // A controller with nothing of its own to save may refuse.
                c.getState(stream.as_ptr());
            }
        }
        Ok(crate::state::vst3(&component.bytes(), &controller.bytes()))
    }

    pub fn load_state(&self, state: &[u8]) -> Result<(), Error> {
        let bad = || Error::State(format!("not a VST3 state saved by canopy ({})", self.name));
        let (component, controller) = crate::state::parse_vst3(state).ok_or_else(bad)?;
        let mut parts = vec![component.to_vec(), controller.to_vec()];
        let fail = || Error::State(format!("{} failed to load its state", self.name));
        // The state wins over changes still waiting for an activation.
        if let Some(main) = self.handler.main.get() {
            main.borrow_mut().pending.clear();
        }
        let component = MemoryStream::new(parts.remove(0));
        let controller_state = parts.remove(0);
        // SAFETY: main-thread state calls, as the SDK's hosts make them: the
        // component's state to the component, then to the controller, then
        // the controller's own.
        unsafe {
            let stream = component.to_com_ptr::<IBStream>().ok_or_else(fail)?;
            if !ok(self.plugin.component.setState(stream.as_ptr())) {
                return Err(fail());
            }
            if let Some(c) = self.controller() {
                component.rewind();
                c.setComponentState(stream.as_ptr());
                if !controller_state.is_empty() {
                    let controller = MemoryStream::new(controller_state);
                    let stream = controller.to_com_ptr::<IBStream>().ok_or_else(fail)?;
                    c.setState(stream.as_ptr());
                }
            }
        }
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.plugin.active.load(Ordering::Acquire)
    }

    // VST3's enum constants are u32 on Linux and i32 on Windows.
    #[allow(clippy::unnecessary_cast)]
    pub fn activate(&self, config: AudioConfig) -> Result<Vst3Processor, Error> {
        if self.is_active() {
            return Err(Error::Plugin(format!("{} is already active", self.name)));
        }
        let fail = |what: &str| Error::Plugin(format!("{}: {what} failed", self.name));
        let max_block = config.max_block.max(1);
        let component = &self.plugin.component;
        let processor = &self.plugin.processor;
        let (inputs, outputs, note_inputs, note_outputs) = describe_buses(component);
        let main_input = crate::info::main_port_index(&inputs);
        let aux_input = crate::info::aux_port_index(&inputs);
        let main_output = crate::info::main_port_index(&outputs);
        // SAFETY: main-thread setup calls on an inactive component.
        unsafe {
            let audio = MediaTypes_::kAudio as MediaType;
            let event = MediaTypes_::kEvent as MediaType;
            let (input, output) = (
                BusDirections_::kInput as BusDirection,
                BusDirections_::kOutput as BusDirection,
            );
            // Main buses, the sidechain and whatever is active by default;
            // the rest stay off (a drum machine's sixteen extra outputs).
            for (i, bus) in buses(component, MediaTypes_::kAudio, BusDirections_::kInput)
                .iter()
                .enumerate()
            {
                let on = Some(i) == main_input || Some(i) == aux_input || bus.flags & 1 != 0;
                component.activateBus(audio, input, i as i32, u8::from(on));
            }
            for (i, bus) in buses(component, MediaTypes_::kAudio, BusDirections_::kOutput)
                .iter()
                .enumerate()
            {
                let on = Some(i) == main_output || bus.flags & 1 != 0;
                component.activateBus(audio, output, i as i32, u8::from(on));
            }
            if !note_inputs.is_empty() {
                component.activateBus(event, input, 0, 1);
            }
            if !note_outputs.is_empty() {
                component.activateBus(event, output, 0, 1);
            }
            let sample32 = SymbolicSampleSizes_::kSample32 as i32;
            if processor.canProcessSampleSize(sample32) != kResultTrue {
                return Err(Error::Plugin(format!(
                    "{} can't process 32-bit samples",
                    self.name
                )));
            }
            let mode = if config.offline {
                ProcessModes_::kOffline
            } else {
                ProcessModes_::kRealtime
            };
            let mut setup = ProcessSetup {
                processMode: mode as i32,
                symbolicSampleSize: sample32,
                maxSamplesPerBlock: max_block as i32,
                sampleRate: config.sample_rate,
            };
            if !ok(processor.setupProcessing(&mut setup)) {
                return Err(fail("setupProcessing"));
            }
            let activated = component.setActive(1);
            if activated != kResultOk && activated != kNotImplemented {
                return Err(fail("setActive"));
            }
        }
        self.plugin.active.store(true, Ordering::Release);
        // Changes made while inactive only reached the controller: give
        // them to the processor too, in a block of no samples (VST3's way
        // to flush parameters), before anything depends on them.
        let pending = match self.handler.main.get() {
            Some(main) => std::mem::take(&mut main.borrow_mut().pending),
            None => Vec::new(),
        };
        if !pending.is_empty() {
            super::processor::flush(&self.plugin, &pending);
        }
        // A latency change asked for while inactive or by those changes
        // needs one more activation to show; the rest is answered by
        // activating at all.
        let flags = self.handler.restart.swap(0, Ordering::AcqRel);
        if flags & RestartFlags_::kLatencyChanged as i32 != 0 {
            // SAFETY: main-thread calls on an active component that isn't
            // processing.
            unsafe {
                component.setActive(0);
                component.setActive(1);
            }
        }
        // SAFETY: main-thread query once set up.
        self.latency.set(unsafe { processor.getLatencySamples() });

        // Controllers and pitch bend: which parameters the plugin maps them
        // to (VST3 has no MIDI controller events of its own).
        let mut midi_map = [None; 130];
        if let Some(mapping) = self.controller().and_then(|c| c.cast::<IMidiMapping>()) {
            for (number, slot) in midi_map.iter_mut().enumerate() {
                let mut id = 0;
                // SAFETY: main-thread query with somewhere to write.
                let mapped =
                    unsafe { mapping.getMidiControllerAssignment(0, 0, number as i16, &mut id) };
                if mapped == kResultTrue {
                    *slot = Some(id);
                }
            }
        }
        // Per-note tuning, if the plugin lists it among its note
        // expressions (on the first event bus's first channel, which
        // carries every note sent).
        let tuning = self
            .controller()
            .and_then(|c| c.cast::<INoteExpressionController>())
            .is_some_and(|expressions| {
                // SAFETY: main-thread queries with somewhere to write.
                let count = unsafe { expressions.getNoteExpressionCount(0, 0) };
                (0..count).any(|i| {
                    // SAFETY: as above; all zeroes is a valid info to fill.
                    let mut info: NoteExpressionTypeInfo = unsafe { std::mem::zeroed() };
                    let found = unsafe { expressions.getNoteExpressionInfo(0, 0, i, &mut info) };
                    found == kResultOk && info.typeId == NoteExpressionTypeIDs_::kTuningTypeID
                })
            });
        let modulation = Modulation::new(self.params().iter().map(|p| {
            let value = self.param_value(p.id).unwrap_or(p.default);
            (p.id.0, value, 0.0, 1.0, false)
        }));
        let (queues, audio_queues) = block::queues();
        let mut to_audio = queues.to_audio;
        if let Some(main) = self.handler.main.get() {
            let mut main = main.borrow_mut();
            for change in main.pending.drain(..) {
                let _ = to_audio.push(change);
            }
            main.to_audio = Some(to_audio);
        }
        *self.from_audio.borrow_mut() = Some(queues.from_audio);
        let channels =
            |ports: &[AudioPortInfo]| ports.iter().map(|p| p.channels).collect::<Vec<_>>();
        let setup = ProcessorSetup {
            config: AudioConfig {
                max_block,
                min_block: config.min_block.clamp(1, max_block),
                ..config
            },
            input_channels: channels(&inputs),
            output_channels: channels(&outputs),
            main_input,
            aux_input,
            main_output,
            note_input: !note_inputs.is_empty(),
            note_output: !note_outputs.is_empty(),
            midi_map,
            tuning,
            modulation,
        };
        Ok(Vst3Processor::new(self.plugin.clone(), setup, audio_queues))
    }

    /// Deactivates the plugin, its processor back from the audio thread.
    pub fn deactivate(&self, mut processor: Vst3Processor) {
        let unsent = processor.take_queued();
        drop(processor);
        // Dropped on the main thread, the processor stopped processing and
        // deactivated already; this covers a processor dropped elsewhere.
        self.plugin.deactivate_now();
        if let Some(main) = self.handler.main.get() {
            let mut main = main.borrow_mut();
            main.to_audio = None;
            // They reach the processor at the next activation.
            for change in unsent {
                main.send(change);
            }
        }
        self.from_audio.borrow_mut().take();
    }

    pub fn latency(&self) -> u32 {
        self.latency.get()
    }

    pub fn tail(&self) -> Tail {
        // SAFETY: main-thread query.
        let samples = unsafe { self.plugin.processor.getTailSamples() };
        if samples == kInfiniteTail {
            Tail::Infinite
        } else {
            Tail::Samples(samples)
        }
    }

    /// Runs what the plugin registered and reports what it did. Returns
    /// when it next needs to run (its next timer).
    pub fn idle(&self, events: &mut Vec<PluginEvent>) -> Option<Instant> {
        let next = match (self.run_loop.idle(), self.plugin.module.run_loop.idle()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let foreign = std::mem::take(
            &mut *self
                .handler
                .foreign
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if let Some(main) = self.handler.main.get() {
            let mut main = main.borrow_mut();
            for edit in foreign {
                match edit {
                    ForeignEdit::Begin(id) => main.events.push(PluginEvent::ParamGesture {
                        id: ParamId(id),
                        begin: true,
                    }),
                    ForeignEdit::Perform(id, value) => main.perform(id, value),
                    ForeignEdit::End(id) => main.events.push(PluginEvent::ParamGesture {
                        id: ParamId(id),
                        begin: false,
                    }),
                }
            }
            events.append(&mut main.events);
        }
        let mut from_audio = Vec::new();
        if let Some(queue) = self.from_audio.borrow_mut().as_mut() {
            while let Ok(event) = queue.pop() {
                from_audio.push(event);
            }
        }
        for event in from_audio {
            match event {
                OutEvent::Param(id, value) => {
                    // The processor changed it: show it on the controller.
                    if let Some(controller) = self.controller() {
                        // SAFETY: main-thread call.
                        unsafe { controller.setParamNormalized(id.0, value) };
                    }
                    events.push(PluginEvent::ParamValue { id, value });
                }
                OutEvent::Gesture(id, begin) => {
                    events.push(PluginEvent::ParamGesture { id, begin })
                }
            }
        }
        use RestartFlags_::*;
        let flags = self.handler.restart.swap(0, Ordering::AcqRel);
        let any = |mask: RestartFlags| flags & mask != 0;
        if any(kReloadComponent | kIoChanged | kLatencyChanged) && self.is_active() {
            events.push(PluginEvent::RestartRequested);
        }
        if any(kParamTitlesChanged | kParamIDMappingChanged) {
            events.push(PluginEvent::ParamsRescanned { infos: true });
        } else if any(kParamValuesChanged) {
            events.push(PluginEvent::ParamsRescanned { infos: false });
        }
        if self.handler.dirty.swap(false, Ordering::AcqRel) {
            events.push(PluginEvent::StateDirty);
        }
        self.idle_editor(events);
        next
    }

    /// The registered file descriptors, for a thread that waits on them.
    pub fn watched_fds(&self, out: &mut Vec<i32>) {
        self.run_loop.fds(out);
        self.plugin.module.run_loop.fds(out);
        if let Some(editor) = self.editor.borrow().as_ref() {
            out.extend(editor.window.fd());
        }
    }

    fn create_view(&self) -> Option<ComPtr<IPlugView>> {
        let controller = self.controller()?;
        // SAFETY: main-thread call; we own the view it returns.
        unsafe { ComPtr::from_raw(controller.createView(c"editor".as_ptr())) }
    }

    pub fn has_editor(&self) -> bool {
        let Some(platform) = editor::VST3_PLATFORM else {
            return false;
        };
        if self.editor.borrow().is_some() {
            return true;
        }
        if let Some(known) = self.has_editor.get() {
            return known;
        }
        // VST3 can only say by making a view; once is enough.
        let has = self.create_view().is_some_and(|view| {
            // SAFETY: main-thread query with a static C string.
            unsafe { view.isPlatformTypeSupported(platform.as_ptr()) == kResultTrue }
        });
        self.has_editor.set(Some(has));
        has
    }

    pub fn editor_is_open(&self) -> bool {
        self.editor.borrow().is_some()
    }

    pub fn open_editor(&self, options: &EditorOptions) -> Result<(), Error> {
        if self.editor_is_open() {
            return Ok(());
        }
        let no_editor = || Error::Editor(format!("{} has no editor we can show", self.name));
        let platform = editor::VST3_PLATFORM.ok_or_else(no_editor)?;
        let view = self.create_view().ok_or_else(no_editor)?;
        // SAFETY: main-thread view calls in the SDK's order: check the
        // platform, set the frame, size the window, attach.
        unsafe {
            if view.isPlatformTypeSupported(platform.as_ptr()) != kResultTrue {
                return Err(no_editor());
            }
            let frame = PlugFrame::new(self.run_loop.clone(), self.plugin.main);
            let frame_ptr = frame.to_com_ptr::<IPlugFrame>().expect("frame");
            view.setFrame(frame_ptr.as_ptr());
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            view.getSize(&mut rect);
            let width = (rect.right - rect.left).max(1) as u32;
            let height = (rect.bottom - rect.top).max(1) as u32;
            let resizable = view.canResize() == kResultTrue;
            let window = match Window::open(options, width, height, resizable) {
                Ok(window) => Arc::new(window),
                Err(e) => {
                    view.setFrame(std::ptr::null_mut());
                    return Err(e);
                }
            };
            if let Some(state) = frame.state.get() {
                *state.borrow_mut() = com::FrameState {
                    window: Some(window.clone()),
                    size: (width, height),
                    resizable,
                };
            }
            if !ok(view.attached(window.parent().as_ptr(), platform.as_ptr())) {
                view.setFrame(std::ptr::null_mut());
                return Err(Error::Editor(format!("{} refused our window", self.name)));
            }
            *self.editor.borrow_mut() = Some(Editor {
                view,
                frame,
                window,
            });
        }
        Ok(())
    }

    pub fn set_editor_keep_on_top(&self, on_top: bool) {
        if let Some(editor) = self.editor.borrow().as_ref() {
            editor.window.set_keep_on_top(on_top);
        }
    }

    pub fn close_editor(&self) {
        let Some(editor) = self.editor.borrow_mut().take() else {
            return;
        };
        // SAFETY: main-thread calls detaching the view we attached.
        unsafe {
            editor.view.removed();
            editor.view.setFrame(std::ptr::null_mut());
        }
        if let Some(state) = editor.frame.state.get() {
            state.borrow_mut().window = None;
        }
        // The window closes as `editor` goes, after the plugin let go of it.
    }

    fn idle_editor(&self, events: &mut Vec<PluginEvent>) {
        let window_events = match self.editor.borrow().as_ref() {
            Some(editor) => editor.window.poll(),
            None => return,
        };
        for event in window_events {
            match event {
                WindowEvent::CloseRequested => {
                    self.close_editor();
                    events.push(PluginEvent::EditorClosed);
                    return;
                }
                WindowEvent::Resized(width, height) => self.window_resized(width, height),
            }
        }
    }

    /// The user resized the window: let the view pick the nearest size it
    /// can be, and fit the window to that.
    fn window_resized(&self, width: u32, height: u32) {
        let editor = self.editor.borrow();
        let Some(editor) = editor.as_ref() else {
            return;
        };
        let Some(state) = editor.frame.state.get() else {
            return;
        };
        let (size, resizable) = {
            let state = state.borrow();
            (state.size, state.resizable)
        };
        if (width, height) == size {
            return;
        }
        if !resizable {
            editor.window.set_size(size.0, size.1, false);
            return;
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        // SAFETY: main-thread view calls on an attached view.
        unsafe {
            editor.view.checkSizeConstraint(&mut rect);
            editor.view.onSize(&mut rect);
        }
        let fitted = (
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        );
        state.borrow_mut().size = fitted;
        if fitted != (width, height) {
            editor.window.set_size(fitted.0, fitted.1, true);
        }
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        self.close_editor();
        self.run_loop.clear();
    }
}
