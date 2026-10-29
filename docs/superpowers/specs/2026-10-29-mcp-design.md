# An MCP server for canopy — design

Date: 29 October 2026. Approved by the user in conversation, section by section:
architecture, tool catalogue, analysis (with spectrum and spectrogram images), robustness
and testing. Second of priority 7 in `docs/progress.md` (after the History panel, before
the Analyzer upgrade and Analysis view, versions, and merging).

## Goal

An LLM (Claude Code, Claude Desktop, any MCP client) can do essentially anything in the
running canopy that a user does with the music: read the whole project, edit tracks,
clips, notes, instruments, effects and automation, and measure and look at the sound.
The user's examples, a subset of what it is for:

- "Generate more variations of this chopped amen break."
- "Analyze the spectrum of this vocal and add a precise de-esser."
- "Which instruments are clashing in the mix?"
- "Name, color and organize my instruments."

Success: those four work from a plain request, every LLM edit is visible at once and is
one undo step, and nothing the LLM sends can crash canopy, corrupt the project, or
disturb the audio.

Decisions the user made:

- The LLM works on **the running app**, not on files.
- LLM edits **apply at once**, each an undo step (no per-edit confirmation).
- **Composable tools** (read, edit, analyse) rather than one tool per task or a
  scripting language; the API covers as much as reasonably can be covered.
- **Saving is the user's alone.** Also left out on purpose: open, new, close, quit;
  audio device settings, plugin editor windows, MIDI learn; the patcher and modulation
  (until their engines exist); deleting files.
- Spectrum and spectrogram **images** are part of this build.
- Order: this build, then the Analyzer upgrade (live spectrogram, capture, ghost
  compare, Save/Copy image) and the whole-song Analysis view, which add
  `source: capture` to `analyze` and `plot`.

## 1. Architecture

Three new pieces; the audio thread and the engine's realtime rules are untouched apart
from stem capture in offline bounces (section 4).

### `crates/analysis` (new, pure)

Measurements, spectrogram data and PNG drawing, on rendered buffers. No threads, no UI,
no MCP. Depends on `realfft`, `media` (transients, beats), the Analyzer effect's loudness
code (`effects::analyzer::loudness`, made `pub` for this), `tiny-skia`, `png` and
`ab_glyph` (all three already in the dependency tree through eframe and winit). Used by
the MCP now and by the GUI Analysis view next.

### `crates/mcp` (new, pure)

- `protocol.rs`: JSON-RPC 2.0 and the MCP subset used: `initialize` (with the server's
  `instructions`), `notifications/initialized`, `ping`, `tools/list`, `tools/call`,
  `notifications/cancelled`, `notifications/progress`. Hand-written over `serde_json`;
  no async runtime (`rmcp` would bring tokio for a protocol this small). Line-delimited
  JSON, as MCP's stdio transport is.
- `tools/`: the tool table, one module per group (section 2). Each tool is a name, a
  description, a JSON schema and a typed argument struct; the schema and the parsing
  live side by side so they can't drift. Shared by the bridge and the app.
- `host.rs`: the `Host` trait the tools run against: `project()`, `edit(label, f)`,
  `undo/redo/jump`, `history()`, the selection, the playhead and transport, loaded audio
  and Slicer views, the device registries and plugin catalogue, presets, file imports,
  and `render(request) -> RenderTicket` for analysis. The app implements it; tests use a
  fake.
- `bin/canopy-mcp.rs`: the stdio bridge (section 1, "Bridge").

### The app side (`crates/ui/src/mcp/`)

- `listener.rs`: a thread listening on `127.0.0.1` on a random port. It writes
  `{port, token, pid}` to `~/.cache/canopy/mcp.json` (`%LOCALAPPDATA%\canopy\mcp.json`
  on Windows), readable only by the user, written atomically, removed on quit. Each
  connection gets a thread that checks the token, parses messages and sends
  `(call, reply sender)` down a channel, then wakes the UI as MIDI input does.
- `pump.rs`: runs each frame in `CanopyApp::ui`, beside `plugins::pump` and before
  `Session::update`. It drains the channel on the UI thread within a time budget and runs
  each call through `host.rs`'s `Host` implementation, which borrows the session, the
  timeline (selection) and the piano roll. Reads and edits answer within the frame;
  renders reply when their worker finishes.
- Renders for analysis reuse export's machinery (`export::start`, `Exporting::serve` for
  projects with plugins, `hosting::Offline` instances), on worker threads.
- View → "Allow LLM connections" (on by default, kept between runs) and a status-bar
  indicator while a client is connected, showing the last action's label.

Architecture rules kept: the UI thread alone owns the `Project` (1); nothing is allocated
on the audio thread (2); every LLM edit is a `Session::edit` undo step (5); ids, not
indices (6); every chain edit ends in `normalize_routing` (10); parameters through
`AutomationTarget` and the static descriptors (11, 12).

