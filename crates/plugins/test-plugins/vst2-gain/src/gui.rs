//! A tiny editor: an X11 window inside the host's, which runs only from the
//! host's `effEditIdle` calls, as VST2 GUIs on Linux do. On its first idle
//! it asks the host for 400 × 250 (`audioMasterSizeWindow`), and it
//! watches the host's window to see that happen. What it saw goes in
//! parameters 7 to 10.

use std::ptr::null_mut;
use std::sync::atomic::Ordering;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::Plugin;
use crate::abi::{ERect, audioMasterSizeWindow};

pub struct Gui {
    conn: RustConnection,
    window: u32,
    parent: u32,
    asked_to_resize: bool,
}

fn size(p: &Plugin) -> (u16, u16) {
    // SAFETY: the rectangle is only written on the main thread, where
    // this runs.
    let rect = unsafe { *p.rect.get() };
    (
        (rect.right - rect.left).max(1) as u16,
        (rect.bottom - rect.top).max(1) as u16,
    )
}

/// Opens the editor inside `parent`.
pub fn open(p: &Plugin, parent: u32) -> bool {
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return false;
    };
    let Ok(window) = conn.generate_id() else {
        return false;
    };
    let white = conn.setup().roots[screen].white_pixel;
    let aux = CreateWindowAux::new()
        .background_pixel(white)
        .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY);
    let (width, height) = size(p);
    let created = conn
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            parent,
            0,
            0,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .is_ok();
    if !created {
        return false;
    }
    // Watch the host's window too, to see it resized.
    let watch = ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY);
    let _ = conn.change_window_attributes(parent, &watch);
    let _ = conn.map_window(window);
    let _ = conn.flush();
    *p.gui.lock().unwrap() = Some(Gui {
        conn,
        window,
        parent,
        asked_to_resize: false,
    });
    p.gui_open.store(true, Ordering::Relaxed);
    true
}

/// One `effEditIdle`: asks to be resized the first time, then reads what
/// happened to the windows.
pub fn idle(p: &Plugin) {
    p.gui_ticks.fetch_add(1, Ordering::Relaxed);
    let ask = p
        .gui
        .lock()
        .unwrap()
        .as_mut()
        .is_some_and(|gui| !std::mem::replace(&mut gui.asked_to_resize, true));
    // Outside the lock: the host may call back.
    if ask && p.call(audioMasterSizeWindow, 400, 250, null_mut(), 0.0) == 1 {
        // SAFETY: as in `size`.
        unsafe {
            *p.rect.get() = ERect {
                top: 0,
                left: 0,
                bottom: 250,
                right: 400,
            }
        };
        if let Some(gui) = p.gui.lock().unwrap().as_ref() {
            let aux = ConfigureWindowAux::new().width(400).height(250);
            let _ = gui.conn.configure_window(gui.window, &aux);
            let _ = gui.conn.flush();
        }
    }
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

pub fn close(p: &Plugin) {
    let Some(gui) = p.gui.lock().unwrap().take() else {
        return;
    };
    let _ = gui.conn.destroy_window(gui.window);
    let _ = gui.conn.flush();
    p.gui_open.store(false, Ordering::Relaxed);
}
