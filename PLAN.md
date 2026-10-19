# Plan: arrangement timeline

Goal: an Ableton-style **Arrangement** timeline (a linear timeline, not the Session clip
launcher) where you can place audio clips and MIDI clips, organize tracks into nested
groups, and later add automation. Audio warping is out of scope for now.

## The core decision

Today the UI keeps a few values in `Params` and sends each change as a `Copy` command.
That doesn't scale to a timeline, because the project becomes a large tree of tracks,
clips and notes. So:

- **The UI thread owns a `Project`.** This is plain data: tracks, clips, notes and
  automation. All editing and undo happen there.
- **The engine plays a compiled, read-only snapshot of it.** After each edit, the UI
  builds an `Arrangement` with positions already converted to samples and sorted. It
  sends the whole thing to the audio thread, which swaps it in. The audio thread never
  sees the editable model.

## Crate layout

```
crates/
  dsp/       + Instrument trait, a simple poly synth, a sampler voice
  project/   NEW. The document: tracks, groups, clips, notes, tempo map, edits/undo, serde.
             No threads, no audio, no I/O. Easy to unit-test.
  media/     NEW. Decodes audio files on a worker thread (symphonia), converts them to the
             session sample rate, and builds waveform peak data for drawing.
  engine/    + arrangement.rs (Project → Arrangement), transport in samples, track/bus graph,
             clip playback. Depends on project + media.
  ui/        + session.rs (owns Project, undo history, EngineHandle, sample pool)
             + timeline/ (see below)
```

## `project`: the model

```rust
pub struct Project {
    pub tempo: TempoMap,      // one fixed BPM + 4/4 for now; all tick↔sample math lives here
    pub tracks: Vec<Track>,   // top level, in display order
    pub master: Mixer,
}

pub struct Track {
    pub id: TrackId,          // stable; edits and engine commands refer to ids, not indices
    pub name: String,
    pub color: Color,
    pub mixer: Mixer,         // volume, pan, mute, solo
    pub devices: Vec<Device>, // effect chain, empty for now
    pub automation: Vec<AutomationLane>, // empty until automation lands
    pub kind: TrackKind,
}

pub enum TrackKind {
    Audio { clips: Vec<AudioClip> },
    Midi  { instrument: InstrumentKind, clips: Vec<MidiClip> },
    Group { children: Vec<Track>, folded: bool },
}

pub struct MidiClip  { id: ClipId, start: Tick, length: Tick, notes: Vec<Note> }
pub struct AudioClip { id: ClipId, start: Tick, source: SourceId,
                       source_offset: u64, source_len: u64 /* samples */, gain_db: f32 }
```

Decisions behind this:

- **Musical time is ticks** (e.g. 960 per quarter note). Clips snap to bars and beats,
  and changing the tempo moves them correctly. Only `TempoMap` converts ticks to samples,
  so tempo automation later only changes that one type.
- **Unwarped audio clips have a fixed length in samples, not ticks.** Their length in
  beats is derived from it. Storing the length in ticks is the classic trap: a tempo
  change would make the model and the playback disagree.
- **Groups are a tree, not a flat list with parent ids.** Ableton's rules fall out of the
  structure: groups nest, groups hold no clips, and a group's children are contiguous.
  Look up tracks by id with a tree walk; a few hundred tracks is nothing.
- **Undo starts by cloning the whole `Project`** on each committed edit. Audio data lives
  in `media` behind `Arc`s, so the clone is small. It's simple and always correct; make it
  smarter only if profiling says so.
- **Derive `serde` from day one**, so save/load is cheap whenever we get to it.

## `engine`: getting the snapshot to the audio thread

- **Swap and return.** Add `Command::Arrangement(Box<Arrangement>)`. The engine swaps it
  in and pushes the *old* box onto a second `rtrb` queue back to the UI, which drops it
  each frame. `Command` stops being `Copy`, but the real rule still holds: nothing is freed
  on the audio thread. `assert_no_alloc` catches deallocations as well as allocations, so
  `tests/realtime_safety.rs` keeps enforcing it. Keep everything on one queue so commands
  stay in order with snapshots.
