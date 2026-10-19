//! Drags a file onto whatever window is at a screen point, using KWin's fake
//! input protocol. Run by `run.sh`, which starts a headless KWin and canopy.
//!
//! Usage: wayland-drop-test <file> <screen-w> <screen-h> <target-x> <target-y>
use std::io::Write;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_source, wl_pointer,
    wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

const BTN_LEFT: u32 = 0x110;
const SIZE: i32 = 120;

#[derive(Default)]
struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    ddm: Option<wl_data_device_manager::WlDataDeviceManager>,
    fake: Option<OrgKdeKwinFakeInput>,
    surface: Option<wl_surface::WlSurface>,
    configured: bool,
    entered: Option<(f64, f64)>,
    button_serial: Option<u32>,
    uri: String,
    sent: bool,
    done: Option<&'static str>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match interface.as_str() {
                "wl_compositor" => app.compositor = Some(r.bind(name, version.min(4), qh, ())),
                "wl_shm" => app.shm = Some(r.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm = Some(r.bind(name, 1, qh, ())),
                "wl_seat" => app.seat = Some(r.bind(name, version.min(5), qh, ())),
                "wl_data_device_manager" => app.ddm = Some(r.bind(name, version.min(3), qh, ())),
                "org_kde_kwin_fake_input" => app.fake = Some(r.bind(name, version.min(4), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        s: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            s.ack_configure(serial);
            app.configured = true;
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => app.entered = Some((surface_x, surface_y)),
            wl_pointer::Event::Leave { .. } => app.entered = None,
            wl_pointer::Event::Button {
                serial,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => app.button_serial = Some(serial),
            _ => {}
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_data_source::WlDataSource,
        e: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_data_source::Event::Send { mime_type, fd } => {
                eprintln!("source: asked for {mime_type}");
                let mut file = std::fs::File::from(fd);
                let _ = write!(file, "{}\r\n", app.uri);
                app.sent = true;
            }
            wl_data_source::Event::DndFinished => app.done = Some("finished"),
            wl_data_source::Event::Cancelled => app.done = Some("cancelled"),
            wl_data_source::Event::Target { mime_type } => {
                eprintln!("source: target accepts {mime_type:?}")
            }
            wl_data_source::Event::Action { dnd_action } => {
                eprintln!("source: action {dnd_action:?}")
            }
            _ => {}
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_seat::WlSeat);
delegate_noop!(App: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(App: ignore wl_data_device_manager::WlDataDeviceManager);
delegate_noop!(App: ignore wayland_client::protocol::wl_data_offer::WlDataOffer);

impl Dispatch<wl_data_device::WlDataDevice, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_data_device::WlDataDevice,
        e: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Our own drag passes over our own window too; never accept it.
        if let wl_data_device::Event::Enter {
            serial,
            id: Some(offer),
            ..
        } = e
        {
            offer.accept(serial, None);
        }
    }

    wayland_client::event_created_child!(App, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wayland_client::protocol::wl_data_offer::WlDataOffer, ()),
    ]);
}
delegate_noop!(App: ignore OrgKdeKwinFakeInput);

fn pump(queue: &mut EventQueue<App>, app: &mut App, wait: Duration) {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        queue.roundtrip(app).unwrap();
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let file = std::fs::canonicalize(&args[1]).unwrap();
    let (w, h): (f64, f64) = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let target: (f64, f64) = (args[4].parse().unwrap(), args[5].parse().unwrap());
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App {
        uri: format!("file://{}", file.display().to_string().replace(' ', "%20")),
        ..Default::default()
    };
    queue.roundtrip(&mut app).unwrap();
    let fake = app
        .fake
        .clone()
        .expect("no fake input (KWIN_WAYLAND_NO_PERMISSION_CHECKS=1?)");
    fake.authenticate("canopy dnd test".into(), "drag a file".into());
    app.seat.as_ref().unwrap().get_pointer(&qh, ());

    // A small window to drag from.
    let surface = app.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg = app.wm.as_ref().unwrap().get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    top.set_title("dnd source".into());
    surface.commit();
    while !app.configured {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    let path = std::env::temp_dir().join(format!("dnd-source-{}", std::process::id()));
    let mut shm_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let stride = SIZE * 4;
    shm_file
        .write_all(&vec![0x80u8; (stride * SIZE) as usize])
        .unwrap();
    let pool = app
        .shm
        .as_ref()
        .unwrap()
        .create_pool(shm_file.as_fd(), stride * SIZE, &qh, ());
    let buffer = pool.create_buffer(0, SIZE, SIZE, stride, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, SIZE, SIZE);
    surface.commit();
    app.surface = Some(surface.clone());
    pump(&mut queue, &mut app, Duration::from_millis(500));

    // Find our window by moving the pointer until it enters it.
    let mut origin = None;
    'scan: for y in (20..h as i32).step_by(30) {
        for x in (20..w as i32).step_by(30) {
            fake.pointer_motion_absolute(x as f64, y as f64);
            queue.roundtrip(&mut app).unwrap();
            queue.roundtrip(&mut app).unwrap();
            if let Some((lx, ly)) = app.entered {
                origin = Some((x as f64 - lx, y as f64 - ly));
                break 'scan;
            }
        }
    }
    let origin = origin.expect("never found the source window");
    eprintln!("source window at {origin:?}");
    let start = (origin.0 + SIZE as f64 / 2.0, origin.1 + SIZE as f64 / 2.0);
    fake.pointer_motion_absolute(start.0, start.1);
    pump(&mut queue, &mut app, Duration::from_millis(100));
    fake.button(BTN_LEFT, 1);
    pump(&mut queue, &mut app, Duration::from_millis(150));
    let serial = app.button_serial.expect("no button press");

    let source = app.ddm.as_ref().unwrap().create_data_source(&qh, ());
    source.offer("text/uri-list".into());
    source.set_actions(
        wl_data_device_manager::DndAction::Copy | wl_data_device_manager::DndAction::Move,
    );
    let device = app
        .ddm
        .as_ref()
        .unwrap()
        .get_data_device(app.seat.as_ref().unwrap(), &qh, ());
    device.start_drag(Some(&source), &surface, None, serial);
    pump(&mut queue, &mut app, Duration::from_millis(150));

    for i in 1..=30 {
        let t = i as f64 / 30.0;
        fake.pointer_motion_absolute(
            start.0 + (target.0 - start.0) * t,
            start.1 + (target.1 - start.1) * t,
        );
        pump(&mut queue, &mut app, Duration::from_millis(30));
    }
    pump(&mut queue, &mut app, Duration::from_millis(300));
    fake.button(BTN_LEFT, 0);
    let end = Instant::now() + Duration::from_secs(5);
    while app.done.is_none() && Instant::now() < end {
        pump(&mut queue, &mut app, Duration::from_millis(50));
    }
    println!("drag result: {:?}, file list sent: {}", app.done, app.sent);
}
