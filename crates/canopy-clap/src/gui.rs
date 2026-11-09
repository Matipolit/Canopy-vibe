//! Native embedded CLAP editor windows. GUI calls stay on the host's main
//! thread; X11 uses baseview's host-callback bridge rather than a DAW event loop.

use crate::{
    device::Display,
    editor::{Edits, ShaperEditor},
    values::Values,
};
use clap_sys::{
    ext::{
        gui::*,
        params::{CLAP_EXT_PARAMS, clap_host_params},
        state::{CLAP_EXT_STATE, clap_host_state},
    },
    host::clap_host,
};
use egui_baseview::{
    EguiWindow, EguiWindowSettings,
    baseview::{
        self,
        dpi::{LogicalSize, PhysicalSize},
    },
};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use std::{
    ffi::CStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const WIDTH: u32 = 1000;
pub const HEIGHT: u32 = 620;

/// CLAP keeps the host alive until plugin destruction. Only request_callback
/// is called off the main thread, as permitted by CLAP. Closing the native
/// window joins/removes its callbacks before the plugin can be destroyed.
#[derive(Clone, Copy)]
pub(crate) struct HostHandle(pub usize);
impl HostHandle {
    fn pointer(self) -> *const clap_host {
        self.0 as *const clap_host
    }
    pub fn request_main(self) {
        if let Some(host) = unsafe { self.pointer().as_ref() }
            && let Some(call) = host.request_callback
        {
            unsafe { call(self.pointer()) };
        }
    }
    fn extension<T>(&self, id: &CStr) -> Option<&T> {
        let host = unsafe { self.pointer().as_ref() }?;
        let ptr = unsafe { host.get_extension?(self.pointer(), id.as_ptr()) };
        unsafe { (ptr as *const T).as_ref() }
    }
    pub fn flush(self, edits: &Edits) {
        if edits.pending()
            && let Some(ext) = self.extension::<clap_host_params>(CLAP_EXT_PARAMS)
            && let Some(flush) = ext.request_flush
        {
            unsafe { flush(self.pointer()) };
        }
        if edits.dirty.swap(false, Ordering::AcqRel)
            && let Some(ext) = self.extension::<clap_host_state>(CLAP_EXT_STATE)
            && let Some(dirty) = ext.mark_dirty
        {
            unsafe { dirty(self.pointer()) };
        }
    }
}
impl baseview::host::HostMainThreadCaller for HostHandle {
    fn call_main_thread(&mut self) {
        self.request_main();
    }
}

struct Callbacks {
    host: HostHandle,
    closing: Arc<AtomicBool>,
    display: Arc<Display>,
}
impl baseview::host::HostCallbacks for Callbacks {
    fn request_resize(&mut self, size: baseview::WindowSize) -> Result<(), baseview::HandlerError> {
        if let Some(ext) = self.host.extension::<clap_host_gui>(CLAP_EXT_GUI)
            && let Some(resize) = ext.request_resize
            && unsafe {
                resize(
                    self.host.pointer(),
                    size.physical.width,
                    size.physical.height,
                )
            }
        {
            return Ok(());
        }
        Err(std::io::Error::other("host rejected editor resize").into())
    }
    fn destroyed(&mut self) {
        self.display.visible.store(false, Ordering::Relaxed);
        if !self.closing.load(Ordering::Relaxed)
            && let Some(ext) = self.host.extension::<clap_host_gui>(CLAP_EXT_GUI)
            && let Some(closed) = ext.closed
        {
            unsafe { closed(self.host.pointer(), true) };
        }
    }
}

struct App {
    editor: ShaperEditor,
    host: HostHandle,
}
impl egui_baseview::App for App {
    fn build(
        &mut self,
        ctx: egui::Context,
        _: &mut egui_baseview::Frame,
    ) -> Result<(), baseview::HandlerError> {
        canopy_widgets::theme::apply(&ctx);
        Ok(())
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut egui_baseview::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            if self.editor.show(ui) {
                self.host.request_main();
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.editor.edits.finish_all();
    }
}

pub(crate) struct NativeGui {
    window: baseview::Window,
    parented: std::cell::Cell<bool>,
    closing: Arc<AtomicBool>,
    display: Arc<Display>,
    edits: Arc<Edits>,
    host: HostHandle,
}
impl NativeGui {
    pub fn create(
        host: HostHandle,
        values: Arc<Values>,
        display: Arc<Display>,
        edits: Arc<Edits>,
    ) -> Option<Self> {
        let closing = Arc::new(AtomicBool::new(false));
        let callbacks = Callbacks {
            host,
            closing: closing.clone(),
            display: display.clone(),
        };
        let app = App {
            editor: ShaperEditor::new(values, display.clone(), edits.clone()),
            host,
        };
        let settings = EguiWindowSettings::new()
            .with_title("Canopy Shaper")
            .with_size(LogicalSize::new(f64::from(WIDTH), f64::from(HEIGHT)))
            .with_min_size(Some(LogicalSize::new(760.0, 480.0)))
            .with_max_size(Some(LogicalSize::new(2000.0, 1400.0)))
            .with_resizable(true)
            .wait_for_parent();
        let window = EguiWindow::create_with_host(
            settings,
            app,
            Some(
                baseview::host::Host::new()
                    .with_main_thread(host)
                    .with_callbacks(callbacks),
            ),
        )
        .ok()?;
        Some(Self {
            window,
            parented: std::cell::Cell::new(false),
            closing,
            display,
            edits,
            host,
        })
    }
    pub fn pump(&self) {
        self.window.host_main_thread_callback();
    }
    pub fn size(&self) -> (u32, u32) {
        let s = self.window.size();
        (s.physical.width, s.physical.height)
    }
    pub fn adjust_size(&self, width: u32, height: u32) -> (u32, u32) {
        let scale = self.window.size().scale_factor;
        let px = |logical: f64| (logical * scale).round() as u32;
        (
            width.clamp(px(760.0), px(2000.0)),
            height.clamp(px(480.0), px(1400.0)),
        )
    }
    pub fn resize(&self, width: u32, height: u32) -> bool {
        self.window.resize(PhysicalSize::new(width, height)).is_ok()
    }
    pub fn show(&self) -> bool {
        if !self.parented.get() {
            return false;
        }
        let shown = self.window.show().is_ok();
        self.display.visible.store(shown, Ordering::Relaxed);
        shown
    }
    pub fn hide(&self) -> bool {
        self.display.visible.store(false, Ordering::Relaxed);
        self.edits.finish_all();
        self.host.request_main();
        self.window.hide().is_ok()
    }
    /// # Safety
    /// `parent` names a valid native host window that outlives the editor.
    pub unsafe fn parent(&self, parent: &clap_window) -> bool {
        let Some(raw) = (unsafe { raw_parent(parent) }) else {
            return false;
        };
        struct Parent(RawWindowHandle);
        impl HasWindowHandle for Parent {
            fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
                Ok(unsafe { WindowHandle::borrow_raw(self.0) })
            }
        }
        let parent = baseview::ParentWindowHandle::from_window(&Parent(raw));
        let result = self.window.set_parent(parent).is_ok();
        self.parented.set(result);
        result
    }
}
impl Drop for NativeGui {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Relaxed);
        self.display.visible.store(false, Ordering::Relaxed);
        self.edits.finish_all();
        // baseview's Window drop closes it and guarantees callbacks are gone.
    }
}

pub fn native_api() -> &'static CStr {
    #[cfg(target_os = "linux")]
    {
        CLAP_WINDOW_API_X11
    }
    #[cfg(target_os = "windows")]
    {
        CLAP_WINDOW_API_WIN32
    }
    #[cfg(target_os = "macos")]
    {
        CLAP_WINDOW_API_COCOA
    }
}
unsafe fn raw_parent(window: &clap_window) -> Option<RawWindowHandle> {
    if window.api.is_null() || unsafe { CStr::from_ptr(window.api) } != native_api() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let id = unsafe { window.specific.x11 };
        (id != 0).then(|| raw_window_handle::XlibWindowHandle::new(id).into())
    }
    #[cfg(target_os = "windows")]
    {
        let id = std::num::NonZeroIsize::new(unsafe { window.specific.win32 } as isize)?;
        Some(raw_window_handle::Win32WindowHandle::new(id).into())
    }
    #[cfg(target_os = "macos")]
    {
        let id = std::ptr::NonNull::new(unsafe { window.specific.cocoa })?;
        Some(raw_window_handle::AppKitWindowHandle::new(id).into())
    }
}