- **Snapshots must not reset synths or effects.** If every clip drag rebuilt the
  instruments, notes and reverb tails would be cut off. Keep two kinds of state apart:
  - **Clip data** (sorted audio regions and MIDI events in samples) is immutable and
    swapped wholesale.
  - **Processors** (instrument, effects, fader smoothing) live across swaps. When a
    snapshot arrives, the engine moves the existing processor `Box`es from the old
    snapshot into the matching tracks of the new one, which is just a pointer move. New
    tracks arrive with processors built on the UI thread. Processors for removed tracks go
    back in the old snapshot and get dropped on the UI side.
- **Fader and mute moves stay small `Copy` commands** addressed by `TrackId`, so dragging
  a fader doesn't rebuild anything.
- **The track graph is a flat list.** Compile the tree in post-order (children before
  their group) and give each group a bus, with bus 0 as master. Per block: clear the
  buses; then for each track, render its clips (or, for a group, read its bus), apply
  devices and fader, and add the result to its output bus. Solo is resolved at compile
  time; soloing a group solos its children.
- **Clip playback works in samples only**, since everything was converted when the
  snapshot was built.
  - **MIDI:** keep a cursor into the sorted events and pass `(frame_offset, NoteOn/Off)`
    to `dsp::Instrument::process(events, left, right)`. Collect events into a `Vec`
    allocated up front and never grown.
  - **Audio:** copy the overlapping region and add a few milliseconds of fade at clip
    edges, like Ableton's default clip fades, to avoid clicks.
  - **Hanging notes:** on seek, stop, or a snapshot that changes a track, send
    all-notes-off.

## `ui`: the timeline

```
ui/src/
  session.rs        Project + undo + EngineHandle + sample pool; commit(edit) → rebuild + send
  timeline/
    mod.rs          TimelineView: lays out ruler, track headers and lanes
    viewport.rs     zoom and scroll; the single place that converts ticks ↔ x pixels
    grid.rs         snap settings and bar/beat grid lines
    ruler.rs        bar numbers, click to seek, loop region later
    track_header.rs name, color, mute/solo, group fold arrow, volume
    lane.rs         one track's row: paints clips; a folded group shows its children's clips in summary
    clip.rs         clip body: waveform from media peaks, or a mini note view for MIDI
    interaction.rs  select, move, resize, split, delete → Edits
  widgets/          (existing)
```

egui notes:

- Put headers and lanes in **one** vertical `ScrollArea`, one row per track, so they
  scroll together.
- Do horizontal scroll and zoom by hand through `Viewport` (Shift+wheel / Ctrl+wheel), so
  the header column stays fixed and the ruler can share the same mapping.
- Paint clips with the painter and use `ui.interact(rect, id, Sense::click_and_drag())`
  for each clip, rather than a widget per clip.
- While dragging, draw a ghost preview and only commit the `Edit` on release. That gives
  one undo step and one engine rebuild per gesture.
- Selection, zoom and scroll are UI state, not part of `Project`.

## Preparing for automation

- Give every automatable value a stable
  `ParamId { track: TrackId, target: Volume | Pan | Device(slot, index) }`.
- Engine parameters already go through `Smoothed`. Automation then compiles to sorted
  breakpoints in samples, like MIDI, and sets those targets every block (or every 64
  samples).
- The `automation` field on `Track` exists from the start, so adding it later doesn't
  change the model's shape.

## Build order

Each step ends with something runnable.

1. [x] **`project` crate + musical transport:** add `TempoMap`, add `Seek`, show bars.beats
   in the transport bar, and build the ruler, playhead and click-to-seek.
2. [x] **Snapshot plumbing + MIDI tracks:** add `dsp::Instrument`, a poly synth (replacing
   the test oscillator panel), and MIDI clips you can create and hear.
