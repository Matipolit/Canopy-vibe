//! canopy-learn: learn to play keyboard, with a MIDI keyboard or without.

// On Windows, release builds are GUI programs, with no console window, as
// the DAW's are (unless built with the `console` feature).
#![cfg_attr(
    all(not(debug_assertions), not(test), not(feature = "console")),
    windows_subsystem = "windows"
)]

use eframe::egui;

// In debug builds, abort if the audio thread allocates inside
// `assert_no_alloc`, as the DAW does.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("canopy learn")
            .with_app_id("canopy-learn")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "canopy-learn",
        options,
        Box::new(|cc| Ok(Box::new(learn::LearnApp::new(cc)))),
    )
}
