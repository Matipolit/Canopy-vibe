//! The wavetable editor: a window over an oscillator's table, as Serum's
//! and Vital's editors start. It opens on the table the oscillator plays,
//! as frames of 2048 samples, and edits them:
//!
//! - draw on the frame shown, freehand;
//! - pick a frame from the filmstrip of them all;
//! - set how many frames there are (spread out or thinned evenly, each new
//!   one a crossfade of its neighbours), crossfade every frame between the
//!   first and the last (a morph), normalize, remove DC;
//! - write a formula of `x` (the phase, 0 to 1) and `y` (the place in the
//!   table, 0 to 1), for the frame shown or all of them (see [`formula`]).
//!
//! Save & Use writes the table as a WAV file (frames of 2048 samples, as
//! Serum and Vital read them) to the app's wavetable folder, or Save As…
//! anywhere, and makes it the oscillator's own table. Nothing changes the
//! sound until then.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Pos2, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use instruments::builtin::wavetable as wt;

use super::super::Panel;
use super::oscillators::{OSC_NAMES, drawn_table, track_of};
use crate::theme::{self, radius, space, stroke};

/// Samples per frame, as Serum and Vital write them.
pub const FRAME_SIZE: usize = 2048;
/// Most frames a table keeps (as the synth does).
const MAX_FRAMES: usize = wt::MAX_USER_FRAMES;

/// The editor's state: which oscillator it edits, and its frames.
#[derive(Debug, Clone)]
pub(super) struct Editor {
    osc: usize,
    frames: Vec<Vec<f32>>,
    current: usize,
    formula: String,
    error: Option<String>,
    name: String,
    /// The last point drawn, for joining up a fast stroke.
    last: Option<(usize, f32)>,
    changed: bool,
}

type Shared = Arc<Mutex<Option<Editor>>>;

fn editor_id(panel: &Panel) -> egui::Id {
    panel.id("table editor")
}

fn shared(ctx: &egui::Context, id: egui::Id) -> Shared {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Shared>(id).clone())
}

/// Opens the editor on oscillator `osc`'s table as it plays it now.
pub(super) fn open(ui: &Ui, panel: &Panel, osc: usize) {
    let (drawn, label, _) = drawn_table(panel, osc, 0.0);
    let source = drawn.source();
    let count = source.frames().max(1);
    let frames: Vec<Vec<f32>> = (0..count)
        .map(|f| {
            let position = if count > 1 {
                f as f32 / (count - 1) as f32
            } else {
                0.0
            };
            let mut frame = vec![0.0; FRAME_SIZE];
            source.draw(
                position,
                (wt::MORPH_OFF, 0.0, 0.0),
                [(wt::WARP_OFF, 0.0); 2],
                &mut frame,
            );
            frame
        })
        .collect();
    let name = label
        .split(" · ")
        .next()
        .unwrap_or("Table")
        .trim_end_matches(".wav")
        .to_owned();
    let editor = Editor {
        osc,
        frames,
        current: 0,
        formula: "sin(tau * x) * (1 - y) + saw(x) * y".to_owned(),
        error: None,
        name: format!("{name} edit"),
        last: None,
        changed: false,
    };
    *shared(ui.ctx(), editor_id(panel)).lock().unwrap() = Some(editor);
}

/// The editor's window, while it's open. Call once a frame.
pub(super) fn window(ctx: &egui::Context, panel: &mut Panel) {
    let state = shared(ctx, editor_id(panel));
    let mut guard = state.lock().unwrap();
    let Some(editor) = guard.as_mut() else {
        return;
    };
    let mut open = true;
    let mut save = None;
    egui::Window::new(format!("Osc {} wavetable", OSC_NAMES[editor.osc]))
        .id(editor_id(panel).with("window"))
        .open(&mut open)
        .default_size(vec2(620.0, 330.0))
        .collapsible(false)
        .show(ctx, |ui| {
            save = contents(ui, editor);
        });
    if let Some(path) = save {
        match save_table(&editor.frames, &path) {
            Ok(()) => {
                if let Some(track) = track_of(panel) {
                    panel
                        .session
                        .load_wavetable(track, editor.osc, path.clone());
                }
                panel.session.info(format!("Saved {}", path.display()));
                editor.changed = false;
            }
            Err(err) => panel
                .session
                .error(format!("Couldn't save the table: {err}")),
        }
    }
    if !open {
        *guard = None;
    }
}