3. [x] **Audio clips:** build the `media` crate, drop a file onto a lane (`dropped_files`),
   draw waveforms, and add sampler playback.
4. [x] **Groups:** buses, folding, Ctrl+G to group the selected tracks.
5. [x] **Editing:** move, resize, split and delete clips, with snap, undo/redo and
   save/load.
6. [x] **Piano roll** as a bottom detail view, like Ableton's.
7. [x] **Automation lanes.**

Update the README's layout and threading-rules sections to match as each step lands.

## Status

All seven steps are in. Where the code ended up differing from the plan above:

- **Instruments live in their own crate** (`instruments`), not in `dsp`: the
  `Instrument` trait, parameter descriptors with normalized/log mapping, and the
  built-ins (a 16-voice Poly Synth with a TPT state-variable filter, plus the simple
  oscillator). Parameters are set by index, so changing one on the audio thread never
  touches a `String`.
- **MIDI clips have an `offset`.** Trimming a clip's left edge moves the offset
  instead of deleting notes, so the trim can be dragged back, the same way audio
  clips keep `source_offset`.
- **Audio clip offsets and lengths count frames at the file's own rate**, not the
  session's, so changing the sample rate doesn't move anything. `project::AudioSource`
  records each file's rate and length.
- **Placing a clip trims what it lands on** (`make_room`), as Ableton does. The one
  exception: an audio clip dropped over just the edge of another crossfades with it
  (both keep their audio and fade over the overlap). Layering deeper than two is
  still trimmed, and crossfade fades go away when the clips stop overlapping.
- **The engine renders in 64-frame blocks** (`MAX_BLOCK`), which is also the
  automation resolution, and finds each block's MIDI events and audio regions by
  binary search instead of keeping cursors, so seeking needs no extra state.
- **Snapshot swaps compare against the live fader**, not the previous snapshot, so a
  fader moved by live commands and then undone ends up in the right place.
- **Automation values are stored on a `0..=1` scale** per lane (`AutomationTarget`:
  volume, pan, or an instrument parameter by id) and applied every block while playing.
- **Stop returns to where playback started**; stopping again goes to the start.
  Instrument release tails keep ringing after Stop; audio clips fade out over 5 ms.
- **UI tests** (`crates/ui/src/ui_tests/`) drive the whole app headlessly with
  `egui_kittest` and check the engine's output.

Added after the plan, from use: vertical zoom, copy/paste, a visual envelope editor
shared by all instruments, audio clip fades and volume handles, clip mute, freezing a
MIDI clip to audio (offline render through its instrument, `engine::Compiler::render_midi_clip`),
curved and drawn automation with several lanes per track, and file drops on native
Wayland (`crates/ui/src/wayland_drop.rs`).

Audio clip tools:
- **Model:** an `AudioClip` can be reversed, looped (`loop_phase`, `play_len`),
  stretched (`Stretch`: Resample, Stretch or Beats mode, speed, transpose, an
  optional original tempo that the clip can sync to), polarity-inverted and set
  to mono or swapped channels. Offsets, lengths and fades stay in material
  frames at the file's rate, before stretching; a clip's length on the timeline
  is its material divided by its speed (`Project::audio_ticks` /
  `audio_material`).
- **Rendering:** clips that don't play their file as is get a buffer rendered by
  `media::process_clip` on a worker thread (`media::Renderer`, cached per clip in
  `ui/src/renders.rs`). The compiler asks for it through `engine::AudioLookup`, and
  `engine::clip_process` says exactly what to render. That spec doesn't change when
  a clip moves, so moves and unsynced tempo changes never re-render. Gain, fades,
  polarity and channels are applied while playing.
- **Editing:** slip, chop (at transients, or to the grid), normalize, and
  consolidate (`Compiler::render_audio_clips`, written like a freeze).

Automation editing: point selection lives in the view
(`timeline::automation::PointSelection`, dropped if the lane changes some other way).
Shapes and random values come from `project::Generate`, and the Generate window
previews them as a live edit.

