//! The host callback (`audioMaster`) every VST2 plugin gets, and the state
//! behind it.
//!
//! VST2 has one callback for all plugins, called with the plugin's
//! `AEffect`. Each plugin's [`HostShared`] is found from there through
//! `AEffect::resvd1`, a field the format reserves for the host, set as soon
//! as the plugin exists. Plugins call the host while their entry point
//! runs too, before that (with a null effect, or one whose `resvd1` isn't
//! set yet): a thread-local marks those calls, which get answers that need
//! no state.
//!
//! A plugin may call from any thread. What it reports from its editor or
//! another thread (parameter edits) is queued under a lock for the next
//! idle; what it reports from inside a processor call goes into the
//! processor's [`AudioSink`], which the processor publishes for the length
//! of each such call, so the audio thread never locks. Requests that just
//! need acting on later (a restart, a display update, a window size) set
//! flags or slots that the instance picks up when the host idles.

use std::cell::{Cell, UnsafeCell};
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::abi::*;
use crate::ParamId;
use crate::block::{self, Modulation, OutEvent, OutEvents};
use crate::thread;
use crate::wake::Wake;

thread_local! {
    /// While an entry point runs on this thread: the unique id being
    /// loaded (0 when unknown), for `audioMasterCurrentId`.
    static LOADING: Cell<Option<i32>> = const { Cell::new(None) };
}

/// Marks this thread as running a plugin's entry point until dropped.
pub(crate) struct Loading {
    was: Option<i32>,
}

impl Loading {
    pub fn enter(id: i32) -> Loading {
        Loading {
            was: LOADING.replace(Some(id)),
        }
    }
}

impl Drop for Loading {
    fn drop(&mut self) {
        LOADING.set(self.was);
    }
}

/// Something the plugin did to one of its parameters, as its editor does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Edit {
    Begin(u32),
    Value(u32, f32),
    End(u32),
}

/// What a processor lends the callback while it's calling the plugin on
/// the audio thread: where the plugin's edits, output events and time
/// info go, without locks or allocation.
pub(crate) struct AudioSink {
    pub to_main: rtrb::Producer<OutEvent>,
    /// Notes and MIDI the plugin sent during this block.
    pub events: OutEvents,
    pub modulation: Modulation,
    /// The transport of the part of the block being processed.
    pub time: VstTimeInfo,
    /// Where that part starts, in frames of the whole `process` call.
    pub frame: u32,
}

impl AudioSink {
    fn edit(&mut self, edit: Edit) {
        let event = match edit {
            Edit::Begin(id) => OutEvent::Gesture(ParamId(id), true),
            Edit::End(id) => OutEvent::Gesture(ParamId(id), false),
            Edit::Value(id, value) => {
                let value = f64::from(value);
                self.modulation.plugin_set(ParamId(id), value);
                OutEvent::Param(ParamId(id), value)
            }
        };
        let _ = self.to_main.push(event);
    }

    /// Takes the notes and MIDI in a plugin's `VstEvents`.
    ///
    /// # Safety
    ///
    /// `list` must be null or a `VstEvents` whose pointers are null or
    /// point at events at least as big as their type says.
    unsafe fn take_events(&mut self, list: *const VstEvents) {
        if list.is_null() {
            return;
        }
        // SAFETY: by the caller; fields are read through the raw pointer,
        // and the count is bounded so a garbage count can't run far.
        unsafe {
            let count = usize::try_from((*list).num_events)
                .unwrap_or(0)
                .min(block::EVENT_CAPACITY);
            let events = std::ptr::addr_of!((*list).events).cast::<*const VstEvent>();
            for i in 0..count {
                let event = *events.add(i);
                if event.is_null() || (*event).type_ != kVstMidiType {
                    continue;
                }
                let midi = &*event.cast::<VstMidiEvent>();
                let [status, a, b, _] = midi.midi_data;
                // Channel messages only (no SysEx or realtime bytes).
                if !(0x80..0xF0).contains(&status) {
                    continue;
                }
                let frame = self
                    .frame
                    .saturating_add(u32::try_from(midi.delta_frames).unwrap_or(0));
                let data = [status, a & 127, b & 127];
                let event =
                    block::midi_to_note(frame, data).unwrap_or(crate::Event::Midi { frame, data });
                if !self.events.push(event) {
                    break;
                }
            }
        }
    }
}

/// Everything behind one plugin's callback. The plugin's `resvd1` holds
/// its address while the plugin lives.
pub(crate) struct HostShared {
    pub wake: Arc<Wake>,
    /// The plugin's unique id, for `audioMasterCurrentId`.
    pub unique_id: i32,
    /// The plugin file's folder, for `audioMasterGetDirectory`.
    directory: CString,
    sample_rate: AtomicU64,
    block_size: AtomicU32,
    offline: AtomicBool,
    /// It said its inputs, outputs or latency changed.
    pub io_changed: AtomicBool,
    /// It asked the host to read its parameters again.
    pub update_display: AtomicBool,
    /// Edits made outside the audio thread, for the next idle.
    pub edits: Mutex<Vec<Edit>>,
    /// The size it asked its editor window to be.
    pub resize: Mutex<Option<(u32, u32)>>,
    /// The sink of the processor calling the plugin right now, and the
    /// thread it's calling from.
    sink: AtomicPtr<AudioSink>,
    sink_thread: AtomicU64,
    /// What `audioMasterGetTime` returns outside processing: a stopped
    /// transport. Never written after it's made; the cell is because the
    /// plugin gets a mutable pointer to it.
    idle_time: UnsafeCell<VstTimeInfo>,
}

