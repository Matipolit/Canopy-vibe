//! A tiny editor: an `IPlugView` that puts an X11 window inside the host's
//! and runs only from the timer and event handler it registers with the
//! host's `IRunLoop` (found on the frame), as Linux VST3 GUIs do. On its
//! first timer tick it asks the frame for 400 × 250, and it watches the
//! host's window to see that happen. What it saw, the controller reports
//! as read-only parameters (raw numbers, not normalized).

use std::ffi::{CStr, c_void};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vst3::Steinberg::Linux::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// What the editor saw, shared with the controller.
#[derive(Default)]
pub struct Counters {
    pub ticks: AtomicU64,
    pub fd_events: AtomicU64,
    pub parent_width: AtomicU64,
    pub open: AtomicBool,
}

#[derive(Default)]
struct State {
    conn: Option<RustConnection>,
    window: Option<u32>,
    parent: u32,
    size: (i32, i32),
    frame: Option<ComPtr<IPlugFrame>>,
    run_loop: Option<ComPtr<IRunLoop>>,
    timer: Option<ComPtr<ITimerHandler>>,
    handler: Option<ComPtr<IEventHandler>>,
    asked_to_resize: bool,
}

struct Shared {
    counters: Arc<Counters>,
    state: Mutex<State>,
    /// The view's own IPlugView pointer (not a reference), for resizeView.
    view: AtomicUsize,
}

pub struct View {
    shared: Arc<Shared>,
}

impl Class for View {
    type Interfaces = (IPlugView,);
}

/// A new view, as `IEditController::createView` returns it.
pub fn create(counters: Arc<Counters>) -> *mut IPlugView {
    let shared = Arc::new(Shared {
        counters,
        state: Mutex::new(State {
            size: (300, 200),
            ..State::default()
        }),
        view: AtomicUsize::new(0),
    });
    let view = ComWrapper::new(View {
        shared: shared.clone(),
    });
    let Some(ptr) = view.to_com_ptr::<IPlugView>() else {
        return std::ptr::null_mut();
    };
    shared.view.store(ptr.as_ptr() as usize, Ordering::Relaxed);
    ptr.into_raw()
}

struct Timer(Arc<Shared>);

impl Class for Timer {
    type Interfaces = (ITimerHandler,);
}

impl ITimerHandlerTrait for Timer {
    unsafe fn onTimer(&self) {
        let shared = &self.0;
        shared.counters.ticks.fetch_add(1, Ordering::Relaxed);
        let frame = {
            let mut state = shared.state.lock().unwrap();
            if std::mem::replace(&mut state.asked_to_resize, true) {
                return;
            }
            state.frame.clone()
        };
        // Outside the lock: the host answers with onSize.
        let view = shared.view.load(Ordering::Relaxed) as *mut IPlugView;
        if let Some(frame) = frame {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 400,
                bottom: 250,
            };
            unsafe { frame.resizeView(view, &mut rect) };
        }
    }
}

struct Watcher(Arc<Shared>);

impl Class for Watcher {
    type Interfaces = (IEventHandler,);
}

impl IEventHandlerTrait for Watcher {
    unsafe fn onFDIsSet(&self, _fd: i32) {
        let shared = &self.0;
        let state = shared.state.lock().unwrap();
        let Some(conn) = &state.conn else { return };
        while let Ok(Some(event)) = conn.poll_for_event() {
            shared.counters.fd_events.fetch_add(1, Ordering::Relaxed);
            if let Event::ConfigureNotify(e) = event
                && e.window == state.parent
            {
                shared
                    .counters
                    .parent_width
                    .store(u64::from(e.width), Ordering::Relaxed);
            }
        }
    }
}

impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, kind: FIDString) -> tresult {
        if !kind.is_null() && unsafe { CStr::from_ptr(kind) } == c"X11EmbedWindowID" {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn attached(&self, parent: *mut c_void, kind: FIDString) -> tresult {
        if unsafe { self.isPlatformTypeSupported(kind) } != kResultTrue {
            return kResultFalse;
        }
        let Ok((conn, screen)) = x11rb::connect(None) else {
            return kResultFalse;
        };
        let parent = parent as usize as u32;
        let (run_loop, fd) = {
            let mut state = self.shared.state.lock().unwrap();
            let Ok(window) = conn.generate_id() else {
                return kResultFalse;
            };
            let screen = &conn.setup().roots[screen];
            let aux = CreateWindowAux::new()
                .background_pixel(screen.white_pixel)
                .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY);
            let (width, height) = state.size;
            if conn
                .create_window(
                    x11rb::COPY_DEPTH_FROM_PARENT,
                    window,
                    parent,
                    0,
                    0,
                    width as u16,
                    height as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    x11rb::COPY_FROM_PARENT,
                    &aux,
                )
                .is_err()
            {
                return kResultFalse;
            }
            let watch = ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY);
            let _ = conn.change_window_attributes(parent, &watch);
            let _ = conn.map_window(window);
            let _ = conn.flush();
            let fd = conn.stream().as_raw_fd();
            state.conn = Some(conn);
            state.window = Some(window);
            state.parent = parent;
            let run_loop = state.frame.as_ref().and_then(|f| f.cast::<IRunLoop>());
            state.run_loop = run_loop.clone();
            (run_loop, fd)
        };
        // Outside the lock: the host may call back.
        if let Some(run_loop) = run_loop {
            let timer = ComWrapper::new(Timer(self.shared.clone()))
                .to_com_ptr::<ITimerHandler>()
                .unwrap();
            let watcher = ComWrapper::new(Watcher(self.shared.clone()))
                .to_com_ptr::<IEventHandler>()
                .unwrap();
            unsafe {
                run_loop.registerTimer(timer.as_ptr(), 16);
                run_loop.registerEventHandler(watcher.as_ptr(), fd);
            }
            let mut state = self.shared.state.lock().unwrap();
            state.timer = Some(timer);
            state.handler = Some(watcher);
        }
        self.shared.counters.open.store(true, Ordering::Relaxed);
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        let (run_loop, timer, handler, conn, window) = {
            let mut state = self.shared.state.lock().unwrap();
            (
                state.run_loop.take(),
                state.timer.take(),
                state.handler.take(),
                state.conn.take(),
                state.window.take(),
            )
        };
        if let Some(run_loop) = run_loop {
            unsafe {
                if let Some(timer) = &timer {
                    run_loop.unregisterTimer(timer.as_ptr());
                }
                if let Some(handler) = &handler {
                    run_loop.unregisterEventHandler(handler.as_ptr());
                }
            }
        }
        if let (Some(conn), Some(window)) = (conn, window) {
            let _ = conn.destroy_window(window);
            let _ = conn.flush();
        }
        self.shared.counters.open.store(false, Ordering::Relaxed);
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        let (width, height) = self.shared.state.lock().unwrap().size;
        unsafe {
            *size = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
        }
        kResultOk
    }

    unsafe fn onSize(&self, size: *mut ViewRect) -> tresult {
        let rect = unsafe { *size };
        let mut state = self.shared.state.lock().unwrap();
        state.size = (rect.right - rect.left, rect.bottom - rect.top);
        if let (Some(conn), Some(window)) = (&state.conn, state.window) {
            let aux = ConfigureWindowAux::new()
                .width(state.size.0 as u32)
                .height(state.size.1 as u32);
            let _ = conn.configure_window(window, &aux);
            let _ = conn.flush();
        }
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        self.shared.state.lock().unwrap().frame =
            unsafe { ComRef::from_raw(frame) }.map(|f| f.to_com_ptr());
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        let rect = unsafe { &mut *rect };
        rect.right = rect.right.max(rect.left + 100);
        rect.bottom = rect.bottom.max(rect.top + 100);
        kResultOk
    }
}