The song's key is `Project::scale` (`project::Scale`: a root pitch class and a
`ScaleMode`), which knows membership, snapping and stepping by scale degrees for
the piano roll and its chord tools.

Piano roll tools: the note math is in `project::note_tools`, pure functions from a
clip's notes and the ids to change to its new notes (plus `chord`, for stamping).
`ui/src/piano_roll/tools.rs` holds the Tools menu, the chord box and the tool
windows. A window keeps the notes as they were when it opened and previews every
change as a live edit from them, so settings never compound.

MIDI input: `ui/src/midi_input.rs` opens every device with midir and sorts what
arrives on each driver thread. Notes, pitch bend (`NoteEventKind::PitchBend`) and
the sustain pedal go to the audio thread through `engine::LiveSender`, and the
engine plays them on the track set with `Command::SetLiveTarget`. The engine
reports what it played (`engine::Played`, with the playhead while rolling), which
the session records into clips. Other CCs drive `project::ControllerLink`s (MIDI
learn), saved in the project. The engine also has a metronome
(`Command::SetMetronome`), worked out from the playhead alone.

Export: `engine::Bounce` plays a freshly compiled arrangement offline. Each track
file is the project with only that track soloed and the master at unity.
`ui/src/export.rs` runs the jobs on a worker thread and writes WAVs with
`media::write_wav_as`.

Effects: the `effects` crate has the `Effect` trait (prepare on the UI thread;
process, set_param, reset and meters on the audio thread) and the built-ins, whose
parameters are `instruments::ParamDescriptor`s like instruments'. In the project a
`Device` (with a `DeviceId`, an `EffectRef`, the parameters set, a bypass flag and
an optional sidechain source track) sits in `Track::devices` or
`Project::master_devices`; effect parameters are automated with
`AutomationTarget::DeviceParam`. An effect's descriptor also lists its UI-only
`settings` (stored with its parameters, never sent to the engine), what each of its
`meters` slots means, whether it wants its sound tapped for the UI, and whether it
takes a sidechain. The compiler builds a chain of `chain::EffectSlot`s per node and
for the master, creating instances only for devices the engine doesn't have yet; on
a swap the engine moves instances over by device id, so chains can be edited
without cutting tails. Chains run after a node's source and before its fader (for
groups, after their children are mixed).

- **Meters and taps:** each live device has a `chain::Meters` (atomics the audio
  thread stores after each block) and, if it asks, an rtrb ring of `TapFrame`s
  (input and output, stereo) that `Compiler::drain_tap` empties for the UI. The
  session keeps the last 1.4 s of each tap (`ui/src/taps.rs`). Instruments publish
  meters too (`Compiler::instrument_meters`).
- **Sidechains:** `Device::sidechain` names a track. The compiler gives every track
  some effect listens to a send bus; the node copies its sound there after its
  effects, and each listening slot gets it as `Context::sidechain`.
- **Instrument data:** instruments that need a file (`DataKind::Sfz`) get it through
  `Instrument::set_data` when they're built. The track names the file
  (`TrackKind::Midi::file`); the UI loads it on a worker thread
  (`ui/src/banks.rs`) and the `AudioLookup` hands it to the compiler, whose
  `ProcessorKind` includes the data's identity, so a new load builds a new instance.
