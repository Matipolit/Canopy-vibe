//! The COM objects the host gives VST3 plugins, implemented in Rust with
//! the `vst3` crate's `Class`/`ComWrapper`:
//!
//! - [`HostApplication`]: the context plugins are initialized with; makes
//!   the messages and attribute lists components and controllers send each
//!   other.
//! - [`ComponentHandler`]: how a controller tells the host about edits in
//!   its editor (begin, perform, end) and asks for restarts.
//! - [`PlugFrame`]: an editor's frame, for resizing; on Linux also the
//!   `IRunLoop` plugin GUIs run their timers and X11 connection through.
//! - [`ConnectionProxy`]: sits between a component and its controller and
//!   only passes messages on the main thread.
//! - [`MemoryStream`]: `IBStream` over bytes, for state.
//!
//! Plugins may call these from any thread. Those that touch main-thread
//! state check the thread first (through [`MainThreadCell`]).

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vst3::Steinberg::Linux::{
    IEventHandler, IEventHandlerTrait, IRunLoopTrait, ITimerHandler, ITimerHandlerTrait,
};
use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

use crate::block::Change;
use crate::editor::Window;
use crate::runloop::{FdFlags, FdWatches, Timers};
use crate::thread::{MainThread, MainThreadCell};
use crate::wake::Wake;
use crate::{ParamId, PluginEvent};

/// A TUID's bytes, as interface ids (`Interface::IID`) are kept.
fn guid(tuid: TUID) -> [u8; 16] {
    tuid.map(|c| c as u8)
}

/// Copies `src` into a fixed UTF-16 buffer, NUL-terminated, cut to fit.
pub(crate) fn copy_wide(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s;
        len += 1;
    }
    let end = len.min(dst.len().saturating_sub(1));
    if let Some(d) = dst.get_mut(end) {
        *d = 0;
    }
}