/// Where Save & Use puts tables: the app's data folder.
fn table_folder() -> Option<PathBuf> {
    eframe::storage_dir("canopy").map(|dir| dir.join("wavetables"))
}

/// `name` without characters file systems dislike.
fn file_safe(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim().to_owned();
    if safe.is_empty() {
        "table".to_owned()
    } else {
        safe
    }
}

/// Writes `frames` as a WAV wavetable at `path`: the frames one after the
/// other in 32-bit float, both channels the same.
pub(super) fn save_table(frames: &[Vec<f32>], path: &Path) -> std::io::Result<()> {
    let samples: Vec<f32> = frames.iter().flatten().copied().collect();
    let audio = media::AudioBuffer::new(48_000, samples.clone(), samples);
    media::write_wav(path, &audio)
}

/// The window's insides; returns where to save, if asked to.
fn contents(ui: &mut Ui, editor: &mut Editor) -> Option<PathBuf> {
    let mut save = None;
    let count = editor.frames.len();
    editor.current = editor.current.min(count - 1);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&editor.name).strong());
        ui.separator();
        ui.weak("Frame");
        let mut shown = editor.current + 1;
        ui.add(egui::DragValue::new(&mut shown).range(1..=count));
        editor.current = shown.clamp(1, count) - 1;
        ui.weak(format!("of {count}"));
        ui.separator();
        let mut frames = count;
        ui.weak("Frames");
        if ui
            .add(egui::DragValue::new(&mut frames).range(1..=MAX_FRAMES))
            .on_hover_text("Spread the table over this many frames")
            .changed()
        {
            editor.frames = resize(&editor.frames, frames);
            editor.changed = true;
        }
    });
    let size = vec2(ui.available_width().max(300.0), 150.0);
    draw_frame(ui, editor, size);
    filmstrip(ui, editor);
    ui.horizontal(|ui| {
        if ui
            .button("Crossfade First to Last")
            .on_hover_text("Every frame between becomes a blend of the first and last: a morph")
            .clicked()
        {
            morph(&mut editor.frames);
            editor.changed = true;
        }
        if ui.button("Normalize").clicked() {
            normalize(&mut editor.frames);
            editor.changed = true;
        }
        if ui.button("Remove DC").clicked() {
            for frame in &mut editor.frames {
                let mean = frame.iter().sum::<f32>() / frame.len() as f32;
                frame.iter_mut().for_each(|x| *x -= mean);
            }
            editor.changed = true;
        }
        if ui.button("Duplicate Frame").clicked() && count < MAX_FRAMES {
            let copy = editor.frames[editor.current].clone();
            editor.frames.insert(editor.current + 1, copy);
            editor.current += 1;
            editor.changed = true;
        }
        if ui
            .add_enabled(count > 1, egui::Button::new("Remove Frame"))
            .clicked()
        {
            editor.frames.remove(editor.current);
            editor.current = editor.current.min(editor.frames.len() - 1);
            editor.changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.weak("Formula");
        let field = ui.add(
            egui::TextEdit::singleline(&mut editor.formula)
                .desired_width(300.0)
                .font(egui::TextStyle::Monospace),
        );
        let field = field.on_hover_text(
            "x: phase 0..1, y: place in the table 0..1, n: frame number; + - * / ^, \
             sin cos tan abs sqrt exp ln pow min max floor ceil round sign clamp lerp, \
             saw tri sqr pulse(x, width) noise; pi, tau",
        );
        let all = ui.button("All Frames").clicked();
        let one = ui.button("This Frame").clicked();
        if all || one || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
            match formula::parse(&editor.formula) {
                Ok(expression) => {
                    let count = editor.frames.len();
                    let which: Vec<usize> = if one {
                        vec![editor.current]
                    } else {
                        (0..count).collect()
                    };
                    for f in which {
                        let y = if count > 1 {
                            f as f32 / (count - 1) as f32
                        } else {
                            0.0
                        };
                        for (i, sample) in editor.frames[f].iter_mut().enumerate() {
                            let x = i as f32 / FRAME_SIZE as f32;
                            let value = expression.eval(x, y, f as f32);
                            *sample = if value.is_finite() {
                                value.clamp(-4.0, 4.0)
                            } else {
                                0.0
                            };
                        }
                    }
                    editor.error = None;
                    editor.changed = true;
                }
                Err(err) => editor.error = Some(err),
            }
        }
        if let Some(err) = &editor.error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        ui.weak("Name");
        ui.add(egui::TextEdit::singleline(&mut editor.name).desired_width(160.0));
        if ui
            .button("Save & Use")
            .on_hover_text("Save to canopy's wavetable folder and play it on this oscillator")
            .clicked()
            && let Some(folder) = table_folder()
        {
            save = Some(folder.join(format!("{}.wav", file_safe(&editor.name))));
        }
        if ui.button("Save As…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Wavetable (WAV)", &["wav"])
                .set_file_name(format!("{}.wav", file_safe(&editor.name)))
                .save_file()
        {
            save = Some(path);
        }
        if editor.changed {
            ui.weak("Not saved: the oscillator plays the table it had");
        }
    });
    save
}

