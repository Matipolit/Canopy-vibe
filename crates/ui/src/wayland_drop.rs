//! Receiving dropped files on Wayland.
//!
//! winit 0.30 implements drag-and-drop on X11, Windows and macOS but not on
//! Wayland, so egui never hears about files dropped there. This listens for
//! them itself: it attaches its own event queue to winit's Wayland connection
//! (as smithay-clipboard, egui's Wayland clipboard, also does), creates a
//! `wl_data_device` per seat, accepts `text/uri-list` offers dropped on our
//! window and turns them into paths.
//!
//! It runs on its own thread so drags are seen even while the UI is idle; a
//! drag over the window sends no pointer events, so nothing else would wake it.
//!
//! KWin (6.6) sends a drag only to the first data device a client created, so
//! this has to start before the window does: smithay-clipboard creates one
//! as soon as the window exists, and ignores drags. `main` starts it on the
//! event loop before handing that to eframe.

use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, Pos2, pos2};
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_client::backend::Backend;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

const URI_LIST: &str = "text/uri-list";
/// How long to wait for the dragging app to send the file list.
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(3);
/// How often the listener thread checks whether it should stop.
const STOP_POLL: Duration = Duration::from_millis(100);

/// What happens over the window during a drag, in egui points.
#[derive(Debug, Clone, PartialEq)]
pub enum DropEvent {
    /// Files are being dragged over this spot.
    Hover(Pos2),
    /// The drag left the window or was cancelled.
    Leave,
    Dropped {
        pos: Pos2,
        paths: Vec<PathBuf>,
    },
}

pub struct WaylandDrops {
    events: Receiver<DropEvent>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WaylandDrops {
    /// Starts listening if `display` is a Wayland connection; `None`
    /// otherwise (on X11 winit delivers drops itself).
    ///
    /// Call before any window exists: the data device is created and
    /// confirmed by the compositor before this returns, so it's the
    /// client's first, which is the one KWin sends drags to.
    pub fn start(display: &impl HasDisplayHandle) -> Option<WaylandDrops> {
        let RawDisplayHandle::Wayland(handle) = display.display_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: the display belongs to the event loop, which outlives this
        // listener: `Drop` stops and joins the thread, and the app (which
        // owns this) is dropped before the event loop.
        let backend = unsafe { Backend::from_foreign_display(handle.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let (sender, events) = mpsc::channel();
        let ctx = Arc::new(Mutex::new(None));
        let (queue, state) = match setup(connection, sender, ctx.clone()) {
            Ok(setup) => setup,
            Err(err) => {
                eprintln!("file drops unavailable: {err}");
                return None;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("wayland drops".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    if let Err(err) = listen(queue, state, &stop) {
                        eprintln!("file drops stopped: {err}");
                    }
                }
            })
            .ok()?;
        Some(WaylandDrops {
            events,
            ctx,
            stop,
            thread: Some(thread),
        })
    }

    /// Where to request repaints when something happens. Until it's set,
    /// events just wait.
    pub fn set_context(&self, ctx: egui::Context) {
        if let Ok(mut slot) = self.ctx.lock() {
            *slot = Some(ctx);
        }
    }

    /// Everything that happened since the last call.
    pub fn poll(&self) -> impl Iterator<Item = DropEvent> + '_ {
        self.events.try_iter()
    }
}

impl Drop for WaylandDrops {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct State {
    connection: Connection,
    sender: Sender<DropEvent>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    manager: Option<WlDataDeviceManager>,
    seats: Vec<WlSeat>,
    devices: Vec<WlDataDevice>,
    /// The offer being dragged over the window, if we accepted it.
    offer: Option<WlDataOffer>,
    pos: Pos2,
}

impl State {
    fn context(&self) -> Option<egui::Context> {
        self.ctx.lock().ok().and_then(|ctx| ctx.clone())
    }

    fn send(&self, event: DropEvent) {
        let _ = self.sender.send(event);
        if let Some(ctx) = self.context() {
            ctx.request_repaint();
        }
    }

    /// Surface coordinates are logical pixels; egui points are those divided
    /// by egui's own zoom.
    fn to_points(&self, x: f64, y: f64) -> Pos2 {
        let zoom = self.context().map_or(1.0, |ctx| ctx.zoom_factor());
        pos2(x as f32 / zoom, y as f32 / zoom)
    }

    /// A data device for every seat, once the manager is known.
    fn create_devices(&mut self, qh: &QueueHandle<State>) {
        let Some(manager) = &self.manager else {
            return;
        };
        for seat in &self.seats[self.devices.len()..] {
            self.devices.push(manager.get_data_device(seat, qh, ()));
        }
    }

    fn drop_offer(&mut self) {
        if let Some(offer) = self.offer.take() {
            offer.destroy();
        }
    }
}

/// Binds the globals and creates the data devices, waiting until the
/// compositor has them.
fn setup(
    connection: Connection,
    sender: Sender<DropEvent>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
) -> Result<(EventQueue<State>, State), String> {
    let mut queue: EventQueue<State> = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut state = State {
        connection: connection.clone(),
        sender,
        ctx,
        manager: None,
        seats: Vec::new(),
        devices: Vec::new(),
        offer: None,
        pos: Pos2::ZERO,
    };
    // The first roundtrip binds the globals and creates the devices; the
    // second makes sure the compositor has processed those requests.
    for _ in 0..2 {
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    }
    if state.manager.is_none() {
        return Err("the compositor has no wl_data_device_manager".into());
    }
    if crate::app::debug_drops() {
        eprintln!(
            "canopy: listening for drops on {} seat(s)",
            state.devices.len()
        );
    }
    Ok((queue, state))
}

fn listen(mut queue: EventQueue<State>, mut state: State, stop: &AtomicBool) -> Result<(), String> {
    // Our own read loop rather than `blocking_dispatch`, so it can notice
    // `stop`. Reading goes through libwayland's prepare/read protocol, which
    // lets winit's thread read the same socket safely.
    while !stop.load(Ordering::Relaxed) {
        queue
            .dispatch_pending(&mut state)
            .map_err(|e| e.to_string())?;
        queue.flush().map_err(|e| e.to_string())?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        if readable(guard.connection_fd(), STOP_POLL) {
            // Another thread may have read our events already; that's fine.
            let _ = guard.read();
        }
    }
    Ok(())
}

/// Waits up to `timeout` for `fd` to have data.
fn readable(fd: BorrowedFd<'_>, timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, and the fd outlives the call.
    let ready = unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as libc::c_int) };
    ready > 0
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" => state
                    .seats
                    .push(registry.bind(name, version.min(1), qh, ())),
                "wl_data_device_manager" => {
                    state.manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                _ => return,
            }
            state.create_devices(qh);
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// The MIME types an offer says it has, collected as they're announced.
#[derive(Debug, Default)]
struct OfferTypes(Mutex<Vec<String>>);

impl OfferTypes {
    fn has(&self, mime: &str) -> bool {
        self.0
            .lock()
            .is_ok_and(|types| types.iter().any(|t| t == mime))
    }
}

impl Dispatch<WlDataOffer, OfferTypes> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        types: &OfferTypes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Ok(mut types) = types.0.lock()
        {
            types.push(mime_type);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if crate::app::debug_drops() {
            eprintln!("canopy: data device {event:?}");
        }
        match event {
            wl_data_device::Event::Enter {
                serial, x, y, id, ..
            } => {
                state.drop_offer();
                let Some(offer) = id else {
                    return;
                };
                let files = offer.data::<OfferTypes>().is_some_and(|t| t.has(URI_LIST));
                if !files {
                    // Not files (e.g. dragged text): say we don't want it.
                    offer.accept(serial, None);
                    offer.destroy();
                    return;
                }
                offer.accept(serial, Some(URI_LIST.to_owned()));
                if offer.version() >= 3 {
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                }
                state.offer = Some(offer);
                state.pos = state.to_points(x, y);
                state.send(DropEvent::Hover(state.pos));
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                if state.offer.is_some() {
                    state.pos = state.to_points(x, y);
                    state.send(DropEvent::Hover(state.pos));
                }
            }
            wl_data_device::Event::Leave => {
                if state.offer.is_some() {
                    state.drop_offer();
                    state.send(DropEvent::Leave);
                }
            }
            wl_data_device::Event::Drop => {
                let Some(offer) = state.offer.take() else {
                    return;
                };
                let paths = receive_paths(&state.connection, &offer);
                if offer.version() >= 3 {
                    offer.finish();
                }
                offer.destroy();
                match paths {
                    Ok(paths) if !paths.is_empty() => state.send(DropEvent::Dropped {
                        pos: state.pos,
                        paths,
                    }),
                    Ok(_) => state.send(DropEvent::Leave),
                    Err(err) => {
                        eprintln!("couldn't read dropped files: {err}");
                        state.send(DropEvent::Leave);
                    }
                }
            }
            // The clipboard, which isn't ours to handle.
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferTypes::default()),
    ]);
}