/// A fixed UTF-16 buffer as a string, up to its NUL.
pub(crate) fn wide(buf: &[TChar]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// A fixed C string buffer as a string, up to its NUL.
pub(crate) fn narrow(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A NUL-terminated UTF-16 string from a plugin.
///
/// # Safety
///
/// `ptr` must be null or NUL-terminated.
unsafe fn wide_ptr(ptr: *const TChar) -> Vec<u16> {
    let mut out = Vec::new();
    if ptr.is_null() {
        return out;
    }
    // SAFETY: by the caller; stops at the NUL (or after a sane maximum).
    unsafe {
        while *ptr.add(out.len()) != 0 && out.len() < 1 << 16 {
            out.push(*ptr.add(out.len()));
        }
    }
    out
}

/// Timers and file descriptors a plugin's GUI registered through
/// `IRunLoop`, run by [`RunLoop::idle`] on the main thread.
pub(crate) struct RunLoop {
    state: MainThreadCell<RefCell<RunLoopState>>,
}

struct RunLoopState {
    timers: Timers<ComPtr<ITimerHandler>>,
    fds: FdWatches<ComPtr<IEventHandler>>,
}

impl RunLoop {
    pub fn new(main: MainThread) -> RunLoop {
        RunLoop {
            state: MainThreadCell::new(
                main,
                RefCell::new(RunLoopState {
                    timers: Timers::new(),
                    fds: FdWatches::new(),
                }),
            ),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut RunLoopState) -> R) -> Option<R> {
        let mut state = self.state.get()?.try_borrow_mut().ok()?;
        Some(f(&mut state))
    }

    /// Fires due timers and ready descriptors; returns when the next timer
    /// is due. Main thread only.
    pub fn idle(&self) -> Option<Instant> {
        let (due, ready) = self
            .with(|s| (s.timers.due(Instant::now()), s.fds.ready()))
            .unwrap_or_default();
        // The plugin may (un)register from inside these: nothing is
        // borrowed while they run.
        for timer in due {
            // SAFETY: a handler the plugin registered, kept alive by our ref.
            unsafe { timer.onTimer() };
        }
        for (fd, _, handler) in ready {
            // SAFETY: as above.
            unsafe { handler.onFDIsSet(fd) };
        }
        self.with(|s| s.timers.next_due()).flatten()
    }

    /// The descriptors registered, for a thread that waits on them.
    pub fn fds(&self, out: &mut Vec<i32>) {
        let _ = self.with(|s| s.fds.fds(out));
    }

    /// Forgets every registration, from any thread, when nothing else
    /// holds the run loop.
    pub fn clear_owned(&mut self) {
        let state = self.state.get_mut().get_mut();
        state.timers = Timers::new();
        state.fds = FdWatches::new();
    }

    /// Forgets every registration (when the plugin goes away).
    pub fn clear(&self) {
        let _ = self.with(|s| {
            s.timers = Timers::new();
            s.fds = FdWatches::new();
        });
    }

    fn register_event_handler(&self, handler: *mut IEventHandler, fd: i32) -> tresult {
        // SAFETY: the plugin passes a live handler; we keep a reference.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }).map(|h| h.to_com_ptr()) else {
            return kInvalidArgument;
        };
        let read = FdFlags {
            read: true,
            write: false,
            error: true,
        };
        self.with(|s| s.fds.set(fd, read, handler))
            .map_or(kResultFalse, |_| kResultOk)
    }

    fn unregister_event_handler(&self, handler: *mut IEventHandler) -> tresult {
        self.with(|s| s.fds.remove_where(|h| h.as_ptr() == handler))
            .map_or(kResultFalse, |_| kResultOk)
    }

    fn register_timer(&self, handler: *mut ITimerHandler, milliseconds: u64) -> tresult {
        // SAFETY: as in register_event_handler.
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }).map(|h| h.to_com_ptr()) else {
            return kInvalidArgument;
        };
        self.with(|s| s.timers.add(milliseconds, handler))
            .map_or(kResultFalse, |_| kResultOk)
    }

    fn unregister_timer(&self, handler: *mut ITimerHandler) -> tresult {
        self.with(|s| s.timers.remove_where(|h| h.as_ptr() == handler))
            .map_or(kResultFalse, |_| kResultOk)
    }
}

/// `IRunLoop` for whichever object a plugin finds it on.
macro_rules! run_loop_impl {
    ($type:ty) => {
        impl IRunLoopTrait for $type {
            unsafe fn registerEventHandler(&self, handler: *mut IEventHandler, fd: i32) -> tresult {
                self.run_loop.register_event_handler(handler, fd)
            }

            unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
                self.run_loop.unregister_event_handler(handler)
            }

            unsafe fn registerTimer(
                &self,
                handler: *mut ITimerHandler,
                milliseconds: u64,
            ) -> tresult {
                self.run_loop.register_timer(handler, milliseconds)
            }

            unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
                self.run_loop.unregister_timer(handler)
            }
        }
    };
}

/// The host context plugins are initialized with.
pub(crate) struct HostApplication {
    run_loop: Arc<RunLoop>,
}

impl HostApplication {
    pub fn new(run_loop: Arc<RunLoop>) -> ComWrapper<HostApplication> {
        ComWrapper::new(HostApplication { run_loop })
    }
}

#[cfg(target_os = "linux")]
impl Class for HostApplication {
    type Interfaces = (
        IHostApplication,
        IPlugInterfaceSupport,
        vst3::Steinberg::Linux::IRunLoop,
    );
}

