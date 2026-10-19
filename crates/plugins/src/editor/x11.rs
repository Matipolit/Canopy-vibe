//! Editor windows on X11 (through XWayland on a Wayland desktop), with
//! x11rb's pure-Rust connection: one connection per window, so closing an
//! editor closes everything it opened.

use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
    EventMask, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{EditorOptions, Parent, WindowEvent};
use crate::Error;

/// A top-level X11 window for one plugin editor.
pub(crate) struct Window {
    conn: RustConnection,
    root: u32,
    window: u32,
    wm_delete_window: u32,
    net_wm_state: u32,
    net_wm_state_above: u32,
}

fn error(e: impl std::fmt::Display) -> Error {
    Error::Editor(format!("X11: {e}"))
}

impl Window {
    /// Opens and maps a `width` × `height` window.
    pub fn open(
        options: &EditorOptions,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Window, Error> {
        let title = options.title.as_str();
        let (conn, screen) = x11rb::connect(None).map_err(error)?;
        let screen = &conn.setup().roots[screen];
        let window = conn.generate_id().map_err(error)?;
        let aux = CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(EventMask::STRUCTURE_NOTIFY);
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            screen.root,
            0,
            0,
            clamp_u16(width),
            clamp_u16(height),
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .map_err(error)?;
        let atom = |name: &[u8]| -> Result<u32, Error> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(error)?
                .reply()
                .map_err(error)?
                .atom)
        };
        let wm_protocols = atom(b"WM_PROTOCOLS")?;
        let wm_delete_window = atom(b"WM_DELETE_WINDOW")?;
        let net_wm_name = atom(b"_NET_WM_NAME")?;
        let utf8_string = atom(b"UTF8_STRING")?;
        let net_wm_state = atom(b"_NET_WM_STATE")?;
        let net_wm_state_above = atom(b"_NET_WM_STATE_ABOVE")?;
        if options.keep_on_top {
            // Before mapping, the property itself is the request.
            conn.change_property32(
                PropMode::REPLACE,
                window,
                net_wm_state,
                AtomEnum::ATOM,
                &[net_wm_state_above],
            )
            .map_err(error)?;
        }
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        )
        .map_err(error)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            net_wm_name,
            utf8_string,
            title.as_bytes(),
        )
        .map_err(error)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"canopy-plugin\0canopy\0",
        )
        .map_err(error)?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            wm_protocols,
            AtomEnum::ATOM,
            &[wm_delete_window],
        )
        .map_err(error)?;
        let root = screen.root;
        let this = Window {
            conn,
            root,
            window,
            wm_delete_window,
            net_wm_state,
            net_wm_state_above,
        };
        this.set_hints(width, height, resizable)?;
        this.conn.map_window(window).map_err(error)?;
        this.conn.flush().map_err(error)?;
        Ok(this)
    }

    /// Fixed-size windows say so, so window managers don't offer resizing.
    fn set_hints(&self, width: u32, height: u32, resizable: bool) -> Result<(), Error> {
        let mut hints = WmSizeHints::new();
        if !resizable {
            let size = (width as i32, height as i32);
            hints.min_size = Some(size);
            hints.max_size = Some(size);
        }
        hints
            .set_normal_hints(&self.conn, self.window)
            .map_err(error)?;
        Ok(())
    }

    pub fn parent(&self) -> Parent {
        Parent::X11(self.window)
    }

    pub fn set_size(&self, width: u32, height: u32, resizable: bool) {
        let _ = self.set_hints(width, height, resizable);
        let aux = ConfigureWindowAux::new()
            .width(width.max(1))
            .height(height.max(1));
        let _ = self.conn.configure_window(self.window, &aux);
        let _ = self.conn.flush();
    }

    /// Asks the window manager to keep the window above others, or not
    /// (EWMH: a `_NET_WM_STATE` message to the root window).
    pub fn set_keep_on_top(&self, on_top: bool) {
        let event = ClientMessageEvent::new(
            32,
            self.window,
            self.net_wm_state,
            [u32::from(on_top), self.net_wm_state_above, 0, 1, 0],
        );
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        let _ = self.conn.send_event(false, self.root, mask, event);
        let _ = self.conn.flush();
    }

    /// The connection's socket, readable when the window has events.
    pub fn fd(&self) -> Option<i32> {
        use std::os::fd::AsRawFd;
        Some(self.conn.stream().as_raw_fd())
    }

    pub fn show(&self, visible: bool) {
        let _ = if visible {
            self.conn.map_window(self.window)
        } else {
            self.conn.unmap_window(self.window)
        };
        let _ = self.conn.flush();
    }

    /// What happened to the window since the last poll; resizes coalesced
    /// to the latest.
    pub fn poll(&self) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        let mut resized = None;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::ConfigureNotify(e) if e.window == self.window => {
                    resized = Some((u32::from(e.width), u32::from(e.height)));
                }
                Event::ClientMessage(e)
                    if e.window == self.window
                        && e.format == 32
                        && e.data.as_data32()[0] == self.wm_delete_window =>
                {
                    events.push(WindowEvent::CloseRequested);
                }
                _ => {}
            }
        }
        if let Some((width, height)) = resized {
            events.insert(0, WindowEvent::Resized(width, height));
        }
        events
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

fn clamp_u16(value: u32) -> u16 {
    value.clamp(1, u32::from(u16::MAX)) as u16
}