/// The frame shown, large; drag across it to draw.
fn draw_frame(ui: &mut Ui, editor: &mut Editor, size: egui::Vec2) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    #[cfg(test)]
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new("table editor frame"), rect));
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, radius::PANEL, visuals.extreme_bg_color);
    let area = rect.shrink2(vec2(space::S + space::XS, space::M));
    let y_of = |v: f32| area.center().y - v.clamp(-1.2, 1.2) * area.height() / 2.4;
    painter.hline(
        area.x_range(),
        area.center().y,
        Stroke::new(
            stroke::HAIRLINE,
            visuals.weak_text_color().gamma_multiply(0.3),
        ),
    );
    for v in [-1.0, 1.0] {
        painter.hline(
            area.x_range(),
            y_of(v),
            Stroke::new(
                stroke::HAIRLINE,
                visuals.weak_text_color().gamma_multiply(0.12),
            ),
        );
    }
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let i = (((pos.x - area.left()) / area.width()) * FRAME_SIZE as f32)
            .clamp(0.0, (FRAME_SIZE - 1) as f32) as usize;
        let value = ((area.center().y - pos.y) / (area.height() / 2.4)).clamp(-1.0, 1.0);
        let (from, from_value) = editor
            .last
            .filter(|_| !response.drag_started() && !response.clicked())
            .unwrap_or((i, value));
        let frame = &mut editor.frames[editor.current];
        let (low, high) = (from.min(i), from.max(i));
        for (k, sample) in frame.iter_mut().enumerate().take(high + 1).skip(low) {
            let t = if from == i {
                1.0
            } else {
                (k as f32 - from as f32) / (i as f32 - from as f32)
            };
            *sample = from_value + (value - from_value) * t;
        }
        editor.last = Some((i, value));
        editor.changed = true;
    }
    if response.drag_stopped() {
        editor.last = None;
    }
    let frame = &editor.frames[editor.current];
    let points: Vec<Pos2> = (0..=400)
        .map(|s| {
            let i = (s * (FRAME_SIZE - 1)) / 400;
            pos2(
                area.left() + area.width() * s as f32 / 400.0,
                y_of(frame[i]),
            )
        })
        .collect();
    painter.add(Shape::line(
        points,
        Stroke::new(1.6, theme::accent(&visuals)),
    ));
    response.on_hover_text("Drag to draw the frame");
}

/// Every frame, small, in a row: click one to show it.
fn filmstrip(ui: &mut Ui, editor: &mut Editor) {
    egui::ScrollArea::horizontal()
        .id_salt("table editor filmstrip")
        .max_height(34.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                for f in 0..editor.frames.len() {
                    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 26.0), Sense::click());
                    let visuals = ui.visuals();
                    let selected = f == editor.current;
                    let painter = ui.painter_at(rect);
                    let accent = theme::accent(visuals);
                    painter.rect_filled(rect, radius::SMALL, visuals.extreme_bg_color);
                    if selected {
                        painter.rect_stroke(
                            rect,
                            radius::SMALL,
                            Stroke::new(stroke::EMPHASIS, accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let frame = &editor.frames[f];
                    let inner = rect.shrink(space::S);
                    let points: Vec<Pos2> = (0..=24)
                        .map(|s| {
                            let i = (s * (FRAME_SIZE - 1)) / 24;
                            pos2(
                                inner.left() + inner.width() * s as f32 / 24.0,
                                inner.center().y - frame[i].clamp(-1.0, 1.0) * inner.height() / 2.0,
                            )
                        })
                        .collect();
                    let color = if selected {
                        accent
                    } else {
                        visuals.weak_text_color()
                    };
                    painter.add(Shape::line(points, Stroke::new(stroke::HAIRLINE, color)));
                    if response.clicked() {
                        editor.current = f;
                    }
                }
            });
        });
}