#[cfg(not(target_os = "linux"))]
impl Class for HostApplication {
    type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

run_loop_impl!(HostApplication);

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin gives a String128 to fill.
        match unsafe { name.as_mut() } {
            Some(name) => {
                copy_wide("canopy", name);
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; TUIDs are 16 bytes.
        let (cid, iid) = unsafe { (guid(*cid), guid(*iid)) };
        let object = if cid == IMessage::IID {
            HostMessage::new().to_com_ptr::<FUnknown>()
        } else if cid == IAttributeList::IID {
            ComWrapper::new(HostAttributes::default()).to_com_ptr::<FUnknown>()
        } else {
            None
        };
        let Some(object) = object else {
            // SAFETY: as above.
            unsafe { *obj = std::ptr::null_mut() };
            return kResultFalse;
        };
        // SAFETY: hand out the interface asked for (adding a reference).
        unsafe {
            let ptr = object.as_ptr();
            ((*(*ptr).vtbl).queryInterface)(ptr, iid.as_ptr().cast(), obj)
        }
    }
}

impl IPlugInterfaceSupportTrait for HostApplication {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        if iid.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null.
        let iid = guid(unsafe { *iid });
        let supported = [
            IComponent::IID,
            IAudioProcessor::IID,
            IEditController::IID,
            IConnectionPoint::IID,
            IMidiMapping::IID,
            IUnitInfo::IID,
            IPlugView::IID,
            IProcessContextRequirements::IID,
        ];
        if supported.contains(&iid) {
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

/// A controller's edits as they arrive on a thread other than the main one
/// (they shouldn't, but plugins do), replayed by the next idle.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ForeignEdit {
    Begin(u32),
    Perform(u32, f64),
    End(u32),
}

/// What [`ComponentHandler`] shares with the instance.
pub(crate) struct HandlerShared {
    pub main: MainThreadCell<RefCell<HandlerMain>>,
    pub wake: Arc<Wake>,
    /// `RestartFlags`, or-ed, from any thread.
    pub restart: AtomicI32,
    pub dirty: AtomicBool,
    pub foreign: Mutex<Vec<ForeignEdit>>,
}

/// The handler's main-thread state.
pub(crate) struct HandlerMain {
    /// To the processor, while active.
    pub to_audio: Option<rtrb::Producer<Change>>,
    /// Changes made while inactive, for the processor's first block.
    pub pending: Vec<Change>,
    pub events: Vec<PluginEvent>,
}

impl HandlerMain {
    /// Sends a change to the processor, or keeps it for its first block.
    pub fn send(&mut self, change: Change) {
        match &mut self.to_audio {
            Some(queue) => {
                let _ = queue.push(change);
            }
            None => {
                self.pending.retain(|c| c.id != change.id);
                self.pending.push(change);
            }
        }
    }

    /// The controller edited a parameter: the processor needs to hear it.
    pub fn perform(&mut self, id: u32, value: f64) {
        let (id, value) = (ParamId(id), value.clamp(0.0, 1.0));
        self.send(Change { id, value });
        self.events.push(PluginEvent::ParamValue { id, value });
    }
}

impl HandlerShared {
    pub fn new(main: MainThread, wake: Arc<Wake>) -> Arc<HandlerShared> {
        Arc::new(HandlerShared {
            wake,
            main: MainThreadCell::new(
                main,
                RefCell::new(HandlerMain {
                    to_audio: None,
                    pending: Vec::new(),
                    events: Vec::new(),
                }),
            ),
            restart: AtomicI32::new(0),
            dirty: AtomicBool::new(false),
            foreign: Mutex::new(Vec::new()),
        })
    }

