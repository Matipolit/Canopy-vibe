//! canopy: a small DAW built for one person's workflow.

// On Windows, release builds are GUI programs: no console window opens next to
// canopy's. Debug builds keep it for what canopy logs, and so do release
// builds with the `console` feature.
#![cfg_attr(
    all(not(debug_assertions), not(test), not(feature = "console")),
    windows_subsystem = "windows"
)]

mod app;
mod banks;
mod clip_panel;
mod device_view;
mod devices;
mod effect_rack;
mod export;
mod flp_import;
mod fx_view;
mod midi_input;
mod modulation;
mod patcher;
mod piano_roll;
mod plugins;
mod presets;
mod renders;
mod session;
mod taps;
mod theme;
mod timeline;
#[cfg(test)]
mod ui_tests;
#[cfg(target_os = "linux")]
mod wayland_drop;
mod widgets;
#[cfg(windows)]
mod windows_drop;

use eframe::egui;

// In debug builds, abort if the audio thread allocates inside `assert_no_alloc`.
// That surfaces real-time violations the moment they're written instead of as
// occasional glitches weeks later.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn main() -> eframe::Result {
    // Scanning plugins runs this program again, once per plugin file, so a
    // plugin that crashes while loading takes only that process down.
    ::plugins::scan::run_scanner_if_requested();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("canopy")
            .with_app_id("canopy")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    // Our own event loop rather than `run_native`, so that on Wayland the
    // drop listener can create its data device before the window (and with
    // it egui's clipboard) exists. KWin sends drag-and-drop only to a
    // client's first data device; the clipboard's ignores drops.
    let event_loop =
        winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    // `CANOPY_NO_WAYLAND_DROPS=1` turns the listener off, for diagnosing.
    // Elsewhere winit delivers drops (on Windows without a position, which
    // `windows_drop` finds).
    #[cfg(target_os = "linux")]
    let drops = std::env::var_os("CANOPY_NO_WAYLAND_DROPS")
        .is_none()
        .then(|| wayland_drop::WaylandDrops::start(&event_loop))
        .flatten();
    #[cfg(not(target_os = "linux"))]
    let drops = ();
    let mut app = eframe::create_native(
        "canopy",
        options,
        Box::new(move |cc| Ok(Box::new(app::CanopyApp::new(cc, drops)))),
        &event_loop,
    );
    event_loop.run_app(&mut app)?;
    Ok(())
}