// SAFETY: every field is thread-safe but `idle_time`, which the host never
// writes after construction (plugins only read it through the pointer
// they're given).
unsafe impl Send for HostShared {}
unsafe impl Sync for HostShared {}

/// The sample rate and block size plugins are told before they're
/// activated.
const DEFAULT_RATE: f64 = 48_000.0;
const DEFAULT_BLOCK: u32 = 512;

impl HostShared {
    pub fn new(wake: Arc<Wake>, unique_id: i32, directory: &std::path::Path) -> HostShared {
        let directory = CString::new(directory.to_string_lossy().as_bytes()).unwrap_or_default();
        HostShared {
            wake,
            unique_id,
            directory,
            sample_rate: AtomicU64::new(DEFAULT_RATE.to_bits()),
            block_size: AtomicU32::new(DEFAULT_BLOCK),
            offline: AtomicBool::new(false),
            io_changed: AtomicBool::new(false),
            update_display: AtomicBool::new(false),
            edits: Mutex::new(Vec::new()),
            resize: Mutex::new(None),
            sink: AtomicPtr::new(std::ptr::null_mut()),
            sink_thread: AtomicU64::new(0),
            idle_time: UnsafeCell::new(VstTimeInfo {
                sample_rate: DEFAULT_RATE,
                tempo: 120.0,
                time_sig_numerator: 4,
                time_sig_denominator: 4,
                flags: kVstTempoValid | kVstTimeSigValid | kVstPpqPosValid | kVstBarsValid,
                ..VstTimeInfo::default()
            }),
        }
    }

    /// What the plugin is told about the processing it's activated for.
    pub fn set_config(&self, sample_rate: f64, block_size: u32, offline: bool) {
        self.sample_rate
            .store(sample_rate.to_bits(), Ordering::Release);
        self.block_size.store(block_size, Ordering::Release);
        self.offline.store(offline, Ordering::Release);
    }

    /// The state of the plugin `effect`, if it's one of ours.
    ///
    /// # Safety
    ///
    /// `effect` must be null or a live plugin whose `resvd1` is null or a
    /// live `HostShared` (true of every plugin this crate made, once its
    /// entry point has returned).
    unsafe fn of<'a>(effect: *mut AEffect) -> Option<&'a HostShared> {
        if effect.is_null() {
            return None;
        }
        // SAFETY: by the caller; the field is read through the raw pointer
        // rather than a reference, since the plugin owns the struct.
        unsafe { ((*effect).resvd1 as *const HostShared).as_ref() }
    }

    /// Publishes `sink` for the plugin's calls from this thread until the
    /// returned guard drops.
    pub fn publish(&self, sink: *mut AudioSink) -> Published<'_> {
        self.sink_thread
            .store(thread::current_thread(), Ordering::Relaxed);
        self.sink.store(sink, Ordering::Release);
        Published(self)
    }

    /// The published sink, if the calling thread is the one that published
    /// it.
    #[allow(clippy::mut_from_ref)]
    fn sink(&self) -> Option<&mut AudioSink> {
        let sink = self.sink.load(Ordering::Acquire);
        if sink.is_null() || self.sink_thread.load(Ordering::Relaxed) != thread::current_thread() {
            return None;
        }
        // SAFETY: the processor published it from this very thread and is
        // inside a call to the plugin, holding no reference to it; it
        // withdraws it before touching it again. Each use below is brief
        // and never calls the plugin, so no two references overlap.
        Some(unsafe { &mut *sink })
    }

    fn edit(&self, edit: Edit) {
        if let Some(sink) = self.sink() {
            sink.edit(edit);
            self.wake.request();
        } else if !thread::is_audio_thread() {
            self.edits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(edit);
            self.wake.request();
        }
        // An audio thread with nothing published: dropped rather than
        // locked for.
    }

    fn process_level(&self) -> isize {
        if self.sink().is_some() || thread::is_audio_thread() {
            if self.offline.load(Ordering::Acquire) {
                kVstProcessLevelOffline
            } else {
                kVstProcessLevelRealtime
            }
        } else {
            kVstProcessLevelUser
        }
    }

    fn time_info(&self) -> *mut VstTimeInfo {
        match self.sink() {
            Some(sink) => &mut sink.time,
            None => self.idle_time.get(),
        }
    }
}

/// Withdraws a published sink when dropped.
pub(crate) struct Published<'a>(&'a HostShared);

impl Drop for Published<'_> {
    fn drop(&mut self) {
        self.0.sink.store(std::ptr::null_mut(), Ordering::Release);
    }
}

