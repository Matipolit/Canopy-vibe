//! Horizontal zoom and scroll: the one place that converts ticks to screen
//! x and back. The ruler, lanes, playhead and piano roll all go through it, so
//! they can't disagree about where a beat is.

/// Default zoom: one bar of 4/4 is 120 px.
const DEFAULT_PX_PER_TICK: f64 = 120.0 / 3840.0;
/// Zoomed all the way out, a bar is 2 px.
const MIN_PX_PER_TICK: f64 = 2.0 / 3840.0;
/// Zoomed all the way in, a 1/64 note (60 ticks) is 60 px.
const MAX_PX_PER_TICK: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// The tick at the left edge.
    pub start: f64,
    pub px_per_tick: f64,
    /// How far left scrolling may go. The timeline starts at zero; the piano
    /// roll may show content before a clip's start.
    pub min_start: f64,
    /// Screen position of the area ticks map onto, set every frame.
    pub left: f32,
    pub width: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            start: 0.0,
            px_per_tick: DEFAULT_PX_PER_TICK,
            min_start: 0.0,
            left: 0.0,
            width: 1.0,
        }
    }
}

impl Viewport {
    pub fn set_screen(&mut self, left: f32, width: f32) {
        self.left = left;
        self.width = width.max(1.0);
    }

    pub fn x(&self, tick: f64) -> f32 {
        self.left + ((tick - self.start) * self.px_per_tick) as f32
    }

    pub fn tick_at(&self, x: f32) -> f64 {
        self.start + (x - self.left) as f64 / self.px_per_tick
    }

    /// The tick at the right edge.
    pub fn end(&self) -> f64 {
        self.tick_at(self.left + self.width)
    }

    /// On-screen width of a span of ticks.
    pub fn width_of(&self, ticks: f64) -> f32 {
        (ticks * self.px_per_tick) as f32
    }

    /// Zooms by `factor`, keeping the tick under screen `x` where it is.
    pub fn zoom_around(&mut self, x: f32, factor: f64) {
        let anchor = self.tick_at(x);
        self.px_per_tick = (self.px_per_tick * factor).clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.start = anchor - (x - self.left) as f64 / self.px_per_tick;
        self.clamp();
    }

    /// Shows from tick `start` at the zoom `px_per_tick`, within the
    /// zoom's limits (a saved view, say).
    pub fn show_from(&mut self, start: f64, px_per_tick: f64) {
        self.px_per_tick = px_per_tick.clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.start = start;
        self.clamp();
    }

    /// Scrolls so the content moves right by `dx` pixels.
    pub fn scroll_by_px(&mut self, dx: f32) {
        self.start -= dx as f64 / self.px_per_tick;
        self.clamp();
    }

    /// Scrolls by `ticks` (positive moves later in time).
    pub fn shift(&mut self, ticks: f64) {
        self.start += ticks;
        self.clamp();
    }

    /// This view with `start..end` fitted in it (see [`Viewport::fit`]).
    pub fn fitted(&self, start: f64, end: f64) -> Viewport {
        let mut fitted = *self;
        fitted.fit(start, end);
        fitted
    }

    /// Fits `start..end` in view, with a little room on both sides.
    pub fn fit(&mut self, start: f64, end: f64) {
        let span = (end - start).max(1.0);
        self.px_per_tick =
            (self.width as f64 / (span * 1.1)).clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.start = start - span * 0.05;
        self.clamp();
    }

    /// Pages forward or back when `tick` has left the view, so a playing
    /// playhead stays visible.
    pub fn follow(&mut self, tick: f64) {
        let margin = (self.end() - self.start) * 0.05;
        if tick > self.end() - margin || tick < self.start {
            self.start = tick - margin;
            self.clamp();
        }
    }

    fn clamp(&mut self) {
        self.start = self.start.max(self.min_start);
    }
}

/// Eases a view from where it is to where a zoom to fit takes it, over
/// [`anim::ZOOM`](crate::theme::anim::ZOOM): the visible stretch's ends
/// glide, so what's in view now and what will be stay in sight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glide {
    from: (f64, f64),
    to: (f64, f64),
    /// When it started, in egui's time (seconds).
    started: f64,
}

impl Glide {
    pub fn new(from: &Viewport, to: &Viewport, now: f64) -> Glide {
        Glide {
            from: (from.start, from.end()),
            to: (to.start, to.end()),
            started: now,
        }
    }

    /// How far along it is at `now`, eased: 0 to 1.
    pub fn progress(&self, now: f64) -> f32 {
        let t = ((now - self.started) / f64::from(crate::theme::anim::ZOOM)).clamp(0.0, 1.0);
        crate::theme::anim::ease_out(t as f32)
    }

    /// Puts `viewport` where the glide is at `progress`.
    pub fn apply(&self, viewport: &mut Viewport, progress: f32) {
        let t = f64::from(progress);
        let start = self.from.0 + (self.to.0 - self.from.0) * t;
        let end = self.from.1 + (self.to.1 - self.from.1) * t;
        viewport.px_per_tick = (viewport.width as f64 / (end - start).max(1.0))
            .clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        viewport.start = start;
        viewport.clamp();
    }
}

