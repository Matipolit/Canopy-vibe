//! A tiny editor: an X11 window inside the host's, which runs only from the
//! host's timer and file-descriptor callbacks (timer-support and
//! posix-fd-support), as Linux plugin GUIs do. On its first timer tick it
//! asks the host to resize it to 400 × 250, and it watches the host's
//! window to see that happen. What it saw goes in read-only parameters.

use std::ffi::{CStr, c_char};
use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;

use clap_sys::ext::gui::*;
use clap_sys::ext::posix_fd_support::*;
use clap_sys::ext::timer_support::*;
use clap_sys::id::clap_id;
use clap_sys::plugin::clap_plugin;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::Plugin;

pub struct Gui {
    conn: RustConnection,
    screen: usize,
    window: Option<u32>,
    parent: u32,
    size: (u32, u32),
    timer: Option<clap_id>,
    fd: Option<i32>,
    asked_to_resize: bool,
}

pub static GUI_EXT: clap_plugin_gui = clap_plugin_gui {
    is_api_supported: Some(is_api_supported),
    get_preferred_api: Some(get_preferred_api),
    create: Some(create),
    destroy: Some(destroy),
    set_scale: Some(set_scale),
    get_size: Some(get_size),
    can_resize: Some(can_resize),
    get_resize_hints: Some(get_resize_hints),
    adjust_size: Some(adjust_size),
    set_size: Some(set_size),
    set_parent: Some(set_parent),
    set_transient: Some(set_transient),
    suggest_title: Some(suggest_title),
    show: Some(show),
    hide: Some(hide),
};

pub static TIMER_EXT: clap_plugin_timer_support = clap_plugin_timer_support {
    on_timer: Some(on_timer),
};

pub static FD_EXT: clap_plugin_posix_fd_support =
    clap_plugin_posix_fd_support { on_fd: Some(on_fd) };

unsafe extern "C" fn is_api_supported(
    _plugin: *const clap_plugin,
    api: *const c_char,
    floating: bool,
) -> bool {
    !floating && unsafe { CStr::from_ptr(api) } == CLAP_WINDOW_API_X11
}

unsafe extern "C" fn get_preferred_api(
    _plugin: *const clap_plugin,
    api: *mut *const c_char,
    floating: *mut bool,
) -> bool {
    unsafe {
        *api = CLAP_WINDOW_API_X11.as_ptr();
        *floating = false;
    }
    true
}

unsafe extern "C" fn create(
    plugin: *const clap_plugin,
    api: *const c_char,
    floating: bool,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    if floating || unsafe { CStr::from_ptr(api) } != CLAP_WINDOW_API_X11 {
        return false;
    }
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return false;
    };
    *p.gui.lock().unwrap() = Some(Gui {
        conn,
        screen,
        window: None,
        parent: 0,
        size: (300, 200),
        timer: None,
        fd: None,
        asked_to_resize: false,
    });
    true
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let Some(gui) = p.gui.lock().unwrap().take() else {
        return;
    };
    unsafe {
        if let (Some(timer), Some(support)) = (
            gui.timer,
            p.host_ext::<clap_host_timer_support>(CLAP_EXT_TIMER_SUPPORT),
        ) && let Some(unregister) = support.unregister_timer
        {
            unregister(p.host, timer);
        }
        if let (Some(fd), Some(support)) = (
            gui.fd,
            p.host_ext::<clap_host_posix_fd_support>(CLAP_EXT_POSIX_FD_SUPPORT),
        ) && let Some(unregister) = support.unregister_fd
        {
            unregister(p.host, fd);
        }
    }
    if let Some(window) = gui.window {
        let _ = gui.conn.destroy_window(window);
        let _ = gui.conn.flush();
    }
    p.gui_open.store(false, Ordering::Relaxed);
}

unsafe extern "C" fn set_scale(_plugin: *const clap_plugin, _scale: f64) -> bool {
    false
}

unsafe extern "C" fn get_size(
    plugin: *const clap_plugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    let gui = p.gui.lock().unwrap();
    let Some(gui) = gui.as_ref() else {
        return false;
    };
    unsafe {
        *width = gui.size.0;
        *height = gui.size.1;
    }
    true
}

unsafe extern "C" fn can_resize(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn get_resize_hints(
    _plugin: *const clap_plugin,
    hints: *mut clap_gui_resize_hints,
) -> bool {
    unsafe {
        *hints = clap_gui_resize_hints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            preserve_aspect_ratio: false,
            aspect_ratio_width: 0,
            aspect_ratio_height: 0,
        };
    }
    true
}

unsafe extern "C" fn adjust_size(
    _plugin: *const clap_plugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    unsafe {
        *width = (*width).max(100);
        *height = (*height).max(100);
    }
    true
}

