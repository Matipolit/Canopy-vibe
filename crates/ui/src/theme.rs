//! The look every view shares: the spacing scale, sizes, corner radii,
//! strokes, text sizes, animation times and colors, and the egui style made
//! from them ([`apply`]).
//!
//! Views take their numbers from here rather than inventing their own, so a
//! title bar is as tall in the device view as over the piano roll, a ruler
//! is as tall over the piano roll as over the arrangement, and a toggle in a
//! track header is the size of one in a panel. Colors that belong to one
//! display (an EQ band's, a scope's) stay with it; what's here is what more
//! than one view uses.
//!
//! The small painted controls built on these numbers (fold arrows, icon
//! buttons, key caps) are in `widgets::chrome`.
//!
//! # Using it
//!
//! - Gaps and margins: [`space`]. Corners: [`radius`]. Lines: [`stroke`].
//!   Text: [`text`] (egui's `small()` is [`text::SMALL`]). Control and bar
//!   heights: [`size`].
//! - Motion: [`hover`] for hover feedback, [`fade`] for things that turn on
//!   and off (a selection, a lit toggle), [`glide`] and [`glide_marker`]
//!   for a marker that follows something in steps (a drop line), all on
//!   the [`anim`] times. They ask
//!   egui for frames only while they move, so an idle window stays idle.
//! - Colors that mean the same thing everywhere: [`color`] and [`accent`].
//!
//! The names and meanings here are kept stable: add to them rather than
//! renaming, since every view (device panels included) leans on them.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, TextStyle};

/// The spacing scale, in points. Gaps and margins are one of these.
pub mod space {
    /// Between the parts of one control (an icon and its text).
    pub const XS: f32 = 2.0;
    /// Between controls in a row.
    pub const S: f32 = 4.0;
    /// Inside a title bar or a header, from its edge to its contents.
    pub const M: f32 = 8.0;
    /// Between groups of controls.
    pub const L: f32 = 12.0;
    /// Around a dialog's contents.
    pub const XL: f32 = 16.0;
}

/// Corner radii.
pub mod radius {
    /// Marks inside displays: meter bars, note heads.
    pub const SMALL: u8 = 2;
    /// Controls, clips, notes and title bars.
    pub const CONTROL: u8 = 3;
    /// Framed displays (a curve, a scope).
    pub const PANEL: u8 = 4;
    /// Windows and menus.
    pub const WINDOW: u8 = 6;
}

/// Stroke widths.
pub mod stroke {
    /// Separators, grid lines, outlines of things at rest.
    pub const HAIRLINE: f32 = 1.0;
    /// The playhead, and outlines of selected things.
    pub const EMPHASIS: f32 = 1.5;
    /// Where a drop will land, and the keyboard focus.
    pub const INDICATOR: f32 = 2.0;
}

/// Text sizes, in points.
pub mod text {
    /// Labels squeezed into a display: key names, sub-ticks.
    pub const TINY: f32 = 9.0;
    /// Captions: clip names, ruler numbers, knob values, egui's `small()`.
    pub const SMALL: f32 = 10.5;
    /// Labels and buttons.
    pub const BODY: f32 = 13.0;
    /// A view's title (the track's name over its devices).
    pub const HEADING: f32 = 16.0;
}

/// Sizes of things that several views draw.
pub mod size {
    /// The height of buttons, fields and combo boxes.
    pub const CONTROL: f32 = 18.0;
    /// Toggles that sit inside a row of their own (mute, solo).
    pub const SMALL_CONTROL: f32 = 16.0;
    /// Painted icon buttons.
    pub const ICON: f32 = 18.0;
    /// Title bars: the detail view's tabs, effects' and instruments' bars.
    pub const BAR: f32 = 24.0;
    /// Rulers over the arrangement and the piano roll.
    pub const RULER: f32 = 24.0;
    /// How tall the loop brace along the top of a ruler is; dragging in it
    /// sets the loop.
    pub const LOOP_BRACE: f32 = 7.0;
    /// The narrowest a dialog's buttons are (`chrome::dialog_buttons`).
    pub const DIALOG_BUTTON: f32 = 72.0;
    /// One level bar beside a device's display (a pair stands
    /// [`space::XS`](super::space::XS) apart: `widgets::scopes::level_bars`).
    pub const LEVEL_BAR: f32 = 8.0;
}

