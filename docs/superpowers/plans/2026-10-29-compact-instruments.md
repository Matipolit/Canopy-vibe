# Compact, More Visual Instruments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every built-in instrument panel fits the device view of a 1280×800 window, in
coloured, titled sections with displays that grow into the height left; the Wavetable first.

**Architecture:** Three shared pieces (compact knob, section card, number field) and a
title-bar slot for instruments, then each panel re-laid out with them. No engine, project,
descriptor or file-format change: only `crates/ui`.

**Tech Stack:** Rust 2024, egui/eframe 0.36, egui_kittest (`ui_tests`, `Rig`).

**Spec:** `docs/superpowers/specs/2026-10-29-compact-instruments-design.md`

## Global Constraints

- Only `crates/ui` changes; sound, parameters, descriptors, presets and files are untouched.
- Sizes and colors come from `theme` (`space`, `size`, `text`, `radius`, `surface`); no literal sizes in panels.
- Every knob keeps: modulation rings, drag-and-drop modulation, right-click automation menu, keyboard focus, value entry, accessible name = its label.
- Target: instrument panel width ≤ 1040 px at 1280×800 with the detail panel at its default height.
- Comments in the codebase's prose voice (CLAUDE.md "Conventions"); tests named as sentences.
- Commit only when the user asks (CLAUDE.md overrides the skill's per-task commits).

## Review Focus

- A short detail panel (dragged to its 140 px minimum): displays shrink to their minimum and the page scrolls vertically, nothing overlaps. Test: wavetable page at 1280×600 renders without panic and every knob is still reachable by name.
- Very long names in compact knobs ("Warp Spr", "Position", automated colors): shrink then ellipsize within 46 px, never overlap neighbours. Covered by the compact-knob test's width assertion.
- Hovering a compact knob shows its value (the only place it's visible): test that hovering changes the painted text (value appears in the hover tooltip).
- Modulation drag onto a compact knob still adds a route: existing `ui_tests` modulation drag tests run against the new layout.
- The matrix hiding unused slots must still let a new route be made from it: test that the `+ Route` row's source menu fills the first free slot.

---

### Task 1: Compact knob

**Files:**
- Modify: `crates/ui/src/widgets/knob.rs`
- Modify: `crates/ui/src/devices/panel.rs` (field `compact`, used by `knob_at`)
- Modify: `crates/ui/src/devices/wavetable/knobs.rs` (`mod_knob` honours `panel.compact`)
- Test: `crates/ui/src/widgets/knob.rs` (unit), `crates/ui/src/ui_tests/device_panels.rs`

**Interfaces:**
- Produces: `Knob::compact(self, compact: bool) -> Self`; `knob::compact_width(diameter: f32) -> f32` (= `diameter + 22.0`); `knob::compact_height(diameter: f32) -> f32` (= `LABEL_HEIGHT + diameter + space::XS`); `pub const knob::COMPACT: f32 = 24.0`; `Panel::compact: bool` (default false; when true `knob_at`/`mod_knob` draw `Knob::compact(true).diameter(knob::COMPACT)`).
- Painting: the label row shows the name, or the value text while `theme::hover` > 0.5 (hovered, dragged, focused); no value row; the hover tooltip shows "name: value".

- [ ] Step 1: test `a_compact_knob_is_shorter_and_narrower` — `compact_height(24.0) < height(24.0) - 15.0` and `compact_width(24.0) == 46.0`; a kittest harness showing a compact knob allocates exactly `vec2(46, compact_height(24))`.
- [ ] Step 2: run `cargo test -p ui compact_knob` — fails (no `compact`).
- [ ] Step 3: implement as above.
- [ ] Step 4: tests pass; `cargo test -p ui ui_tests::device_panels` still passes.

### Task 2: Section card and section colors

**Files:**
- Modify: `crates/ui/src/theme.rs` (`pub mod section` colors)
- Modify: `crates/ui/src/widgets/chrome.rs`
- Test: `crates/ui/src/widgets/chrome.rs` tests or `ui_tests/device_panels.rs`

**Interfaces:**
- Produces: `theme::section::{OSC_A, OSC_B, OSC_C, FILTER, ENVELOPE, LFO, MACRO, NEUTRAL}: Color32` (envelope/LFO/macro equal to `knobs::group_color` of their groups, which then read from here).
- Produces: `chrome::section<R>(ui: &mut Ui, title: &str, color: Color32, header: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R>` — a card (faint ground, hairline border, `radius::PANEL`) with a 2 px strip of `color` along its top, a header row (`size::CONTROL` tall: title in `text::semibold(text::SMALL)` tinted by `color`, then `header`'s controls), then `body` with `space::S` inner margin. Painted behind via a reserved `Shape::Noop`.

- [ ] Step 1: test `a_section_wraps_its_body_with_a_header` — the returned rect contains the body's rect and is `size::CONTROL + ...` taller.
- [ ] Step 2–4: fail, implement, pass.

### Task 3: Instrument title-bar slot and Wavetable header

**Files:**
- Modify: `crates/ui/src/device_view.rs` (`instrument`: after the picker, the rest of the bar's rect goes to the panel)
- Modify: `crates/ui/src/devices/panel.rs` (`pub bar: Option<egui::Rect>`)
- Modify: `crates/ui/src/devices/wavetable/mod.rs` (tabs drawn in `panel.bar`; source chips on their own row)
- Modify: `crates/ui/src/devices/wavetable/knobs.rs` (`source_tile` compact chip form)
- Test: `crates/ui/src/ui_tests/` wavetable tests (existing tab clicks by name keep working)

**Interfaces:**
- Produces: `Panel::bar: Option<Rect>` — the free part of the instrument's title bar, for page tabs; `None` for effects.
- Source strip: each group starts with a caption (`ENV`, `LFO`, `NOTE`, `CTRL`), chips show the short name only (Amp, 2…6; 1…8; Vel, Key…), ~600 px total. Chips keep their accessible names (`Env 2`, `LFO 1`) so drag tests find them.

- [ ] Step 1: test `the_wavetable_tabs_sit_in_the_title_bar` — the "Matrix" tab's rect is within `size::BAR` of the instrument card's top.
- [ ] Step 2–4: fail, implement, pass; run the existing wavetable `ui_tests`.

### Task 4: Wavetable Oscillators page

**Files:**
- Modify: `crates/ui/src/devices/wavetable/oscillators.rs`, `filters.rs`, `global.rs` (`side`), `mod.rs`
- Modify: `crates/ui/src/devices/wavetable/knobs.rs` (`number_field`)
- Test: `crates/ui/src/ui_tests/` new `instrument_widths.rs`

**Interfaces:**
- Produces: `knobs::number_field(ui, panel, index: usize, label: &str) -> Response` — a drag field for a whole-number parameter (Octave, Unison) with `attach_choice`'s menu; choices (Stack) stay `choice_menu`.
- Oscillator card (section color `OSC_A/B/C`), body: warp/morph menus row; `wave_display` with height = space left minus the controls (minimum 48); fields row (Octave, Unison, Stack); four rows of four compact `mod_knob`s in the spec's order: Position, Morph, Warp, Warp 2 / Level, Pan, Semi, Fine / Detune, Blend, Width, Random / Warp Spr, Pos Spr, Phase. Load/Edit/options in a `⋯` header menu.
- Sub/Noise: one section stacked; Filter: section (`FILTER`) with display, two rows of three compact knobs, routing.
- `side`: a ~110 px `MACRO` section: compact macro knobs two per row, their drag chips in the header, the output scope (width of the column) and volume.

- [ ] Step 1: test `every_wavetable_page_fits_a_1280_window` — `Rig` at 1280×800, select the wavetable track, for each page tab the instrument card's width ≤ 1040.
- [ ] Step 2: fails on Oscillators.
- [ ] Step 3: implement.
- [ ] Step 4: passes for Oscillators; existing wavetable tests pass (`cargo test -p ui ui_tests`).

### Task 5: Wavetable Envelopes & LFOs, Matrix, Voicing pages

**Files:**
- Modify: `crates/ui/src/devices/wavetable/modulation.rs`, `matrix.rs`, `global.rs` (`page`)
- Test: `instrument_widths.rs` (all pages), matrix test

**Interfaces:**
- Envelopes & LFOs: two sections (`ENVELOPE`, `LFO`); header: chips for each env/LFO with a 24×12 thumbnail of its curve (from `EnvelopeParams` values / `lfo_display`'s shape), Sync/Retrig switches; editor fills the section; compact knob row under it.
- Matrix: rows only for used slots, then one row for the first free slot captioned `+ Route`; source as a colored chip.
- Voicing: sections Voices, Glide (curve), Expression (Bend, Velocity, Volume), Oscillators table.

- [ ] Step 1: tests `the_matrix_lists_only_routes_in_use` (default patch: rows = used slots + 1) and `a_new_route_fills_the_first_free_slot`; the width test covers all pages.
- [ ] Step 2–4: fail, implement, pass.

### Task 6: Drum Synth, Guitar, Poly Synth, Sampler

**Files:**
- Modify: `crates/ui/src/devices/drums.rs`, `guitar.rs`, `mod.rs` (Poly Synth panel), `sampler.rs`
- Test: `instrument_widths.rs` (each instrument ≤ 1040 px at 1280×800)

**Interfaces:** consumes Tasks 1–2 (`Panel::compact`, `chrome::section`, `theme::section`).
- Drum Synth: layer chips with a mini waveform each; layer controls as sections Source, Pitch envelope, Amp envelope (curve), Filter, Output, in two rows of compact knobs.
- Guitar: fretboard; sections String (Tone, Sustain, Body, Hardness, Position), Playing (Palm Mute, Strum, Direction, Humanize, Vibrato, Legato), Noises (Noises, Squeaks, Resonance), Amp (mode, Drive, Amp tone, Level); body/technique/pickup as segmented choices in a header.
- Poly Synth: Oscillator section drawing the chosen wave; Filter section with a response curve (`dsp::svf::Coefs` magnitude of the synth's filter at its cutoff/resonance); Envelope as now.
- Sampler: a key-map display (the loaded regions' key ranges over a keyboard strip); sections Amp (Volume, Pan, Velocity), Pitch (Tune), Envelope (Attack, Release, with an AR curve).

- [ ] Step 1: width tests for each instrument (fail where too wide).
- [ ] Step 2: implement each panel; tests and existing `ui_tests` pass.

### Task 7: Verify, measure, document

- [ ] `cargo test --workspace` passes; `cargo clippy -p ui --all-targets` clean.
- [ ] Tour screenshots (`CANOPY_SCREENSHOTS=target/shots cargo test --release -p ui tour`) compared with `target/baseline/shots`.
- [ ] `ui_tests/perf.rs` device view no slower than 1.61–1.67 ms CPU.
- [ ] `README.md` (instrument panels in "Using it"), `docs/progress.md` (new section 16), `CLAUDE.md` ("ui map": section cards, compact knobs).
