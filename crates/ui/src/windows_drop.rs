//! Where files dragged over the window are, on Windows.
//!
//! winit 0.30 tells egui that files are being dragged over the window and
//! when they're dropped, but not where: its `IDropTarget` ignores the cursor
//! position Windows hands it, and the window gets no mouse moves while
//! another program runs the drag. egui's pointer is stale or gone meanwhile,
//! so this asks Windows where the cursor is, both to show where the files
//! will land and to drop them there.

use eframe::egui::{self, Pos2, pos2};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// The cursor in egui points, or `None` if Windows won't say or the window
/// has no position (it's minimized).
pub fn cursor_pos(ctx: &egui::Context) -> Option<Pos2> {
    let mut cursor = POINT { x: 0, y: 0 };
    // SAFETY: `cursor` is a POINT for the call to write to.
    if unsafe { GetCursorPos(&mut cursor) } == 0 {
        return None;
    }
    // Screen pixels to points the way egui-winit measures the window: by
    // the monitor's scale times egui's zoom.
    let pixels_per_point = ctx.pixels_per_point();
    let window = ctx.input(|i| i.viewport().inner_rect)?;
    let screen = pos2(
        cursor.x as f32 / pixels_per_point,
        cursor.y as f32 / pixels_per_point,
    );
    Some(screen - window.min.to_vec2())
}

/// Call every frame. While files are dragged over the window, keeps `hover`
/// at the cursor and repaints, since the drag sends the window nothing to
/// repaint for; clears it once they're dropped or leave.
pub fn update_hover(ctx: &egui::Context, hover: &mut Option<Pos2>) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        *hover = None;
    } else {
        *hover = cursor_pos(ctx);
        ctx.request_repaint();
    }
}