- **Presets:** `project::PresetFile` (the device's id and every setting by id) is
  what a preset file holds; `ui/src/presets.rs` keeps them in the app's data
  folder, a folder per device id. Loading one replaces all the settings in one
  undo step (`set_device_params`, `set_instrument_params`), leaving out ids the
  device doesn't have.
- **Clipper:** its oversampling is linear phase (`mastering_halfband.rs`), so
  its latency is exact and reported per setting; its true-peak safety
  (`mastering_safety.rs`) reads peaks from the last filter's half-sample
  values. It shares the limiter's true-peak detector (`limiter::truepeak`).
- **Organizing chains:** `project::CopiedDevice` is an effect with its automation
  lanes; `copy_devices`, `insert_devices` (paste, with new ids), and
  `duplicate_devices` build on it, and `move_devices` moves effects within or
  between chains by id, so the engine keeps their instances (and tails). Their
  lanes and controller links follow them; a sidechain that would point at its
  own track is cleared. In the UI (`effect_rack.rs`) the selection is
  `Selection::devices`, the clipboard lives in egui's memory so it outlives a
  project, and the keyboard goes to the effects while the device view was the
  last thing clicked (`CanopyApp::devices_focused`). Drags carry an
  `effect_rack::DeviceDrag`, which the chain, track headers and the master
  strip accept.
- **Panels:** `ui/src/devices/` has a panel per effect and instrument, all built
  from `devices::Panel`: knobs (`widgets/knob.rs`), choices and switches that carry
  the automation menu, reset and undo; the device's meters and tap; and its
  settings. Devices without a panel of their own get a grid of knobs.
- **Delay compensation:** an effect's descriptor says how many samples late it
  plays at given settings (`EffectDescriptor::latency`: the Limiter's lookahead,
  the Ducker's). The compiler adds up each node's effects (bypassed ones don't
  count), works out when each node's sound reaches its bus (a group's own lateness
  included), and gives every node the delay that makes it meet the slowest there
  (`compensate` in `arrangement.rs`). The delay lives in the track's processors
  (`compensation.rs`) and crossfades when it changes. While playing, the UI's
  playhead and MIDI recording take the mix's total lateness
  (`Compiler::latency`) off the engine's position, so they follow what's heard.
  Sidechain sends aren't compensated: a few milliseconds.

Loop region: `Project::loop_region` (start, end, on) is a view setting carried
through undo like folding. The arrangement holds it in samples; the engine cuts a
block at the loop's end, jumps back to its start and renders the rest there,
releasing held notes as a seek does, so the wrap lands on the exact sample.
Exports don't loop; they can cover just the loop's stretch instead of the song
(`Bounce::range`, which also leaves out the mix's latency at the start).

FL Studio import: `crates/flp` reads the .flp event stream into a neutral
model of channels, patterns, playlist items, mixer inserts and automation
(`model.rs`), then builds a `Project` from it with a stand-in for each plugin
(`import.rs`, `plugins.rs`) that the dialog lets the user change before
importing. Inserts used by one channel fold into its track; the others become
groups nested by their routing. Sampler channels get a generated one-region
.sfz in the app's data folder. The import replaces the session like opening a
file (fresh undo history, unsaved). The format knowledge comes from public
reverse-engineering notes (PyFLP, FLParser, dawtool, node-flp), checked against
real files; `cargo run -p flp --example flp_dump -- [--model|--import] x.flp`
shows what a file holds. Real-file tests run when `CANOPY_FLP_SAMPLES` names a
folder of .flp files.

Keyboard-learning app: `crates/learn` (binary `canopy-learn`) plays through
the same engine as the DAW, with a two-track project: one track for what you
play (through `engine::LiveSender`, as the DAW's MIDI input does) and one for
what the app plays. The exercise logic (theory, notation layout, grading, song
progression, the MIDI file parser) is pure and unit-tested; the UI tests drive
the app with simulated MIDI on a test clock. Some of it duplicates the DAW and
could move into shared crates: MIDI device handling (`learn/devices.rs` is
`ui/midi_input.rs` plus timestamps), the kittest rig (`quiet_wgpu`), the .sfz
loading in `ui/banks.rs`, and the piano drawing; its theory, notation and MIDI
file code could give the DAW chord names, a score view and MIDI import/export.
The app's own notes are sent from the frame loop, so up to a frame late; a
timestamped note command in the engine would fix that.

Not done: plugins (CLAP/VST3); latency compensation for sidechains; and
tempo/time-signature changes over time (`TempoMap` holds one tempo and meter).
