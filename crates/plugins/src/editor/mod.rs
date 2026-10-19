//! Native windows for plugin editors.
//!
//! Each editor gets its own top-level window that the plugin embeds its GUI
//! into: an X11 window on Linux (CLAP's "x11" API, VST3's
//! `X11EmbedWindowID`, VST2's window id), a Win32 one on Windows (CLAP's
//! "win32", VST3's and VST2's `HWND`). Plugins on Linux only embed into X11 windows, and canopy's own
//! window may be a Wayland one, so editors never go inside the app's window;
//! as separate windows they work under XWayland too.
//!
//! The window reports the user resizing or closing it, polled from
//! [`crate::Host::idle`]; the plugin's GUI runs from the timers and file
//! descriptors it registered (`crate::runloop`), or for VST2, which has
//! neither, from `effEditIdle` calls the host makes about 30 times a
//! second.
//!
//! Without the `editor` feature there are no windows, and plugins say they
//! have no editor.

use std::ffi::CStr;

#[cfg(all(feature = "editor", unix))]
mod x11;
#[cfg(all(feature = "editor", unix))]
pub(crate) use x11::Window;

#[cfg(all(feature = "editor", windows))]
mod win32;
#[cfg(all(feature = "editor", windows))]
pub(crate) use win32::Window;
#[cfg(all(feature = "editor", windows))]
pub use win32::pump_messages;

/// Dispatches this thread's pending window messages, for programs that
/// have no event loop of their own (like the example CLI) while an editor
/// is open. On Windows plugin GUIs live on the thread's message queue; in
/// an app, its event loop (winit's) already does this. Elsewhere it does
/// nothing.
#[cfg(not(all(feature = "editor", windows)))]
pub fn pump_messages() {}

/// How to open a plugin's editor window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorOptions {
    /// The window's title, e.g. "Bass — Serum".
    pub title: String,
    /// Ask the window manager to keep it above other windows. Editors
    /// can't be made transient for canopy's own window when that's a
    /// Wayland one, so this is how they stay in sight.
    pub keep_on_top: bool,
}

impl EditorOptions {
    pub fn titled(title: impl Into<String>) -> EditorOptions {
        EditorOptions {
            title: title.into(),
            keep_on_top: false,
        }
    }
}

/// What a plugin embeds its GUI into.
#[cfg_attr(not(feature = "editor"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) enum Parent {
    /// An X11 window id.
    #[cfg(unix)]
    X11(u32),
    /// An `HWND`.
    #[cfg(windows)]
    Win32(*mut std::ffi::c_void),
}

impl Parent {
    /// The handle as VST3's `IPlugView::attached` and VST2's `effEditOpen`
    /// take it.
    pub fn as_ptr(self) -> *mut std::ffi::c_void {
        match self {
            #[cfg(unix)]
            Parent::X11(id) => id as usize as *mut std::ffi::c_void,
            #[cfg(windows)]
            Parent::Win32(hwnd) => hwnd,
        }
    }
}

/// Something the user did to an editor window.
#[cfg_attr(not(feature = "editor"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowEvent {
    Resized(u32, u32),
    CloseRequested,
}

/// The window API CLAP plugins are asked for.
pub(crate) const CLAP_API: Option<&CStr> = if cfg!(not(feature = "editor")) {
    None
} else if cfg!(windows) {
    Some(c"win32")
} else if cfg!(target_os = "linux") {
    Some(c"x11")
} else {
    None
};

/// The platform type VST3 plugins are asked for.
pub(crate) const VST3_PLATFORM: Option<&CStr> = if cfg!(not(feature = "editor")) {
    None
} else if cfg!(windows) {
    Some(c"HWND")
} else if cfg!(target_os = "linux") {
    Some(c"X11EmbedWindowID")
} else {
    None
};

/// Whether VST2 editors can be shown: they embed into an X11 window on
/// Linux and an `HWND` on Windows.
pub(crate) const VST2_EDITOR: bool =
    cfg!(feature = "editor") && (cfg!(windows) || cfg!(target_os = "linux"));

/// Without the `editor` feature there are no windows to open.
#[cfg(not(feature = "editor"))]
pub(crate) enum Window {}

#[cfg(not(feature = "editor"))]
impl Window {
    pub fn open(
        _options: &EditorOptions,
        _width: u32,
        _height: u32,
        _resizable: bool,
    ) -> Result<Window, crate::Error> {
        Err(crate::Error::Editor(
            "built without the editor feature".into(),
        ))
    }

    pub fn set_keep_on_top(&self, _on_top: bool) {
        match *self {}
    }

    pub fn fd(&self) -> Option<i32> {
        match *self {}
    }

    pub fn parent(&self) -> Parent {
        match *self {}
    }

    pub fn set_size(&self, _width: u32, _height: u32, _resizable: bool) {
        match *self {}
    }

    pub fn show(&self, _visible: bool) {
        match *self {}
    }

    pub fn poll(&self) -> Vec<WindowEvent> {
        match *self {}
    }
}
