# Per-note bends and the Slicer — design

Date: 29 October 2026. Approved by the user in conversation: per-note bend curves;
slicing by transients, grid and hand; extras: slices to piano roll, cut by default,
per-slice settings, drop a file to start. Bends are built first, then the slicer.

## 1. Per-note bend curves

### Goal

A note can carry its own pitch curve, drawn in the piano roll, and every built-in
instrument follows it per voice, so each note of a chord bends on its own.

### Data (`crates/project`)

- `Note.bend: Vec<BendPoint>`, empty for none; `BendPoint { at: Tick, semitones: f32,
  curve: f32 }`: `at` from the note's start, `semitones` clamped to ±48, `curve` in
  `-1..=1` bends the segment leading into the point (the same curve as envelope stages,
  `dsp::curve_coefficients`). Before the first point the bend is 0 (the note's own
  pitch), after the last it holds the last value.
- Serialized with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`: notes
  without bends, and so every existing file, re-save byte-identically (architecture
  rule 9). Projects using bends write format 1 too: older builds ignore the field.
- `Note::bend_at(tick_in_note) -> f32` evaluates the curve.
- Note tools keep bends sensible: moving, transposing, copying, quantizing and
  velocity edits carry the bend unchanged; splitting a note gives each part the
  points in its span (the second part's times shifted, starting with a point at the
  value where it was cut); changing a note's length keeps the points (those past the
  end are not played); glue appends the second note's points after the first's, with
  a point at the join; reverse mirrors the points in time; flip negates them.
- `Project::set_note_bend(clip, note, points)` sets one note's bend (one undo step).

### Engine (`crates/midi`, `crates/engine`)

- `NoteEventKind::NoteBend { pitch: Pitch, cents: i16 }`: the sounding note of
  `pitch` is now `cents` from its own pitch. Declared after `On` so a bend at a note's
  start sorts after its Note On.
- The compiler turns each bent note into `NoteBend` events: one at every 64-frame
  block boundary inside the note where the rounded value changes (and one at the
  start if the first value is not 0). No allocation on the audio thread; parallel
  output stays bit-identical; the event cap per block is respected (bends past it
  are dropped before notes are).
- Live MIDI input is unchanged (MPE input is not part of this).

### Instruments

Each built-in instrument looks up the voice(s) playing `pitch` and sets a per-voice
bend in semitones, reached smoothly within the block (no zipper noise), added to
whatever pitch the voice already has (glide, pitch wheel, modulation):

- Poly Synth, Simple Osc, Wavetable (per voice, every oscillator and the sub), SFZ
  Sampler (playback rate), Drum Synth (the hit's pitch), Guitar (the string's loop
  delay, so it sounds like a string bend), Slicer (playback rate).
- Plugins: CLAP gets `CLAP_NOTE_EXPRESSION_TUNING` (semitones) for the note; VST3 gets
  a tuning note-expression value where the plugin supports note expressions; VST2
  has no per-note pitch and ignores bends.
- `canopy-clap` (the built-ins as a CLAP plugin) turns incoming tuning note
  expressions into `NoteBend`.

### Piano roll (`crates/ui`)

- A Bend mode: a toggle in the piano roll's toolbar and the `B` key. In it, dragging
  on a note draws its pitch path in the roll itself, starting from the note's own
  row: each drag adds or moves points; points snap to semitones (Shift: free, to the
  cent) and to the grid in time (Alt: free). Drag a point to move it; Alt-drag a
  segment to curve it; double-click a point to delete it. The whole gesture is one
  `set_note_bend`/`set_notes` edit, as every piano-roll gesture is.
- The note's right-click menu has Clear Bend.
- Outside Bend mode a bent note shows its path as a thin line from its start.

## 2. The Slicer (`canopy.slicer`, "Slicer")

### Goal

FL Studio Slicex's core for chopping breaks: load a loop, tempo-match it to the
project (Resample or Stretch, as audio clips do) before slicing it, slice it, and play
slices from the keyboard.

### Data

- The audio file: the MIDI track's existing `file` (as the SFZ Sampler's .sfz).
- `TrackKind::Midi.slices: Vec<u64>`: slice starts in the original file's frames,
  sorted, the first at 0; `#[serde(default, skip_serializing_if = "Vec::is_empty")]`,
  kept when the instrument changes (like `file`). Edited by
  `Project::set_slices(track, starts)` (one undo step; sorts, dedups, clamps to 64).