### Bridge (`canopy-mcp`)

What MCP clients launch (`claude mcp add canopy -- canopy-mcp`; Claude Desktop's
`mcpServers` config). It answers `initialize`, `tools/list` and `ping` itself from the
shared table, so it starts instantly and never exits because canopy isn't running. Each
`tools/call` is forwarded to the app, connecting (or reconnecting) as needed; without an
app it returns a tool error, "canopy isn't running; open it and try again". Reads are
retried once on a dropped connection; edits never are (they may already have happened).
Cancellation is forwarded. Built with the workspace; `tools/build-windows.sh` ships it
beside `canopy.exe`.

### History origin

`History` steps gain an origin: `Origin::User` or `Origin::Llm { client: String }` (the
client's name from `initialize`). LLM labels read "LLM: Add EQ to Vocal". The History
panel shows a small tag on LLM steps and draws consecutive LLM steps as one group.
`get_history` reports origins too.

## 2. Tools

### Conventions (also in the `instructions` the server sends)

- **Ids** are numbers from one counter, never reused: a number alone names a track, clip,
  note, device or modulator. Reads always include them.
- **Time** in beats (quarter notes, fractions allowed), 0 at the song's start; reads add
  "bar.beat" for readability. Positions inside an audio file in seconds.
- **Pitch** as a MIDI number or a name, "C3" = 60 (the piano roll's naming).
- **Parameters** in their real units (Hz, dB, ms, %) from the descriptors; choices by
  label. Out of range is an error that states the range, never a silent clamp.
- **Selection**: `get_project` includes the user's selected tracks, clips and (in the open
  piano roll) notes, so "this clip" resolves.
- **Results**: every edit returns the ids it made and a one-line summary. A failed edit
  changes nothing and says what to fix.
- **Measuring, not hearing**: analysis is measurement; check an edit by measuring again.

### Catalogue (33 tools)

Read

- `get_project`: tempo, key, loop, length, playhead; the track tree (id, name, color,
  kind, instrument, mixer, devices with ids and names, clip counts, automated targets);
  the master's chain; the selection; whether LLM edits are allowed.
- `get_track { track }`: everything about one track: mixer, instrument with all
  parameters, devices with all parameters, clips (id, name, kind, start, length),
  automation lanes (targets, point counts), sidechain sources.
- `get_clip { clip, from?, to?, cursor? }`: a MIDI clip's notes (id, start, length,
  pitch, name, velocity, release velocity, muted, bend points), paged 2000 at a time; an
  audio clip's file, offsets, length, gain, fades and curves, reverse, loop, stretch,
  channels, polarity; on a Slicer track, the slices.
- `get_device { device | track }`: an effect's or a track's instrument's parameters: id,
  name, value, unit, range, mapping (log/linear), default, choices; bypass and sidechain.
- `list_devices { kind? }`: every built-in instrument and effect and installed plugin,
  with descriptions; with `id`, one type's full parameter table.
- `get_automation { track, target? }`: lanes and points (beats, value in the target's
  units, curve).
- `list_presets { device | track }`: factory and user presets for that device type.
- `get_history { last? }`: steps with labels, ages and origins, and the position.

Song and tracks

- `add_track { kind: midi|audio|group, name?, color?, instrument?, after? | inside? }`.
- `edit_track { track, name?, color?, volume_db?, pan?, mute?, solo?, folded? }`.
- `arrange_tracks { op: move|group|ungroup|delete, tracks, after?|inside?, name? }`.
- `edit_song { tempo_bpm?, key?, loop? }`.

Clips

- `add_clip { track, start, kind: midi, length, name?, notes? }` or
  `{ track?, start, kind: audio, file }` (waits for the file to load; a new audio track if
  `track` is missing).
- `edit_clips { clips, move_by?|start?, track?, length?, name?, muted?, gain_db?,
  fade_in?, fade_out?, fade_in_curve?, fade_out_curve?, reversed?, looped?, stretch?, pitch?, channels?,
  polarity?, slip? }`; audio-only fields on a MIDI clip are an error.
- `clip_ops { op: split|delete|duplicate|copy_to|chop|consolidate|normalize|freeze,
  clips, at?, times?, track?, mode: transients|grid?, sensitivity?, division? }`.

Notes

- `write_notes { clip, mode: add|replace, notes: [{ start, length, pitch, velocity?,
  release_velocity?, muted?, bend? }] }`; at most 10,000 notes.
- `edit_notes { clip, change?: [{ id, start?, length?, pitch?, velocity?, muted? }],
  delete?: [id] }`.