/// What the host can do, as plugins ask with `audioMasterCanDo`.
const HOST_CAN_DO: &[&[u8]] = &[
    b"sendVstEvents",
    b"sendVstMidiEvent",
    b"sendVstTimeInfo",
    b"receiveVstEvents",
    b"receiveVstMidiEvent",
    b"sizeWindow",
    b"acceptIOChanges",
    b"startStopProcess",
];

/// A C string from a plugin, up to `max` bytes, without allocating.
///
/// # Safety
///
/// `ptr` must be null or point at a NUL-terminated string or at least
/// `max` readable bytes.
pub(crate) unsafe fn bounded_str<'a>(ptr: *const c_char, max: usize) -> &'a [u8] {
    if ptr.is_null() {
        return &[];
    }
    let mut len = 0;
    // SAFETY: by the caller; reading stops at the NUL or at `max`.
    unsafe {
        while len < max && *ptr.add(len) != 0 {
            len += 1;
        }
        std::slice::from_raw_parts(ptr.cast::<u8>(), len)
    }
}

/// Copies `text` into a buffer of `len` bytes the plugin gave, truncated
/// and NUL-terminated.
///
/// # Safety
///
/// `ptr` must be null or point at `len` writable bytes.
pub(crate) unsafe fn write_str(ptr: *mut c_void, len: usize, text: &str) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let n = text.len().min(len - 1);
    // SAFETY: by the caller; `n + 1 <= len` bytes are written.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), ptr.cast::<u8>(), n);
        *ptr.cast::<u8>().add(n) = 0;
    }
}

/// The host callback: what plugins call to ask the host things and tell
/// it what they did.
pub(crate) unsafe extern "C" fn callback(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize {
    let loading = LOADING.get();
    // SAFETY: outside an entry point on this thread, a plugin's `resvd1`
    // is ours (or null): it's set as soon as the entry returns.
    let shared = if loading.is_some() {
        None
    } else {
        unsafe { HostShared::of(effect) }
    };
    let id = u32::try_from(index).ok();
    match opcode {
        audioMasterVersion => kVstVersion,
        audioMasterCurrentId => shared.map_or(loading.unwrap_or(0), |s| s.unique_id) as isize,
        audioMasterAutomate => {
            if let (Some(shared), Some(id)) = (shared, id) {
                shared.edit(Edit::Value(id, opt));
            }
            0
        }
        audioMasterBeginEdit | audioMasterEndEdit => match (shared, id) {
            (Some(shared), Some(id)) => {
                shared.edit(if opcode == audioMasterBeginEdit {
                    Edit::Begin(id)
                } else {
                    Edit::End(id)
                });
                1
            }
            _ => 0,
        },
        audioMasterGetTime => shared.map_or(0, |s| s.time_info() as isize),
        audioMasterProcessEvents => match shared.and_then(HostShared::sink) {
            Some(sink) => {
                // SAFETY: the plugin passes a VstEvents of its own, valid
                // for the call.
                unsafe { sink.take_events(ptr.cast_const().cast()) };
                1
            }
            None => 0,
        },
        audioMasterIOChanged => match shared {
            Some(shared) => {
                shared.io_changed.store(true, Ordering::Release);
                shared.wake.request();
                1
            }
            None => 0,
        },
        audioMasterUpdateDisplay => match shared {
            Some(shared) => {
                shared.update_display.store(true, Ordering::Release);
                shared.wake.request();
                1
            }
            None => 0,
        },
        audioMasterSizeWindow => match (shared, u32::try_from(index), u32::try_from(value)) {
            (Some(shared), Ok(width @ 1..), Ok(height @ 1..)) if !thread::is_audio_thread() => {
                *shared.resize.lock().unwrap_or_else(|e| e.into_inner()) = Some((width, height));
                shared.wake.request();
                1
            }
            _ => 0,
        },
        audioMasterGetSampleRate => shared.map_or(DEFAULT_RATE, |s| {
            f64::from_bits(s.sample_rate.load(Ordering::Acquire))
        }) as isize,
        audioMasterGetBlockSize => {
            shared.map_or(DEFAULT_BLOCK, |s| s.block_size.load(Ordering::Acquire)) as isize
        }
        audioMasterGetCurrentProcessLevel => {
            shared.map_or(kVstProcessLevelUser, HostShared::process_level)
        }
        audioMasterGetAutomationState => kVstAutomationReadWrite,
        audioMasterGetVendorString | audioMasterGetProductString => {
            // SAFETY: the plugin passes a buffer of the format's size.
            unsafe { write_str(ptr, kVstMaxVendorStrLen, "canopy") };
            1
        }
        audioMasterGetVendorVersion => 100,
        audioMasterCanDo => {
            // SAFETY: the plugin passes a C string.
            let asked = unsafe { bounded_str(ptr.cast_const().cast(), 64) };
            isize::from(HOST_CAN_DO.contains(&asked))
        }
        audioMasterGetLanguage => kVstLangEnglish,
        audioMasterGetDirectory => shared.map_or(0, |s| s.directory.as_ptr() as isize),
        // Old plugins say they want MIDI; every plugin gets it.
        audioMasterWantMidi => 1,
        _ => 0,
    }
}