    /// Runs `f` on the main-thread state, or records `edit` for later.
    fn edit(&self, edit: ForeignEdit, f: impl FnOnce(&mut HandlerMain)) -> tresult {
        match self.main.get().and_then(|m| m.try_borrow_mut().ok()) {
            Some(mut main) => f(&mut main),
            None => {
                self.foreign
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(edit);
                self.wake.request();
            }
        }
        kResultOk
    }
}

/// The controller's line to the host.
pub(crate) struct ComponentHandler {
    shared: Arc<HandlerShared>,
}

impl ComponentHandler {
    pub fn new(shared: Arc<HandlerShared>) -> ComWrapper<ComponentHandler> {
        ComWrapper::new(ComponentHandler { shared })
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.shared.edit(ForeignEdit::Begin(id), |m| {
            m.events.push(PluginEvent::ParamGesture {
                id: ParamId(id),
                begin: true,
            })
        })
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.shared
            .edit(ForeignEdit::Perform(id, value), |m| m.perform(id, value))
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.shared.edit(ForeignEdit::End(id), |m| {
            m.events.push(PluginEvent::ParamGesture {
                id: ParamId(id),
                begin: false,
            })
        })
    }

    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        self.shared.restart.fetch_or(flags, Ordering::AcqRel);
        self.shared.wake.request();
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0 {
            self.shared.dirty.store(true, Ordering::Release);
            self.shared.wake.request();
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        kResultFalse
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

/// An open editor's window, as its frame sees it.
pub(crate) struct FrameState {
    pub window: Option<Arc<Window>>,
    pub size: (u32, u32),
    pub resizable: bool,
}

/// The frame around an editor view.
pub(crate) struct PlugFrame {
    run_loop: Arc<RunLoop>,
    pub state: MainThreadCell<RefCell<FrameState>>,
}

impl PlugFrame {
    pub fn new(run_loop: Arc<RunLoop>, main: MainThread) -> ComWrapper<PlugFrame> {
        ComWrapper::new(PlugFrame {
            run_loop,
            state: MainThreadCell::new(
                main,
                RefCell::new(FrameState {
                    window: None,
                    size: (0, 0),
                    resizable: false,
                }),
            ),
        })
    }
}

#[cfg(target_os = "linux")]
impl Class for PlugFrame {
    type Interfaces = (IPlugFrame, vst3::Steinberg::Linux::IRunLoop);
}

#[cfg(not(target_os = "linux"))]
impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

run_loop_impl!(PlugFrame);

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, size: *mut ViewRect) -> tresult {
        // SAFETY: the plugin passes its view and the size it wants.
        let (Some(view), Some(rect)) =
            (unsafe { ComRef::from_raw(view) }, unsafe { size.as_ref() })
        else {
            return kInvalidArgument;
        };
        let width = (rect.right - rect.left).max(1) as u32;
        let height = (rect.bottom - rect.top).max(1) as u32;
        {
            let Some(mut state) = self.state.get().and_then(|s| s.try_borrow_mut().ok()) else {
                return kResultFalse;
            };
            state.size = (width, height);
            if let Some(window) = &state.window {
                window.set_size(width, height, state.resizable);
            }
        }
        // The host resizes, then tells the view (VST3's resizing protocol).
        // SAFETY: main-thread call on the view that asked.
        unsafe { view.onSize(size) };
        kResultTrue
    }
}

/// Passes messages between a component and its controller, on the main
/// thread only (as the SDK's own host does).
pub(crate) struct ConnectionProxy {
    peer: ComPtr<IConnectionPoint>,
    main: MainThread,
}

impl ConnectionProxy {
    pub fn new(peer: ComPtr<IConnectionPoint>, main: MainThread) -> ComWrapper<ConnectionProxy> {
        ComWrapper::new(ConnectionProxy { peer, main })
    }
}

impl Class for ConnectionProxy {
    type Interfaces = (IConnectionPoint,);
}

impl IConnectionPointTrait for ConnectionProxy {
    unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        if !self.main.is_current() {
            return kResultFalse;
        }
        // SAFETY: forwarding the plugin's message to the other side.
        unsafe { self.peer.notify(message) }
    }
}

/// A message between a component and its controller.
pub(crate) struct HostMessage {
    id: Mutex<Option<CString>>,
    attributes: ComWrapper<HostAttributes>,
}

impl HostMessage {
    fn new() -> ComWrapper<HostMessage> {
        ComWrapper::new(HostMessage {
            id: Mutex::new(None),
            attributes: ComWrapper::new(HostAttributes::default()),
        })
    }
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        let id = self.id.lock().unwrap_or_else(|e| e.into_inner());
        // The string lives in the message until the ID is set again.
        id.as_ref().map_or(std::ptr::null(), |id| id.as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let id = (!id.is_null()).then(|| {
            // SAFETY: plugins pass NUL-terminated ids.
            unsafe { CStr::from_ptr(id) }.to_owned()
        });
        *self.id.lock().unwrap_or_else(|e| e.into_inner()) = id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Owned by the message; not an extra reference (as in the SDK).
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |a| a.as_ptr())
    }
}