/// Animation times, in seconds: short enough never to be waited on.
pub mod anim {
    /// Hover and press feedback, and markers following the pointer.
    pub const FAST: f32 = 0.08;
    /// Selection highlights, folds and panels.
    pub const NORMAL: f32 = 0.12;
    /// Zooming to fit, and other moves of the whole view.
    pub const ZOOM: f32 = 0.15;

    /// Eases `t` (0 to 1) out: quick to start, gentle to land.
    pub fn ease_out(t: f32) -> f32 {
        1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
    }
}

/// Colors used by more than one view.
pub mod color {
    use eframe::egui::Color32;

    /// The playhead, wherever time is shown.
    pub const PLAYHEAD: Color32 = Color32::from_rgb(255, 140, 60);
    /// The loop brace and the loop's bounds.
    pub const LOOP: Color32 = Color32::from_rgb(110, 190, 120);
    pub const MUTE: Color32 = Color32::from_rgb(230, 170, 60);
    pub const SOLO: Color32 = Color32::from_rgb(70, 150, 230);
    /// Recording armed.
    pub const RECORD: Color32 = Color32::from_rgb(230, 60, 60);
    /// Something live: MIDI coming in.
    pub const ACTIVE: Color32 = Color32::from_rgb(90, 200, 120);
    /// Level meters: safe, hot, clipping.
    pub const METER_LOW: Color32 = Color32::from_rgb(80, 200, 120);
    pub const METER_MID: Color32 = Color32::from_rgb(230, 200, 70);
    pub const METER_HIGH: Color32 = Color32::from_rgb(230, 70, 60);
    /// Text on a bright fill (a lit toggle, a clip's title).
    pub const ON_BRIGHT: Color32 = Color32::from_black_alpha(220);
    /// A title bar at rest, and hovered.
    pub const BAR: Color32 = Color32::from_rgba_premultiplied(10, 10, 10, 10);
    pub const BAR_HOVER: Color32 = Color32::from_rgba_premultiplied(22, 22, 22, 22);
    /// Over what doesn't play (outside a clip).
    pub const SHADE: Color32 = Color32::from_black_alpha(110);
    /// The outline of a selected clip or note.
    pub const SELECTED_OUTLINE: Color32 = Color32::WHITE;
    /// The outline of a clip or note at rest, against what's around it.
    pub const OUTLINE: Color32 = Color32::from_black_alpha(140);
}

/// The accent: what's selected, where a drop lands, the keyboard focus.
pub fn accent(visuals: &egui::Visuals) -> Color32 {
    visuals.selection.stroke.color
}

/// A selected thing's fill, blended over `base` by `t` (0 to 1, for a fade).
pub fn selected_fill(visuals: &egui::Visuals, base: Color32, t: f32) -> Color32 {
    base.lerp_to_gamma(visuals.selection.bg_fill, 0.45 * t)
}

/// How far a hover over `response` has faded in, 0 to 1. Repaints only
/// while it's changing.
pub fn hover(ui: &egui::Ui, response: &egui::Response) -> f32 {
    ui.ctx().animate_bool_with_time(
        response.id.with("hover fade"),
        response.hovered() || response.has_focus(),
        anim::FAST,
    )
}

/// How far `on` has faded in for `id`, 0 to 1, over [`anim::NORMAL`]: for
/// highlights that come and go (a selection, a lit toggle, a fold). Asks
/// for frames only while it's changing.
pub fn fade(ctx: &egui::Context, id: egui::Id, on: bool) -> f32 {
    ctx.animate_bool_with_time(id, on, anim::NORMAL)
}

