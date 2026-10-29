# Per-note Bends and the Slicer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Notes carry drawn pitch curves that every built-in instrument follows per voice, and a new Slicer instrument chops a tempo-matched (Resample or Stretch) loop into slices played from the keyboard.

**Architecture:** Bends are data on `project::Note`, compiled by the engine into a new `NoteEventKind::NoteBend` event per 64-frame block, which instruments apply per voice. The Slicer is a built-in instrument whose file and slice markers live on the MIDI track; the UI builds its tempo-matched audio and slice table off the audio thread and hands it over as `InstrumentData`, as the SFZ Sampler's bank is.

**Tech Stack:** Rust 2024 workspace (`midi`, `project`, `engine`, `instruments`, `media`, `plugins`, `hosting`, `canopy-clap`, `ui`), egui 0.36, egui_kittest.

**Spec:** `docs/superpowers/specs/2026-10-29-note-bends-and-slicer-design.md`

## Global Constraints

- No allocation or freeing on the audio thread or its workers (`assert_no_alloc`; `*/tests/realtime*.rs`).
- Parallel rendering bit-identical to single-threaded (`engine/tests/parallel.rs`).
- Files without bends or slices re-save byte-identically; new fields use `#[serde(default, skip_serializing_if = ...)]`.
- Parameter order is file format: append descriptors, never reorder.
- `engine/examples/golden`: every existing device's hashes unchanged.
- Comments in the codebase's prose voice; tests named as sentences; README "Using it" and `docs/progress.md` updated.
- Agents: at most 2–3 at once, each `CARGO_TARGET_DIR=target/<agent>`; never build in /tmp.

## Review Focus

- Two overlapping notes of the same pitch, one bent: the bend reaches the voice of that pitch (as Note Off does); nothing panics. Test in Task 2.
- A bend whose points lie past the note's end (after shortening): those points are not played and the note still ends cleanly. Test in Task 2.
- A Slicer with no file, or a file that fails to decode: silent, panel says so, no panic. Test in Task 7/8.
- Changing the project tempo while a Stretch render is in flight: the old render keeps playing, the new one replaces it, no stale-render overwrite. Test in Task 8.
- More than 64 transients: slicing stops at 64 and keys past 127 are never produced. Test in Task 6.

---

### Task 1: Bend data on notes (`crates/project`)

**Files:** `project/src/note.rs`, `project/src/note_tools.rs`, `project/src/edit.rs`, every `Note { .. }` literal in the workspace (add `bend: Vec::new()`), `project/tests/` round trip.

