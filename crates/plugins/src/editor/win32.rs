//! Editor windows on Windows: a plain top-level window whose client area
//! the plugin fills with its own child window.
//!
//! Window messages reach the window procedure from whatever pumps the
//! thread's message queue: the app's event loop (winit), or
//! [`pump_messages`] in programs without one. The procedure only records
//! what happened; [`Window::poll`] hands it over.

use std::sync::Mutex;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{EditorOptions, Parent, WindowEvent};
use crate::Error;

/// What the window procedure records for [`Window::poll`].
struct State {
    events: Mutex<Vec<WindowEvent>>,
}

/// A top-level window for one plugin editor.
pub(crate) struct Window {
    hwnd: HWND,
    state: Box<State>,
}

// SAFETY: an HWND is a handle, not memory; we only call window functions
// on the thread that made the window (instances are main-thread only, and
// the VST3 frame reaches the window through a MainThreadCell).
unsafe impl Send for Window {}
unsafe impl Sync for Window {}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn class_name() -> &'static [u16] {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| wide("canopy-plugin-editor"))
}

/// Registers the window class once per process.
fn register_class() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        // SAFETY: a fully initialized class description whose strings are
        // 'static.
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name().as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&class);
        }
    });
}

fn style(resizable: bool) -> WINDOW_STYLE {
    if resizable {
        WS_OVERLAPPEDWINDOW
    } else {
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
    }
}

/// The outer size of a window whose client area is `width` × `height`.
fn outer_size(width: u32, height: u32, resizable: bool) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    // SAFETY: a valid RECT to adjust in place.
    unsafe { AdjustWindowRectEx(&mut rect, style(resizable), 0, 0) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

impl Window {
    pub fn open(
        options: &EditorOptions,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Window, Error> {
        register_class();
        let state = Box::new(State {
            events: Mutex::new(Vec::new()),
        });
        let title = wide(&options.title);
        let (outer_width, outer_height) = outer_size(width, height, resizable);
        // SAFETY: the class is registered; strings are NUL-terminated and
        // outlive the call. The state pointer stays valid until Drop
        // clears it, before the box is freed.
        let hwnd = unsafe {
            let hwnd = CreateWindowExW(
                0,
                class_name().as_ptr(),
                title.as_ptr(),
                style(resizable),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                outer_width,
                outer_height,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return Err(Error::Editor("CreateWindowExW failed".into()));
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const State as isize);
            ShowWindow(hwnd, SW_SHOW);
            hwnd
        };
        let window = Window { hwnd, state };
        if options.keep_on_top {
            window.set_keep_on_top(true);
        }
        Ok(window)
    }

    pub fn parent(&self) -> Parent {
        Parent::Win32(self.hwnd)
    }

    pub fn set_size(&self, width: u32, height: u32, resizable: bool) {
        let (outer_width, outer_height) = outer_size(width, height, resizable);
        // SAFETY: our window, on its own thread.
        unsafe {
            let visible = GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32 & WS_VISIBLE;
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, (style(resizable) | visible) as isize);
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                outer_width,
                outer_height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    pub fn set_keep_on_top(&self, on_top: bool) {
        let after = if on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
        // SAFETY: our window, on its own thread.
        unsafe {
            SetWindowPos(
                self.hwnd,
                after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// Window messages don't come through a file descriptor on Windows.
    pub fn fd(&self) -> Option<i32> {
        None
    }

    pub fn show(&self, visible: bool) {
        // SAFETY: our window, on its own thread.
        unsafe { ShowWindow(self.hwnd, if visible { SW_SHOW } else { SW_HIDE }) };
    }

    pub fn poll(&self) -> Vec<WindowEvent> {
        std::mem::take(&mut *self.state.events.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: detach the state before destroying, so no message reaches
        // a freed box.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: GWLP_USERDATA is 0 or our live State (see `open` and Drop).
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State).as_ref() };
    if let Some(state) = state {
        let mut events = state.events.lock().unwrap_or_else(|e| e.into_inner());
        match message {
            WM_CLOSE => {
                events.push(WindowEvent::CloseRequested);
                return 0;
            }
            WM_SIZE if wparam as u32 != SIZE_MINIMIZED => {
                let width = (lparam as u32) & 0xFFFF;
                let height = ((lparam as u32) >> 16) & 0xFFFF;
                events.push(WindowEvent::Resized(width, height));
            }
            _ => {}
        }
    }
    // SAFETY: the default handling for everything else.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Dispatches this thread's pending window messages (see
/// [`super::pump_messages`]).
pub fn pump_messages() {
    // SAFETY: the standard message loop, without blocking.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}