- Parameters (static descriptors): Mode (Resample / Stretch / Off), Original Tempo
  (BPM, 40–300), Follow Tempo (switch, default on), Root Key (default C5 = MIDI 60),
  Cut (switch, default on), Volume, Pitch (semitones), Attack, Decay, Sustain,
  Release; and per slice (64 of each, appended in blocks so ids stay stable):
  Reverse, Pitch, Level, Pan.

### Tempo matching (before slicing)

- The speed is `project::Stretch { mode, original_bpm, sync, speed: 1, transpose: 0 }`
  `.effective_speed(tempo)`, as for audio clips; Off plays the file as is.
- On load the original tempo is guessed from the file's length: the whole number of
  4/4 bars (1, 2, 4, 8, 16) that puts it between 70 and 180 BPM (the one nearest
  120 if several do), written to the parameter; the user can type over it.
- Resample: the instrument plays the original audio at `speed` (pitch moves with it),
  instantly. Stretch: the UI renders the whole file at `speed` with the same
  `media::process` path audio clips use, on a worker thread; the last render (or the
  original) plays until the new one is ready.
- Slice boundaries map from original frames to the matched audio by the same ratio,
  so they land on the same musical places whatever the mode or tempo.

### Slicing

- Transients: `media::process::detect_transients` over the file with a sensitivity
  (0–1), replacing the markers.
- Grid: 1/4, 1/8, 1/16, 1/32 of a bar at the original tempo, replacing the markers.
- By hand on the waveform: double-click adds a marker, drag moves one, double-click
  on a marker (or Delete with it selected) removes it. Each action is one undo step.
- A newly loaded file is sliced at transients (default sensitivity).
- At most 64 slices; slice `k` plays on key `root + k`.

### Playing

- A note plays its slice from its start to the next marker (or the file's end),
  through the amp envelope; past the slice's end the voice stops (with a short
  fade-out so it doesn't click). Reverse plays the slice backwards.
- Rate = matched speed (Resample) × 2^((Pitch + slice Pitch + note bend)/12).
- Cut (default on): a new note stops any slice still playing with a ~5 ms fade; off,
  up to 16 voices ring over each other.
- Per slice: Level (dB) and Pan.
- Real-time safe: the matched audio and slice table reach the instrument as
  `InstrumentData` built on the UI side; nothing is allocated on the audio thread.

### Panel (in the section-card style; fits 1280×800)

- A large waveform card: the matched audio's peaks, slices in alternating tints,
  draggable markers, each slice's key name, the playing slices lit; Load…, the file's
  name, and a drop target for audio files.
- Tempo card: Mode, Original Tempo, Follow Tempo, the resulting speed.
- Slicing card: Transients (with sensitivity), Grid (1/4–1/32), Slices to Piano Roll.
- Selected-slice card: Reverse, Pitch, Level, Pan of the slice clicked on the
  waveform.
- Amp card: Volume, Pitch, Attack, Decay, Sustain, Release, Cut, Root Key.

### Slices to Piano Roll

Writes (one undo step) a MIDI clip on the slicer's track at the playhead (or bar 1)
playing every slice in order, each note at its slice's place in the matched loop (in
ticks at the project tempo) and as long as its slice.

### Loading by dropping

- An audio file dropped on the slicer's waveform loads it (auto-sliced).
- An audio file Alt-dropped on the timeline makes a new MIDI track playing a Slicer
  with that file.

### Out of scope

FL Studio Slicex import (a follow-up: the importer could map Slicex channels to this);
the CLAP packaging of the slicer (it needs a file, like the Sampler, which isn't
packaged either).

## Testing

- project: bend round trip; files without bends byte-identical; split/glue/reverse/flip
  of bent notes; `set_slices` sorting and limits; slices kept across instrument change.
- engine: bend events compiled at the right samples and values; each built-in
  instrument's bent note measured at the target pitch; realtime-safety tests with
  bends; parallel rendering identical with bends.
- slicer (instrument tests): slice `k` on key `root + k` plays that slice's audio;
  Resample at speed 2 doubles the pitch, Stretch keeps it; Cut stops the previous
  slice; Reverse; bends move the pitch.
- ui_tests: drawing a bend in Bend mode, Clear Bend; loading and slicing (transients,
  grid, by hand), Slices to Piano Roll, dropping a file, Alt-drop on the timeline;
  the slicer panel fits 1280×800.
- golden (`engine/examples/golden`): every existing device's hashes unchanged; the
  slicer's added.