enum Attribute {
    Int(i64),
    Float(f64),
    String(Vec<u16>),
    Binary(Vec<u8>),
}

/// A message's attributes.
#[derive(Default)]
pub(crate) struct HostAttributes {
    values: Mutex<Vec<(CString, Attribute)>>,
}

impl HostAttributes {
    fn set(&self, id: AttrID, value: Attribute) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: attribute ids are NUL-terminated.
        let id = unsafe { CStr::from_ptr(id) }.to_owned();
        let mut values = self.values.lock().unwrap_or_else(|e| e.into_inner());
        values.retain(|(k, _)| *k != id);
        values.push((id, value));
        kResultOk
    }

    fn get<R>(&self, id: AttrID, f: impl FnOnce(&Attribute) -> Option<R>) -> Option<R> {
        if id.is_null() {
            return None;
        }
        // SAFETY: as above.
        let id = unsafe { CStr::from_ptr(id) };
        let values = self.values.lock().unwrap_or_else(|e| e.into_inner());
        values
            .iter()
            .find(|(k, _)| k.as_c_str() == id)
            .and_then(|(_, v)| f(v))
    }
}

impl Class for HostAttributes {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for HostAttributes {
    unsafe fn setInt(&self, id: AttrID, value: i64) -> tresult {
        self.set(id, Attribute::Int(value))
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut i64) -> tresult {
        match self.get(id, |a| match a {
            Attribute::Int(v) => Some(*v),
            _ => None,
        }) {
            // SAFETY: the plugin gives somewhere to write.
            Some(v) if !value.is_null() => unsafe {
                *value = v;
                kResultOk
            },
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        self.set(id, Attribute::Float(value))
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        match self.get(id, |a| match a {
            Attribute::Float(v) => Some(*v),
            _ => None,
        }) {
            // SAFETY: as in getInt.
            Some(v) if !value.is_null() => unsafe {
                *value = v;
                kResultOk
            },
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        // SAFETY: plugins pass NUL-terminated strings.
        self.set(id, Attribute::String(unsafe { wide_ptr(string) }))
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: u32) -> tresult {
        let capacity = size_in_bytes as usize / size_of::<TChar>();
        if string.is_null() || capacity == 0 {
            return kInvalidArgument;
        }
        let copied = self.get(id, |a| match a {
            Attribute::String(s) => {
                let n = s.len().min(capacity - 1);
                // SAFETY: room for `capacity` chars, NUL included.
                unsafe {
                    std::ptr::copy_nonoverlapping(s.as_ptr(), string, n);
                    *string.add(n) = 0;
                }
                Some(())
            }
            _ => None,
        });
        if copied.is_some() {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: u32) -> tresult {
        let bytes = if data.is_null() {
            Vec::new()
        } else {
            // SAFETY: the plugin gives `size_in_bytes` readable bytes.
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size_in_bytes as usize) }
                .to_vec()
        };
        self.set(id, Attribute::Binary(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut u32,
    ) -> tresult {
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        // The bytes stay where they are until the attribute is set again,
        // which is what the SDK's own attribute list promises too.
        let found = self.get(id, |a| match a {
            Attribute::Binary(b) => Some((b.as_ptr(), b.len())),
            _ => None,
        });
        match found {
            // SAFETY: checked non-null.
            Some((ptr, len)) => unsafe {
                *data = ptr.cast();
                *size_in_bytes = len as u32;
                kResultOk
            },
            None => kResultFalse,
        }
    }
}

/// An `IBStream` over bytes in memory: what plugins write their state to
/// and read it from.
pub(crate) struct MemoryStream {
    inner: Mutex<(Vec<u8>, usize)>,
}

impl MemoryStream {
    pub fn new(bytes: Vec<u8>) -> ComWrapper<MemoryStream> {
        ComWrapper::new(MemoryStream {
            inner: Mutex::new((bytes, 0)),
        })
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .clone()
    }

    pub fn rewind(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).1 = 0;
    }
}

impl Class for MemoryStream {
    type Interfaces = (IBStream, ISizeableStream);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, count: i32, read: *mut i32) -> tresult {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, pos) = &mut *inner;
        let n = (count.max(0) as usize).min(bytes.len().saturating_sub(*pos));
        if n > 0 && buffer.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin gives room for `count` ≥ n bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes[*pos..].as_ptr(), buffer.cast::<u8>(), n);
            if !read.is_null() {
                *read = n as i32;
            }
        }
        *pos += n;
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, count: i32, written: *mut i32) -> tresult {
        let n = count.max(0) as usize;
        if n > 0 && buffer.is_null() {
            return kInvalidArgument;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, pos) = &mut *inner;
        if bytes.len() < *pos + n {
            bytes.resize(*pos + n, 0);
        }
        // SAFETY: the plugin gives `count` readable bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(buffer.cast::<u8>(), bytes[*pos..].as_mut_ptr(), n);
            if !written.is_null() {
                *written = n as i32;
            }
        }
        *pos += n;
        kResultOk
    }

    // The seek modes' type is u32 here and i32 on Windows.
    #[allow(clippy::unnecessary_cast)]
    unsafe fn seek(&self, offset: i64, mode: i32, result: *mut i64) -> tresult {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, pos) = &mut *inner;
        let base = match mode as u32 {
            m if m == IBStream_::IStreamSeekMode_::kIBSeekSet as u32 => 0,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekCur as u32 => *pos as i64,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekEnd as u32 => bytes.len() as i64,
            _ => return kInvalidArgument,
        };
        let target = base.saturating_add(offset);
        if target < 0 {
            return kInvalidArgument;
        }
        *pos = target as usize;
        if !result.is_null() {
            // SAFETY: checked non-null.
            unsafe { *result = target };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: checked non-null.
        unsafe { *pos = inner.1 as i64 };
        kResultOk
    }
}