**Interfaces — Produces:**
- `pub struct BendPoint { pub at: Tick, pub semitones: f32, pub curve: f32 }` (`Debug, Clone, Copy, PartialEq, Serialize, Deserialize`), `BendPoint::MAX_SEMITONES = 48.0`.
- `Note.bend: Vec<BendPoint>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- `Note::bend_at(&self, at: Tick) -> f32` (0 before the first point, holds after the last, segments shaped by `curve` with the envelope curve law).
- `Project::set_note_bend(&mut self, clip: ClipId, note: NoteId, points: Vec<BendPoint>) -> EditResult` (sorts by `at`, clamps).
- note tools: `chop` splits bends (second part starts with a point at the cut value), `glue` joins them, `reverse` mirrors them, `flip` negates them.

- [ ] Tests: `a_note_without_a_bend_saves_as_before`, `a_bend_survives_save_and_load`, `bend_at_follows_the_points_and_holds_the_last`, `chopping_a_bent_note_splits_its_bend`, `gluing_joins_bends`, `reversing_mirrors_a_bend`, `flipping_negates_a_bend`, `set_note_bend_sorts_and_clamps`.
- [ ] Run `cargo test -p project` — fail, implement, pass; `cargo build --workspace --all-targets` compiles.

### Task 2: The NoteBend event and the compiler (`crates/midi`, `crates/engine`)

**Interfaces — Consumes:** Task 1. **Produces:** `NoteEventKind::NoteBend { pitch: Pitch, cents: i16 }` declared after `On`; `midi::BEND_STEP_FRAMES = 64`.

- Compiler (`arrangement.rs::midi_events`): for each audible bent note, one event at the note's start (if non-zero) and at every 64-frame boundary inside it where the rounded cents change; nothing past the note's end; capacity reserved up front.
- Every `match` on `NoteEventKind` across the workspace handles the new arm (instruments ignore it until Task 3; plugin bridges until Task 4).

- [ ] Tests (`engine/tests/`): `a_bent_note_compiles_to_bend_events_inside_it`, `bend_points_past_the_end_are_not_played`, `a_bend_follows_its_pitch_when_two_notes_overlap`; `realtime_safety` and `parallel` with a bent note.

### Task 3: Built-in instruments follow bends (`crates/instruments`)

**Consumes:** Task 2. Each instrument finds the voice(s) playing `pitch` and sets its bend, ramped within the block: Poly Synth, Simple Osc, Wavetable (every oscillator and the sub of that voice), SFZ Sampler (rate), Drum Synth (pitch of the hit), Guitar (string loop delay).

- [ ] Test per instrument (`instruments/tests/bends.rs`): a held note bent +12 semitones measures an octave up (FFT peak or zero crossings) and a chord's other note is unmoved. Golden diff empty.

### Task 4: Plugins and canopy-clap

- [ ] `plugins`: CLAP hosts send `clap_event_note_expression` with `CLAP_NOTE_EXPRESSION_TUNING` (value in semitones, `note_id`/key from the note); VST3 sends a tuning `NoteExpressionValueEvent` where supported; VST2 ignores. Unit tests on the event translation.
- [ ] `canopy-clap`: incoming tuning note expressions become `NoteBend`. Test in its bridge tests.

### Task 5: Piano roll Bend mode (`crates/ui`)

- Toolbar toggle "Bend" and key `B`; in Bend mode, a drag on a note adds/moves points along the drag (snap to semitone and grid; Shift free pitch, Alt free time); drag a point moves it; Alt-drag a segment curves it; double-click a point removes it; the gesture commits one `set_note_bend`. Note menu: Clear Bend. Bent notes draw their path.
- [ ] ui_tests (`ui_tests/piano_tools.rs` or new `bends.rs`): `draw_a_bend_on_a_note`, `clear_a_notes_bend`, `bend_mode_toggles_with_b`; README Shortcuts/Using it.

### Task 6: Slicer data (`crates/project`)

**Produces:** `TrackKind::Midi.slices: Vec<u64>` (`serde(default, skip_serializing_if = "Vec::is_empty")`), `Track::slices(&self) -> &[u64]`, `Project::set_slices(&mut self, track: TrackId, starts: Vec<u64>) -> EditResult` (sorted, deduped, first 0, at most `MAX_SLICES = 64`); `project::slicer::guess_bars(frames: u64, rate: u32) -> (u32, f64)` (bars, bpm).

- [ ] Tests: `slices_save_only_when_set`, `set_slices_sorts_dedups_and_caps_at_64`, `slices_stay_when_the_instrument_changes`, `the_tempo_guess_lands_between_70_and_180`.

### Task 7: The Slicer instrument (`crates/instruments/src/builtin/slicer.rs`)

**Produces:** `canopy.slicer` in `builtins()`; `pub struct SliceData { pub audio: Arc<AudioBuffer-like stereo f32>, pub starts: Box<[usize]>, pub speed: f64 }` passed via `set_data`; params per spec (globals then 4 × 64 per-slice, appended).

- [ ] Tests (`instruments/tests/slicer.rs`): `slice_k_plays_on_key_root_plus_k`, `resample_at_double_speed_is_an_octave_up`, `cut_stops_the_previous_slice`, `reverse_plays_a_slice_backwards`, `a_bend_moves_a_slices_pitch`, `no_data_is_silent`; realtime-safety.

### Task 8: Slicer data on the UI side (`crates/ui`)

- `Session` loads the slicer's file on a worker (decode via `media`), guesses the tempo, auto-slices at transients when the track has no slices, renders Stretch mode with `media::process` (generation-stamped so a stale render never replaces a newer one), and answers `instrument_data` with `SliceData`.
- [ ] Tests: `a_stale_stretch_render_never_replaces_a_newer_one` (unit), ui test loading a WAV yields slices and sound.

### Task 9: Slicer panel, slicing, Slices to Piano Roll, drops (`crates/ui`)

- Panel per spec (cards; waveform with markers; tempo, slicing, selected slice, amp). Slicing actions: Transients(sensitivity), Grid(1/4..1/32), by hand. Slices to Piano Roll. Drop on waveform loads; Alt-drop on timeline makes a slicer track.
- [ ] ui_tests: `slice_a_break_by_grid_and_play_it`, `add_move_and_remove_a_marker`, `slices_to_piano_roll_writes_the_loop`, `alt_drop_makes_a_slicer_track`; the slicer in `instrument_widths` (fits 1280×800, reachable when short).

### Task 10: Verify, document

- [ ] `cargo test --workspace`; clippy clean on touched crates; golden diff only adds the slicer; README (Using it, Shortcuts), `docs/progress.md` (section 17), `CLAUDE.md` (crates table / ui map) updated.

## Execution

Tasks 1–2 first (shared foundations), committed. Then in parallel, each in its own git worktree off that commit: agent A — Tasks 3–4; agent B — Tasks 6–9; the main session — Task 5. Merge, then Task 10 and a whole-branch review.