- `transform_notes { clip, notes?, op: quantize|naturalize|chop|arpeggiate|strum|legato|
  glue|fit_to_scale|reverse|flip|transpose|chord, options }`, on the `project::note_tools`
  functions; all notes when `notes` is missing.
- `set_note_bends { clip, bends: [{ note, points }] }`.

Sound

- `set_instrument { track, instrument, file?, params? }` (a built-in id or a plugin from
  the catalogue; a file for the Sampler or the Slicer).
- `set_params { device | track, values: { param: value } }`: many at once, one undo step.
- `add_effect { track | master, effect, position?, params?, sidechain? }`.
- `edit_effects { op: remove|move|bypass|sidechain, devices, position?, on?, track? }`.
- `load_preset { device | track, preset }`.
- `slicer { track, op: get|set|transients|grid|to_piano_roll, slices?, sensitivity?,
  division?, at? }`.
- `set_automation { track, target, points? | generate? | clear? }`; `generate` is
  `project::generate` (wave, from, to, period, depth, curve).

Analysis (section 3)

- `analyze { target, range?, variant?, measures: [spectrum|loudness|dynamics|transients|
  sibilance|pitch|key|stereo] }`.
- `find_clashes { tracks?, range?, top? }`.
- `plot { kind: spectrum|spectrogram, targets, range?, variant?, markers?, width?,
  height?, freq_range?, db_range? }`: image content plus axis JSON.
- `render_audio { target, range?, path }`: a WAV; never overwrites an existing file.

Control

- `history { op: undo|redo|jump, to? }`.
- `transport { op: play|stop|seek, at? }`.
- `batch { operations: [{ tool, arguments }] }`: up to 200 edits as one undo step, all or
  nothing; reads and analysis aren't allowed inside.

`target` is `{ track } | { clip } | { master }` (a group is a track). `variant` is `mix`
(default: after effects and fader), `pre_fader` or `dry` (no effects). `range` is
`{ from, to }` in beats or `"selection"`.

## 3. Analysis

### Renders

- The UI thread compiles a snapshot from a copy of the project (soloed for one track, as
  export does; devices bypassed for `dry`), and a worker renders it at the session's
  rate. Playback is not touched.
- **Stem capture**: `engine::Bounce` gains an option to keep each requested track's output
  (post-fader or pre-fader) during one offline pass, so `find_clashes` and multi-target
  plots render once rather than once per track. Bit-identical to soloed renders of each
  track; nothing allocated per block.
- A cache keyed by (history generation, target, range, variant) holds recent renders
  (up to 512 MB of audio, least recently used dropped), so a spectrum and then sibilance of
  the same vocal render once.
- At most 2 renders at once, the rest queued. Each checks for cancellation every block and
  sends progress every half second. Renders longer than 10 minutes of audio are refused
  with a hint to pass a range.
- A result measured on an older project version than the current one says so.

### Measures

All return compact JSON (dB to 0.1, Hz to whole numbers, times in beats and bar.beat)
with a one-sentence `summary`.

| Measure | Returns |
|---|---|
| `spectrum` | Average spectrum (Welch, 4096-point Hann, half overlap) in ⅓-octave bands 20 Hz–20 kHz; strongest resonant peaks (frequency, level, prominence over a ⅓-octave neighbourhood); centroid, 85% roll-off, tilt in dB/octave; optionally 8 bands' energy per bar. |
| `loudness` | Integrated LUFS, short-term and momentary maxima, LRA, true peak dBTP, RMS, crest factor (the Analyzer effect's BS.1770/R128 code). |
| `dynamics` | Peak and RMS per bar, crest factor over time, loudest and quietest sections. |
| `transients` | Onsets (`media::process::detect_transients`) with strength; tempo estimate (`media::beats`); onsets' offsets from the grid. |
| `sibilance` | Events (time, duration, peak frequency in 4–12 kHz, level, how far above the surrounding voice); summary: the typical sibilant band, its median and 95th-percentile level against the voice's. |
| `pitch` | Monophonic f0 (YIN): median, range, notes over time, deviation from equal temperament. |
| `key` | Chroma-based key estimate (Krumhansl profiles) with confidence and the runner-up. |
| `stereo` | Correlation; width (side against mid) in low, mid and high bands; level lost summed to mono. |

### `find_clashes`

Every sounding track (or the ones named) from one stem-capture pass, each split into 24
Bark bands over 100 ms windows. Two tracks clash in a band and window when both are within
40 dB of the mix's level there and within 6 dB of each other, so neither clearly masks the
other. Pairs are ranked by clashing time weighted by level; each reports its bands, share
of time, worst bars, and which track is usually louder. It reports; fixing is the LLM's
choice (EQ, sidechain ducking, panning, arrangement).

### Images (`plot`)

Drawn by `crates/analysis::images` with tiny-skia, text in Inter (from
`ui/assets/fonts`) via ab_glyph, on the app's dark palette and the live spectrum widget's
scales (`ui/src/widgets/spectrum.rs`), so the Analyzer's Save image will look the same.

- `spectrum`: log frequency 20 Hz–20 kHz with labelled ticks, dB vertical; the average
  spectrum smoothed to ⅙ octave, a faint peak-hold curve, the strongest peaks marked with
  their frequency. Several targets overlay in distinct colors with a legend.
- `spectrogram`: time across, labelled in bars; log frequency up; level as color with a
  scale bar. Optional markers: sibilance events, transients, a clash's worst regions
  against a second track, the loop or selection.
- Default 1024×512, at most 1600×1000 (an image costs the model about w×h/750 tokens).
  Returned as MCP image content (base64 PNG) with JSON giving the axes' ranges and scales.

## 4. Robustness

Connection

- Loopback only; a 256-bit random token compared in constant time; closed if the token
  isn't sent within 2 s. A stale token file (dead pid) is replaced.
- Line-delimited JSON, at most 16 MB per message; malformed input gets a JSON-RPC error.
- Several canopy windows: the newest owns the file; `get_project` names the project.

UI thread

- Calls are drained within ~4 ms a frame; the rest wait for the next frame, which a
  waiting call requests.
- Edits wait while the user is mid-gesture (`begin_live` … `end_live`), so a drag and an
  LLM edit can't share an undo step, and while a modal dialog (export, FLP import, unsaved
  changes) is open. Reads always answer, from the project as it is shown.