/// `target`, eased there over [`anim::FAST`] when it changes: for a marker
/// that follows something in steps (a drop line jumping from gap to gap).
/// The first time `id` is seen it starts at `target`. Asks for frames only
/// while it's moving.
pub fn glide(ctx: &egui::Context, id: egui::Id, target: f32) -> f32 {
    ctx.animate_value_with_time(id, target, anim::FAST)
}

/// [`glide`] for a marker that comes and goes (a drop line, the focus
/// ring): each time it reappears it starts where it's first shown, rather
/// than sliding over from wherever it was last time.
pub fn glide_marker(ctx: &egui::Context, id: egui::Id, target: f32) -> f32 {
    let pass = ctx.cumulative_pass_nr();
    let (last, generation) = ctx.data(|d| d.get_temp::<(u64, u64)>(id)).unwrap_or((0, 0));
    let shown_just_now = last == pass || last + 1 == pass;
    let generation = if shown_just_now {
        generation
    } else {
        generation + 1
    };
    ctx.data_mut(|d| d.insert_temp(id, (pass, generation)));
    glide(ctx, id.with(generation), target)
}

/// Applies the theme to every style egui keeps. canopy is dark only: its
/// painted views (lanes, keys, clips) are drawn for a dark ground.
pub fn apply(ctx: &egui::Context) {
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Small,
                FontId::new(text::SMALL, FontFamily::Proportional),
            ),
            (
                TextStyle::Body,
                FontId::new(text::BODY, FontFamily::Proportional),
            ),
            (
                TextStyle::Button,
                FontId::new(text::BODY, FontFamily::Proportional),
            ),
            (
                TextStyle::Heading,
                FontId::new(text::HEADING, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(text::BODY - 0.5, FontFamily::Monospace),
            ),
        ]
        .into();
        style.animation_time = anim::NORMAL;

        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(space::M - 2.0, space::S - 1.0);
        spacing.button_padding = egui::vec2(space::S + 1.0, 1.0);
        spacing.interact_size.y = size::CONTROL;
        spacing.window_margin = Margin::same(space::L as i8);
        spacing.menu_margin = Margin::same(space::S as i8 + 2);
        spacing.icon_spacing = space::S;

        let visuals = &mut style.visuals;
        let control = CornerRadius::same(radius::CONTROL);
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = control;
        }
        visuals.window_corner_radius = CornerRadius::same(radius::WINDOW);
        visuals.menu_corner_radius = CornerRadius::same(radius::WINDOW);
        // Volume and pan sliders read better filled up to their handle.
        visuals.slider_trailing_fill = true;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_grows() {
        let steps = [space::XS, space::S, space::M, space::L, space::XL];
        assert!(steps.windows(2).all(|w| w[0] < w[1]));
        let sizes = [text::TINY, text::SMALL, text::BODY, text::HEADING];
        assert!(sizes.windows(2).all(|w| w[0] < w[1]));
        const {
            assert!(anim::FAST < anim::NORMAL && anim::NORMAL <= anim::ZOOM);
            assert!(anim::ZOOM <= 0.15, "animations stay out of the way");
        }
    }

    #[test]
    fn easing_runs_from_start_to_end() {
        assert_eq!(anim::ease_out(0.0), 0.0);
        assert_eq!(anim::ease_out(1.0), 1.0);
        assert!(anim::ease_out(0.5) > 0.5, "most of the way by half time");
        assert_eq!(anim::ease_out(2.0), 1.0, "clamped");
    }

    #[test]
    fn applying_keeps_the_ui_dark() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let style = ctx.global_style();
        assert!(style.visuals.dark_mode);
        assert_eq!(style.spacing.interact_size.y, size::CONTROL);
        assert_eq!(
            style.text_styles[&TextStyle::Small].size,
            text::SMALL,
            "egui's small() is our caption size"
        );
    }
}