impl ISizeableStreamTrait for MemoryStream {
    unsafe fn getStreamSize(&self, size: *mut i64) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: checked non-null.
        unsafe { *size = inner.0.len() as i64 };
        kResultOk
    }

    unsafe fn setStreamSize(&self, size: i64) -> tresult {
        if size < 0 {
            return kInvalidArgument;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.0.resize(size as usize, 0);
        inner.1 = inner.1.min(size as usize);
        kResultOk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unnecessary_cast)] // the seek mode's type differs by OS
    fn memory_stream_reads_what_was_written() {
        let stream = MemoryStream::new(Vec::new());
        let ptr = stream.to_com_ptr::<IBStream>().unwrap();
        let mut data = *b"hello";
        let mut n = 0;
        // SAFETY: valid buffers and out-pointers.
        unsafe {
            assert_eq!(ptr.write(data.as_mut_ptr().cast(), 5, &mut n), kResultOk);
            assert_eq!(n, 5);
            let mut at = 0;
            ptr.seek(1, IBStream_::IStreamSeekMode_::kIBSeekSet as i32, &mut at);
            assert_eq!(at, 1);
            let mut out = [0u8; 8];
            ptr.read(out.as_mut_ptr().cast(), 8, &mut n);
            assert_eq!((n, &out[..4]), (4, &b"ello"[..]));
        }
        assert_eq!(stream.bytes(), b"hello");
    }

    #[test]
    fn wide_strings_fit_their_buffers() {
        let mut buf = [0u16; 4];
        copy_wide("canopy", &mut buf);
        assert_eq!(wide(&buf), "can");
        copy_wide("ok", &mut buf);
        assert_eq!(wide(&buf), "ok");
    }
}