/// `frames` spread over (or thinned to) `count` frames, each new one a
/// crossfade of the two nearest.
pub(super) fn resize(frames: &[Vec<f32>], count: usize) -> Vec<Vec<f32>> {
    let count = count.clamp(1, MAX_FRAMES);
    if frames.len() == 1 || count == 1 {
        return vec![frames[0].clone(); count];
    }
    (0..count)
        .map(|f| {
            let place = f as f32 / (count - 1) as f32 * (frames.len() - 1) as f32;
            let i = (place as usize).min(frames.len() - 1);
            let j = (i + 1).min(frames.len() - 1);
            let t = place - i as f32;
            frames[i]
                .iter()
                .zip(&frames[j])
                .map(|(a, b)| a + (b - a) * t)
                .collect()
        })
        .collect()
}

/// Every frame between the first and last made a crossfade of the two.
pub(super) fn morph(frames: &mut [Vec<f32>]) {
    let count = frames.len();
    if count < 3 {
        return;
    }
    let (first, last) = (frames[0].clone(), frames[count - 1].clone());
    for (f, frame) in frames.iter_mut().enumerate().take(count - 1).skip(1) {
        let t = f as f32 / (count - 1) as f32;
        for ((x, a), b) in frame.iter_mut().zip(&first).zip(&last) {
            *x = a + (b - a) * t;
        }
    }
}

/// Every frame scaled so the table's loudest sample is at full scale.
pub(super) fn normalize(frames: &mut [Vec<f32>]) {
    let peak = frames.iter().flatten().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 1e-6 {
        frames.iter_mut().flatten().for_each(|x| *x /= peak);
    }
}

/// Wavetable formulas: an expression of `x` (the phase), `y` (the place in
/// the table) and `n` (the frame's number), parsed once and evaluated per
/// sample.
pub(super) mod formula {
    use std::f32::consts::{PI, TAU};

    /// A parsed formula.
    #[derive(Debug, Clone)]
    pub enum Expression {
        Number(f32),
        X,
        Y,
        N,
        Negate(Box<Expression>),
        Binary(char, Box<Expression>, Box<Expression>),
        Call(&'static str, Vec<Expression>),
    }

    /// The functions a formula may call, with how many arguments each takes.
    const FUNCTIONS: &[(&str, usize)] = &[
        ("sin", 1),
        ("cos", 1),
        ("tan", 1),
        ("abs", 1),
        ("sqrt", 1),
        ("exp", 1),
        ("ln", 1),
        ("log", 1),
        ("floor", 1),
        ("ceil", 1),
        ("round", 1),
        ("sign", 1),
        ("saw", 1),
        ("tri", 1),
        ("sqr", 1),
        ("noise", 1),
        ("pow", 2),
        ("min", 2),
        ("max", 2),
        ("pulse", 2),
        ("clamp", 3),
        ("lerp", 3),
    ];