/// Ctrl+wheel zoom, eased: each notch's zoom is spread over the next few
/// frames, around where the pointer was, rather than landing at once.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WheelZoom {
    /// The zoom still to come, as a natural log of its factor.
    pending: f64,
    /// The screen x it zooms around.
    anchor: f32,
}

impl WheelZoom {
    /// Most of a notch has landed after this long; the rest follows.
    const TIME_CONSTANT: f32 = crate::theme::anim::ZOOM / 4.0;
    /// Within this of the end (as a log factor, about 1%), it lands.
    const CLOSE_ENOUGH: f64 = 0.01;

    /// Adds a zoom by `factor` around screen `x`.
    pub fn add(&mut self, factor: f64, x: f32) {
        if factor > 0.0 {
            self.pending += factor.ln();
            self.anchor = x;
        }
    }

    /// Stops where it is (a zoom to fit, or another clip, takes over).
    pub fn stop(&mut self) {
        self.pending = 0.0;
    }

    /// Applies this frame's share to `viewport`, `dt` seconds after the
    /// last. True while there's more to come, so the caller asks for
    /// another frame.
    pub fn step(&mut self, viewport: &mut Viewport, dt: f32) -> bool {
        if self.pending == 0.0 {
            return false;
        }
        let share = 1.0 - (-f64::from(dt.max(0.0)) / f64::from(Self::TIME_CONSTANT)).exp();
        let mut now = self.pending * share;
        // The last sliver lands at once rather than creeping in.
        if (self.pending - now).abs() < Self::CLOSE_ENOUGH {
            now = self.pending;
        }
        self.pending -= now;
        viewport.zoom_around(self.anchor, now.exp());
        self.pending != 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Viewport {
        let mut v = Viewport::default();
        v.set_screen(100.0, 800.0);
        v
    }

    #[test]
    fn ticks_and_pixels_round_trip() {
        let mut v = viewport();
        v.start = 960.0;
        assert_eq!(v.x(960.0), 100.0);
        assert_eq!(v.x(960.0 + 3840.0), 220.0, "a bar is 120 px");
        assert_eq!(v.tick_at(v.x(5000.0)), 5000.0);
    }

    #[test]
    fn zoom_keeps_the_anchor_still() {
        let mut v = viewport();
        v.start = 3840.0;
        let x = 400.0;
        let before = v.tick_at(x);
        v.zoom_around(x, 2.0);
        assert!((v.tick_at(x) - before).abs() < 1e-6);
        v.zoom_around(x, 1e9);
        assert_eq!(v.px_per_tick, MAX_PX_PER_TICK, "zoom is limited");
    }

    #[test]
    fn cannot_scroll_before_the_start() {
        let mut v = viewport();
        v.scroll_by_px(500.0);
        assert_eq!(v.start, 0.0);
        v.scroll_by_px(-120.0);
        assert_eq!(v.start, 3840.0);
    }

    #[test]
    fn a_glide_lands_where_the_fit_goes() {
        let v = viewport();
        let to = v.fitted(7680.0, 4.0 * 3840.0);
        let glide = Glide::new(&v, &to, 10.0);
        let mut moving = v;
        glide.apply(&mut moving, glide.progress(10.0));
        assert_eq!(moving.start, v.start, "starts where it was");
        let halfway = glide.progress(10.0 + f64::from(crate::theme::anim::ZOOM) / 2.0);
        assert!(halfway > 0.5 && halfway < 1.0, "eases out: {halfway}");
        glide.apply(&mut moving, glide.progress(11.0));
        assert!((moving.start - to.start).abs() < 1e-6);
        assert!((moving.px_per_tick - to.px_per_tick).abs() < 1e-9);
    }

    #[test]
    fn wheel_zoom_eases_in_and_lands_exactly() {
        let mut v = viewport();
        let x = 500.0;
        let anchor = v.tick_at(x);
        let mut zoom = WheelZoom::default();
        zoom.add(2.0, x);
        assert!(zoom.step(&mut v, 1.0 / 60.0), "more to come");
        let first = v.px_per_tick / DEFAULT_PX_PER_TICK;
        assert!(first > 1.0 && first < 1.8, "part of the way: {first}");
        let mut frames = 1;
        while zoom.step(&mut v, 1.0 / 60.0) {
            frames += 1;
            assert!(frames < 30, "lands within half a second");
        }
        assert!(frames * 1000 / 60 <= 200, "{frames} frames");
        assert!((v.px_per_tick / DEFAULT_PX_PER_TICK - 2.0).abs() < 1e-9);
        assert!((v.tick_at(x) - anchor).abs() < 1e-6, "around the pointer");
        assert!(!zoom.step(&mut v, 1.0 / 60.0), "idle once landed");
    }

    #[test]
    fn follow_pages_when_the_playhead_leaves() {
        let mut v = viewport();
        let end = v.end();
        v.follow(end / 2.0);
        assert_eq!(v.start, 0.0, "still in view");
        v.follow(end + 100.0);
        assert!(v.start > 0.0 && v.end() > end + 100.0);
    }
}