fn resize(gui: &mut Gui, width: u32, height: u32) {
    gui.size = (width, height);
    if let Some(window) = gui.window {
        let aux = ConfigureWindowAux::new().width(width).height(height);
        let _ = gui.conn.configure_window(window, &aux);
        let _ = gui.conn.flush();
    }
}

unsafe extern "C" fn set_size(plugin: *const clap_plugin, width: u32, height: u32) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    match p.gui.lock().unwrap().as_mut() {
        Some(gui) => {
            resize(gui, width, height);
            true
        }
        None => false,
    }
}

unsafe extern "C" fn set_parent(plugin: *const clap_plugin, window: *const clap_window) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let parent = unsafe { (*window).specific.x11 } as u32;
    let fd = {
        let mut slot = p.gui.lock().unwrap();
        let Some(gui) = slot.as_mut() else {
            return false;
        };
        let Ok(id) = gui.conn.generate_id() else {
            return false;
        };
        let screen = &gui.conn.setup().roots[gui.screen];
        let aux = CreateWindowAux::new()
            .background_pixel(screen.white_pixel)
            .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY);
        let (width, height) = gui.size;
        let created = gui.conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            id,
            parent,
            0,
            0,
            width as u16,
            height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        );
        if created.is_err() {
            return false;
        }
        // Watch the host's window too, to see it resized.
        let watch = ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY);
        let _ = gui.conn.change_window_attributes(parent, &watch);
        let _ = gui.conn.flush();
        gui.window = Some(id);
        gui.parent = parent;
        gui.conn.stream().as_raw_fd()
    };
    // Outside the lock: the host may call back.
    let mut timer = 0;
    let timer = unsafe {
        p.host_ext::<clap_host_timer_support>(CLAP_EXT_TIMER_SUPPORT)
            .and_then(|s| s.register_timer)
            .is_some_and(|register| register(p.host, 16, &mut timer))
            .then_some(timer)
    };
    let fd_registered = unsafe {
        p.host_ext::<clap_host_posix_fd_support>(CLAP_EXT_POSIX_FD_SUPPORT)
            .and_then(|s| s.register_fd)
            .is_some_and(|register| register(p.host, fd, CLAP_POSIX_FD_READ))
    };
    if let Some(gui) = p.gui.lock().unwrap().as_mut() {
        gui.timer = timer;
        gui.fd = fd_registered.then_some(fd);
    }
    p.gui_open.store(true, Ordering::Relaxed);
    true
}

unsafe extern "C" fn set_transient(
    _plugin: *const clap_plugin,
    _window: *const clap_window,
) -> bool {
    false
}

unsafe extern "C" fn suggest_title(_plugin: *const clap_plugin, _title: *const c_char) {}

fn map(plugin: *const clap_plugin, visible: bool) -> bool {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let gui = p.gui.lock().unwrap();
    let Some(gui) = gui.as_ref() else {
        return false;
    };
    let Some(window) = gui.window else {
        return false;
    };
    let _ = if visible {
        gui.conn.map_window(window)
    } else {
        gui.conn.unmap_window(window)
    };
    let _ = gui.conn.flush();
    true
}

unsafe extern "C" fn show(plugin: *const clap_plugin) -> bool {
    map(plugin, true)
}

unsafe extern "C" fn hide(plugin: *const clap_plugin) -> bool {
    map(plugin, false)
}

unsafe extern "C" fn on_timer(plugin: *const clap_plugin, _timer: clap_id) {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    p.gui_ticks.fetch_add(1, Ordering::Relaxed);
    let ask = p
        .gui
        .lock()
        .unwrap()
        .as_mut()
        .is_some_and(|gui| !std::mem::replace(&mut gui.asked_to_resize, true));
    if !ask {
        return;
    }
    let accepted = unsafe {
        p.host_ext::<clap_host_gui>(CLAP_EXT_GUI)
            .and_then(|g| g.request_resize)
            .is_some_and(|request| request(p.host, 400, 250))
    };
    if accepted && let Some(gui) = p.gui.lock().unwrap().as_mut() {
        resize(gui, 400, 250);
    }
}

unsafe extern "C" fn on_fd(plugin: *const clap_plugin, _fd: i32, _flags: clap_posix_fd_flags) {
    let p = unsafe { Plugin::from(plugin) };
    p.expect_thread(false);
    let gui = p.gui.lock().unwrap();
    let Some(gui) = gui.as_ref() else { return };
    while let Ok(Some(event)) = gui.conn.poll_for_event() {
        p.gui_fd_events.fetch_add(1, Ordering::Relaxed);
        if let Event::ConfigureNotify(e) = event
            && e.window == gui.parent
        {
            p.gui_parent_width
                .store(u64::from(e.width), Ordering::Relaxed);
        }
    }
}