    impl Expression {
        pub fn eval(&self, x: f32, y: f32, n: f32) -> f32 {
            match self {
                Expression::Number(v) => *v,
                Expression::X => x,
                Expression::Y => y,
                Expression::N => n,
                Expression::Negate(e) => -e.eval(x, y, n),
                Expression::Binary(op, a, b) => {
                    let (a, b) = (a.eval(x, y, n), b.eval(x, y, n));
                    match op {
                        '+' => a + b,
                        '-' => a - b,
                        '*' => a * b,
                        '/' => a / b,
                        '%' => a.rem_euclid(b),
                        _ => a.powf(b),
                    }
                }
                Expression::Call(name, args) => {
                    let a: Vec<f32> = args.iter().map(|e| e.eval(x, y, n)).collect();
                    let phase = |p: f32| p.rem_euclid(1.0);
                    match *name {
                        "sin" => a[0].sin(),
                        "cos" => a[0].cos(),
                        "tan" => a[0].tan(),
                        "abs" => a[0].abs(),
                        "sqrt" => a[0].sqrt(),
                        "exp" => a[0].exp(),
                        "ln" | "log" => a[0].ln(),
                        "floor" => a[0].floor(),
                        "ceil" => a[0].ceil(),
                        "round" => a[0].round(),
                        "sign" => a[0].signum(),
                        "saw" => 2.0 * phase(a[0]) - 1.0,
                        "tri" => 1.0 - 4.0 * (phase(a[0]) - 0.5).abs(),
                        "sqr" => {
                            if phase(a[0]) < 0.5 {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        "noise" => {
                            // A repeatable value per sample position.
                            let mut h = (a[0] * 65_536.0) as i64 as u32 ^ 0x9E37_79B9;
                            h ^= h >> 15;
                            h = h.wrapping_mul(0x2C1B_3C6D);
                            h ^= h >> 12;
                            (h >> 8) as f32 / (1 << 23) as f32 - 1.0
                        }
                        "pow" => a[0].powf(a[1]),
                        "min" => a[0].min(a[1]),
                        "max" => a[0].max(a[1]),
                        "pulse" => {
                            if phase(a[0]) < a[1] {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        "clamp" => a[0].clamp(a[1].min(a[2]), a[2].max(a[1])),
                        _ => a[0] + (a[1] - a[0]) * a[2],
                    }
                }
            }
        }
    }

    struct Parser<'a> {
        text: &'a [u8],
        at: usize,
    }

    /// Reads `text` as a formula, or says what's wrong with it.
    pub fn parse(text: &str) -> Result<Expression, String> {
        let mut parser = Parser {
            text: text.as_bytes(),
            at: 0,
        };
        let expression = parser.sum()?;
        parser.space();
        if parser.at < parser.text.len() {
            return Err(format!("didn't expect \"{}\"", &text[parser.at..]));
        }
        Ok(expression)
    }

    impl Parser<'_> {
        fn space(&mut self) {
            while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
        }

        fn peek(&mut self) -> Option<u8> {
            self.space();
            self.text.get(self.at).copied()
        }

        fn sum(&mut self) -> Result<Expression, String> {
            let mut left = self.product()?;
            while let Some(op @ (b'+' | b'-')) = self.peek() {
                self.at += 1;
                let right = self.product()?;
                left = Expression::Binary(op as char, Box::new(left), Box::new(right));
            }
            Ok(left)
        }

        fn product(&mut self) -> Result<Expression, String> {
            let mut left = self.power()?;
            while let Some(op @ (b'*' | b'/' | b'%')) = self.peek() {
                self.at += 1;
                let right = self.power()?;
                left = Expression::Binary(op as char, Box::new(left), Box::new(right));
            }
            Ok(left)
        }

        fn power(&mut self) -> Result<Expression, String> {
            let base = self.unary()?;
            if self.peek() == Some(b'^') {
                self.at += 1;
                // Right-associative: 2^3^2 is 2^9.
                let exponent = self.power()?;
                return Ok(Expression::Binary('^', Box::new(base), Box::new(exponent)));
            }
            Ok(base)
        }

        fn unary(&mut self) -> Result<Expression, String> {
            match self.peek() {
                Some(b'-') => {
                    self.at += 1;
                    Ok(Expression::Negate(Box::new(self.unary()?)))
                }
                Some(b'+') => {
                    self.at += 1;
                    self.unary()
                }
                _ => self.primary(),
            }
        }

        fn primary(&mut self) -> Result<Expression, String> {
            match self.peek() {
                Some(b'(') => {
                    self.at += 1;
                    let inner = self.sum()?;
                    if self.peek() != Some(b')') {
                        return Err("a \"(\" isn't closed".to_owned());
                    }
                    self.at += 1;
                    Ok(inner)
                }
                Some(c) if c.is_ascii_digit() || c == b'.' => {
                    let start = self.at;
                    while self.at < self.text.len()
                        && (self.text[self.at].is_ascii_digit() || self.text[self.at] == b'.')
                    {
                        self.at += 1;
                    }
                    let digits = std::str::from_utf8(&self.text[start..self.at]).unwrap_or("");
                    digits
                        .parse()
                        .map(Expression::Number)
                        .map_err(|_| format!("\"{digits}\" isn't a number"))
                }
                Some(c) if c.is_ascii_alphabetic() => {
                    let start = self.at;
                    while self.at < self.text.len() && self.text[self.at].is_ascii_alphanumeric() {
                        self.at += 1;
                    }
                    let name = std::str::from_utf8(&self.text[start..self.at])
                        .unwrap_or("")
                        .to_lowercase();
                    match name.as_str() {
                        "x" => return Ok(Expression::X),
                        "y" => return Ok(Expression::Y),
                        "n" => return Ok(Expression::N),
                        "pi" => return Ok(Expression::Number(PI)),
                        "tau" => return Ok(Expression::Number(TAU)),
                        "e" => return Ok(Expression::Number(std::f32::consts::E)),
                        _ => {}
                    }
                    let Some(&(function, arity)) = FUNCTIONS.iter().find(|(f, _)| *f == name)
                    else {
                        return Err(format!("no such name as \"{name}\""));
                    };
                    if self.peek() != Some(b'(') {
                        return Err(format!("{function} needs ( ) after it"));
                    }
                    self.at += 1;
                    let mut args = vec![self.sum()?];
                    while self.peek() == Some(b',') {
                        self.at += 1;
                        args.push(self.sum()?);
                    }
                    if self.peek() != Some(b')') {
                        return Err(format!("{function}'s ( isn't closed"));
                    }
                    self.at += 1;
                    if args.len() != arity {
                        return Err(format!("{function} takes {arity}, not {}", args.len()));
                    }
                    Ok(Expression::Call(function, args))
                }
                Some(c) => Err(format!("didn't expect \"{}\"", c as char)),
                None => Err("it ends too soon".to_owned()),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn eval(text: &str, x: f32, y: f32) -> f32 {
            parse(text).unwrap().eval(x, y, 0.0)
        }

        #[test]
        fn formulas_read_as_written() {
            assert_eq!(eval("1 + 2 * 3", 0.0, 0.0), 7.0);
            assert_eq!(eval("(1 + 2) * 3", 0.0, 0.0), 9.0);
            assert_eq!(eval("2 ^ 3 ^ 2", 0.0, 0.0), 512.0);
            assert_eq!(eval("-x + y", 0.25, 1.0), 0.75);
            assert!((eval("sin(tau * x)", 0.25, 0.0) - 1.0).abs() < 1e-6);
            assert_eq!(eval("saw(x)", 0.75, 0.0), 0.5);
            assert_eq!(eval("lerp(saw(x), sqr(x), y)", 0.25, 1.0), 1.0);
            assert_eq!(eval("pulse(x, 0.2)", 0.3, 0.0), -1.0);
            assert_eq!(eval("clamp(3, -1, 1)", 0.0, 0.0), 1.0);
            assert!(parse("sin(").is_err());
            assert!(parse("foo(x)").is_err());
            assert!(parse("pow(x)").is_err());
            assert!(parse("1 +").is_err());
            assert!(parse("x y").is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_spread_morph_and_normalize() {
        let a = vec![0.0; 4];
        let b = vec![1.0; 4];
        let spread = resize(&[a.clone(), b.clone()], 5);
        assert_eq!(spread.len(), 5);
        assert_eq!(spread[2], vec![0.5; 4]);
        let mut frames = vec![a, vec![9.0; 4], vec![9.0; 4], b];
        morph(&mut frames);
        assert!((frames[1][0] - 1.0 / 3.0).abs() < 1e-6);
        normalize(&mut frames);
        assert_eq!(frames[3][0], 1.0);
        assert_eq!(resize(&frames, 1).len(), 1);
    }

    #[test]
    fn a_saved_table_reads_back_as_its_frames() {
        let dir = std::env::temp_dir().join(format!("canopy-table-editor-{}", std::process::id()));
        let path = dir.join("saw.wav");
        let frames: Vec<Vec<f32>> = (0..3)
            .map(|f| {
                (0..FRAME_SIZE)
                    .map(|i| (i as f32 / FRAME_SIZE as f32) * (f + 1) as f32 / 3.0)
                    .collect()
            })
            .collect();
        save_table(&frames, &path).unwrap();
        let decode = |p: &Path| -> Result<Vec<f32>, String> {
            let (audio, _) = media::decode(p).map_err(|e| e.to_string())?;
            Ok(audio.left().to_vec())
        };
        let table = wt::load_user_table(&path, &decode).unwrap();
        assert_eq!(table.frames(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn names_are_safe_for_files() {
        assert_eq!(file_safe("a/b: c"), "a_b_ c");
        assert_eq!(file_safe("  "), "table");
    }
}