/// Asks the dragging app for the file list and reads it from a pipe.
fn receive_paths(connection: &Connection, offer: &WlDataOffer) -> std::io::Result<Vec<PathBuf>> {
    let (mut reader, writer) = std::io::pipe()?;
    offer.receive(URI_LIST.to_owned(), writer.as_fd());
    // Our copy of the write end must close, or the read never sees EOF.
    drop(writer);
    connection
        .flush()
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let deadline = Instant::now() + RECEIVE_TIMEOUT;
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !readable(reader.as_fd(), left) {
            return Err(std::io::Error::other(
                "the dragging app didn't send the file list",
            ));
        }
        match reader.read(&mut chunk)? {
            0 => break,
            n => data.extend_from_slice(&chunk[..n]),
        }
    }
    Ok(parse_uri_list(&data))
}

/// The local paths in a `text/uri-list` (RFC 2483): one URI per line,
/// `#` comments, `file://` URIs percent-encoded.
fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    data.split(|b| *b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .filter_map(|line| line.strip_prefix(b"file://"))
        .filter_map(|rest| {
            // file:///path, or file://localhost/path.
            let path = rest.strip_prefix(b"localhost").unwrap_or(rest);
            path.starts_with(b"/").then(|| percent_decode(path))
        })
        .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        .collect()
}

fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    #[test]
    fn parses_uri_lists() {
        let list = b"# from Dolphin\r\nfile:///home/me/Samples/kick%2001.wav\r\nfile://localhost/tmp/caf%C3%A9.flac\r\nhttps://example.com/x.wav\r\nfile:///tmp/raw%e9.wav\n";
        let paths = parse_uri_list(list);
        assert_eq!(paths[0], PathBuf::from("/home/me/Samples/kick 01.wav"));
        assert_eq!(paths[1], PathBuf::from("/tmp/café.flac"));
        assert_eq!(
            paths[2].as_os_str().as_bytes(),
            b"/tmp/raw\xe9.wav",
            "not UTF-8, kept as bytes"
        );
        assert_eq!(paths.len(), 3, "the https URI is skipped");
        assert_eq!(percent_decode(b"100%"), b"100%", "a stray % stays");
    }
}