- With "Allow LLM connections" off, every call gets "canopy has LLM access turned off".

Edits

- Validation before any change: ids exist and are the right kind; times not negative;
  pitches 0–127; parameters in range; choices match a label; clips on matching tracks.
  Errors name the fix ("track 41 is an audio track; MIDI clips need a MIDI track (MIDI
  tracks: 12 Bass, 18 Lead)").
- `History::edit` rolls back a failed edit; `batch` is all or nothing.
- Limits: 10,000 notes per write, 200 operations per batch, 64 tracks per arrangement
  call; reads paged.

Analysis

- Panics in analysis are caught and become tool errors; canopy stays up.

## 5. Testing

- `crates/mcp`: protocol tests (initialize, list, call, errors, malformed input,
  cancellation); every tool against a fake `Host` on real `Project`s (success, each
  validation error, rollback, batch atomicity, paging); every tool's arguments parse from
  its own published schema, and every example in a description is valid.
- `crates/analysis`, on synthetic signals with known answers: a sine's spectral peak and
  its pitch; EBU loudness reference tones; clicks found as transients at their times;
  `ess` bursts over a vowel found as sibilance at their frequency; two tracks sharing a
  band clash, two in separate bands don't; a synthesised progression's key; PNGs of the
  right size with a sine's peak in the right column; images saved under
  `CANOPY_SCREENSHOTS` for review.
- `crates/engine`: stem capture bit-identical to soloed renders; no allocation
  (`tests/realtime*.rs`).
- `ui_tests/mcp.rs`, with a real socket: the token required; `get_project` sees the rig's
  tracks; an edit is one undo step that Ctrl+Z reverts, tagged in the History panel; a call
  during a drag waits; the off switch refuses; `analyze` and `plot` reply after the render;
  a cancelled render stops.
- The bridge spawned as a process: it answers without canopy, then forwards once a test
  listener appears.
- Windows: loopback TCP and the token file's path are the only platform differences; both
  run in the Windows CI.

## 6. Build order

One spec, built in three phases, each ending with its tests passing and usable on its own:

1. **Connection and reading**: `crates/mcp` protocol and tool table, the bridge, the app's
   listener and pump, the switch and indicator, the read tools, `history`, `transport`.
   An LLM can see everything and undo.
2. **Editing**: the song, track, clip, note, sound and automation tools, `batch`, the
   History origin and its tags.
3. **Analysis**: `crates/analysis` (measures, clashes, images), stem capture in
   `engine::Bounce`, the render queue and cache, `analyze`, `find_clashes`, `plot`,
   `render_audio`.

## 7. Docs

- README: "Using it" gains "Talking to canopy from an LLM" (setting up Claude Code and
  Claude Desktop, what it can and can't do, the switch); the Layout section lists the two
  crates.
- CLAUDE.md: the crate table and the `ui` map (`mcp/`, `history.rs`).
- `docs/progress.md`: a workstream section, and priority 7 updated.

## Later (not in this build)

- `source: capture` for `analyze` and `plot`, with the Analyzer upgrade.
- Tools for the patcher and modulation, when their engines are built.
- Versions (commits, branches, diff) and their tools, per priority 7.
