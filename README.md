# canopy

A small, personal DAW for Linux (and Windows; see [Building for
Windows](#building-for-windows)), written in Rust with egui.

It has an Ableton-style arrangement timeline: MIDI and audio tracks, nested
groups, clips you can move, trim, split and duplicate, a piano roll, automation
lanes, audio clip editing (reverse, loop, FL-style resample and time-stretch,
slip, chop, crossfades, consolidate), MIDI keyboards and controllers (playing,
recording, MIDI learn), WAV export of the mix or each track, undo/redo, and
save/load.

## Setup

```sh
sudo apt install pkg-config libasound2-dev   # needed by cpal, even when using PipeWire
cargo run                                     # debug build (DSP is still optimized a bit)
cargo run --release
```

On Wayland/X11 the window libraries are loaded at runtime, so they need no dev packages.

To talk to PipeWire directly instead of through its ALSA layer (lower latency, finer control):

```sh
sudo apt install libpipewire-0.3-dev clang
cargo run --features pipewire
```

## Building for Windows

canopy builds as a single static `canopy.exe` for 64-bit Windows 10 and 11. The
C runtime, and the C++ runtime of the time-stretcher (ssstretch), are linked in,
so the .exe needs no Visual C++ Redistributable and no DLLs besides the ones that
come with Windows. Copy it anywhere and run it.

### From Linux

The default, and the recommended toolchain, is MSVC (`x86_64-pc-windows-msvc`),
cross-compiled with [cargo-xwin](https://github.com/rust-cross/cargo-xwin):

```sh
sudo apt install clang                # compiles ssstretch's C++ (as clang-cl)
cargo install --locked cargo-xwin
tools/build-windows.sh --accept-microsoft-license   # the first time
tools/build-windows.sh                              # after that
```

This writes `dist/canopy-windows-x86_64/canopy.exe`, plus `canopy.pdb`, its debug
symbols: keep it next to the .exe and a panic's backtrace names functions and
lines. The script then lists the DLLs the .exe imports, and fails if one of them
doesn't come with Windows.

On its first build, cargo-xwin downloads Microsoft's C runtime and Windows SDK
into `~/.cache/cargo-xwin`, and doing so means accepting [Microsoft's
licence](https://go.microsoft.com/fwlink/?LinkId=2086102) for them. That's why the
script won't start it without `--accept-microsoft-license`. Other options:
`--console` keeps the console window (see below), `--profile release` builds
faster without whole-program optimization, and arguments after `--` go to cargo
(`-- --features asio`).

Without Microsoft's licence, MinGW-w64 (`x86_64-pc-windows-gnu`) builds it with
open-source tools only:

```sh
sudo apt install mingw-w64
tools/build-windows.sh --gnu
```

That links libstdc++, libgcc and winpthread in statically, and uses Windows' own
`msvcrt.dll` as the C runtime. MSVC stays the default because it's Windows' native
toolchain (what Windows plugins are built with, and what CI builds with) and a
fully maintained Rust target. The rustc book says the `windows-gnu` targets "do
not have any maintainers" ([platform support:
windows-gnu](https://doc.rust-lang.org/rustc/platform-support/windows-gnu.html)).
CI runs the tests on Windows. With Wine installed, cross-built tests run under it
too (checked with `canopy-clap`'s; the UI's need a Vulkan driver):
`CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER=wine cargo xwin test --target x86_64-pc-windows-msvc -p canopy-clap`.

### On Windows

With Rust (rustup's MSVC toolchain) and Visual Studio's Build Tools ("Desktop
development with C++") installed:

```sh
cargo build --profile dist -p ui --bin canopy --target x86_64-pc-windows-msvc
```

The .exe is in `target\x86_64-pc-windows-msvc\dist`. `bash tools/build-windows.sh`
in Git Bash does the same and also checks the DLLs, and `cargo run` works as on
Linux.

### On GitHub Actions

`.github/workflows/windows.yml` runs on every push, on `windows-latest`. It runs
`cargo test --workspace` there (the UI tests render with WARP, Direct3D 12's
software rasterizer), builds the .exe with `tools/build-windows.sh`, and uploads
it as the `canopy-windows-x86_64` artifact. To also get an ASIO build, run the
workflow by hand (Actions → Windows → Run workflow) with "asio" ticked.

### What makes it static

- `.cargo/config.toml` builds MSVC targets with `-C target-feature=+crt-static`,
  so the C runtime (the UCRT and vcruntime) is linked in. cc sees the same flag
  and compiles ssstretch's C++ with `/MT` to match (see [the Rust reference on
  static C runtimes](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)).
  `RUSTFLAGS` replaces those flags instead of adding to them, so leave it unset;
  the script unsets it.
- The `dist` profile, also in `.cargo/config.toml`, is `release` with fat LTO,
  one codegen unit, and line tables for the `.pdb`. `--release` is unchanged.
- The GPU code needs nothing extra: wgpu draws with Direct3D 12 and compiles its
  shaders with `d3dcompiler_47.dll`, which comes with Windows (it uses
  `dxcompiler.dll` only if one is installed).
- To check on Windows, run `dumpbin /dependents canopy.exe` in a Developer
  PowerShell for Visual Studio (or `llvm-objdump -p canopy.exe`, then look for
  `DLL Name`). Only Windows' own DLLs should be listed: `kernel32.dll`,
  `user32.dll`, `d3d12.dll` and the like, plus `api-ms-win-*` API sets. There
  should be no `vcruntime140.dll`, `msvcp140.dll`, `libstdc++-6.dll`,
  `libgcc_s_seh-1.dll` or `libwinpthread-1.dll`. Running it on a PC that has never
  had Visual Studio installed proves the same.

### Running it on Windows

- **Console.** Release builds are GUI programs, so no console window opens next
  to canopy, and what it logs goes nowhere. Build with `--console` (or `cargo
  build --features console`) to keep the console. Debug builds always have one.
- **Settings and data** (window size, presets, FL imports, frozen audio of unsaved
  projects) live in `%APPDATA%\canopy\data`.
- **Audio** goes through WASAPI to the default output device, with the audio
  thread under MMCSS (Windows' scheduling for audio). cpal opens WASAPI in shared
  mode only, so Windows mixes canopy with other programs and converts its sample
  rate to the device's if they differ. Latency is the device's period (usually
  10 ms) plus the buffer. There's no exclusive mode in cpal; use ASIO for lower
  latency.
- **ASIO** is an optional cargo feature, `asio`, built on Windows (or in CI, see
  above): `cargo build --profile dist -p ui --bin canopy --target
  x86_64-pc-windows-msvc --features asio`. It needs LLVM for bindgen
  (`winget install LLVM.LLVM`, then set `LIBCLANG_PATH` to its `bin` folder) and
  downloads Steinberg's ASIO SDK at build time; `CPAL_ASIO_DIR` points it at a
  copy you already have ([cpal's ASIO
  notes](https://github.com/RustAudio/cpal#compiling-for-asio)). The SDK can't come
  with canopy: Steinberg licenses it under GPLv3 or its own proprietary licence
  ([since October 2025](https://www.kvraudio.com/news/steinberg-moves-vst-3-sdk-to-mit-open-source-license-asio-now-gplv3-65179)),
  so an ASIO build is for your own use, and handing it to others would make it
  GPLv3. When it runs, canopy uses the first ASIO driver it finds, or the one
  `CANOPY_ASIO_DRIVER` names (any part of the name, any case: `focusrite`). If no
  driver starts (none installed, or another program has it), it falls back to
  WASAPI. The status bar shows which one is playing. Drivers that want integer
  samples (most of them) get them converted from the engine's floats. ASIO4ALL and
  FlexASIO give ASIO to sound cards without their own driver.
- **MIDI** goes through WinMM. Before Windows MIDI Services (rolled out to Windows
  11 in 2026), which shares devices, only one program at a time can open a MIDI
  device. One that's in use is shown as such in the MIDI menu, and canopy keeps
  trying, so it opens once the other program lets go.
- **Drag and drop** from Explorer works. winit doesn't say where files are
  dropped on Windows, so canopy asks Windows for the cursor
  (`crates/ui/src/windows_drop.rs`).
- **Plugins** are found in Windows' standard folders (`%COMMONPROGRAMFILES%\VST3` and
  `\CLAP`, `%PROGRAMFILES%\VSTPlugins` and `\Steinberg\VSTPlugins`, `%COMMONPROGRAMFILES%\VST2`;
  `VST_PATH` and `CLAP_PATH` add more). To read a new plugin file safely, canopy.exe starts
  itself again, once per file, as a scanner; the results are cached in
  `%LOCALAPPDATA%\canopy\cache\plugins.json`. Editors open in windows of their own. 64-bit
  plugins only.
- **Manifest.** `crates/ui/build.rs` embeds an application manifest. It declares
  per-monitor DPI awareness, version 6 of the common controls, long paths, and
  Windows 10 and 11 as supported.

### Known limitations

- WASAPI and ASIO both play through one device, picked as described above; there's
  no device picker yet, as on Linux.
- The window has egui's default icon, and the .exe has no icon of its own (canopy
  has no icon yet).
- Projects store absolute paths. When a project moves between Linux and Windows,
  an audio file is found again only if it's next to the project. Paths saved on
  Windows (`C:\…`) aren't split into folders on Linux, so even that fails there.
- `canopy-learn` builds the same way (`-p learn --bin canopy-learn`), but without
  a manifest.

## Layout

```
crates/
  midi/         MIDI primitives as Rust types: Pitch, Velocity, Tick (960 PPQ), NoteEvent,
                and parsing raw messages from hardware.
  dsp/          Building blocks: oscillator, ADSR, state-variable filter, gain,
                smoothing. Pure DSP, no threads or I/O. Unit-tested.
  instruments/  The Instrument trait, parameter descriptions, and the built-in
                instruments: Poly Synth, Wavetable, Drum Synth, SFZ Sampler (with
                its .sfz parser), Guitar (a physical model), Simple Oscillator.
  effects/      The Effect trait and the built-in effects: EQ (24 bands, dynamic),
                Compressor (four characters), Limiter, Clipper, Multiband, Ducker,
                Waveshaper, Shaper, Vocoder, Reverb, Delay, Analyzer. Parameters
                are described like instruments' ones; effects also publish meters
                and, if they ask, their sound (a tap) for the UI to draw.
  project/      The document: tracks, nested groups, clips, notes, effect chains,
                tempo map, automation, every edit, undo history, RON save/load.
                No threads, audio or I/O.
  media/        Decodes audio files on worker threads (symphonia), converts them to
                the session sample rate, and builds waveform peaks. Renders clips
                that are reversed, looped or stretched (process.rs; Signalsmith
                Stretch for the pitch-keeping modes), on a worker thread too
                (renderer.rs).
  engine/       The audio thread. arrangement.rs compiles a Project into a read-only
                snapshot (positions in samples, a flat track/bus graph); audio.rs
                plays it; chain.rs runs effect chains. bounce.rs plays one offline,
                as fast as it renders, for exports. live.rs carries MIDI keyboard
                input to the audio thread. device.rs is the only file that knows
                about cpal.
  plugins/      Hosts third-party plugins (CLAP, VST3, VST2) on Linux and Windows:
                scanning in separate processes, instances, real-time processing,
                parameters, state, editor windows. Knows nothing of canopy; its
                README has the details (folders, yabridge).
  hosting/      Plugins as canopy's devices: the manager that keeps an instance
                per plugin device and instrument in step with the project (state,
                undo, restarts, the editors' own edits), the Effect and Instrument
                adapters the engine runs, and their registered descriptions.
  canopy-clap/  The other way round: the built-in instruments as CLAP plugins for
                other hosts, all in one canopy.clap (tools/package-clap.sh).
  ui/           The egui app (binary: `canopy`).
                session.rs     Project + undo + engine handle + loaded audio
                renders.rs     rendered audio for processed clips, kept per clip
                banks.rs       instruments' files (.sfz banks, user wavetables), loaded
                               on worker threads
                taps.rs        the recent sound of effects that tap it, for displays
                export.rs      the Export dialog, and exports on a worker thread
                plugins/       plugin menus, panels, editor buttons, the Plugin Manager
                midi_input.rs  MIDI devices (midir)
                timeline/      ruler, track headers, lanes, clip and automation editing
                piano_roll.rs, clip_panel.rs, device_view.rs, effect_rack.rs
                               the bottom detail views
                devices/       each effect's and instrument's panel, built on Panel
                               (knobs, choices, meters, taps, automation menus)
                theme.rs       the design system: spacing, sizes, radii, strokes,
                               text sizes, animation times, shared colors
                widgets/       hand-painted widgets (knob, envelope, EQ curve,
                               spectrum, scopes, shape editor, keyboard map, meters);
                               chrome.rs has the small shared ones (icon buttons,
                               toggles, menu items, drag labels, the focus ring)
  flp/          Reads FL Studio projects (.flp, FL 9 to 25): the event stream
                (reader.rs), what it describes (parse.rs, model.rs), which
                built-in stands in for each plugin (plugins.rs), and the canopy
                project made from it (import.rs). write.rs writes small .flp
                files for tests. ui/src/flp_import.rs is the dialog.
  learn/        A separate app for learning to play keyboards (binary:
                `canopy-learn`), on the same instruments and engine.
                theory.rs, notation.rs  spelling, key signatures, chord names,
                                        fingering; laying out notes on a staff
                grading.rs, reading.rs, drill.rs, song.rs
                                        what was asked, what was played, and how
                                        well (pure, unit-tested)
                smf.rs         Standard MIDI File reading and writing
                devices.rs     MIDI keyboards (as the DAW's midi_input.rs, plus
                               timestamped notes for the UI)
                audio.rs       a two-track project on the engine: you, and the app
                ui/            the tabs, staff and falling-notes painters, piano
```

## The keyboard-learning app

```sh
cargo run --release -p learn
```

Plug in a MIDI keyboard (the MIDI menu picks it up while running) or play with
the mouse or the computer keyboard (FL's layout: Z and Q rows, `-`/`=` for the
octave). F1 lists the shortcuts.

- **Keyboard:** free play; what you hold shows on a grand staff, spelled in the
  key you pick, with the chord named.
- **Note reading:** a note on the staff (treble, bass or both, as many ledger
  lines as you allow, any key signature) or a key name; play it. Rounds of 10,
  20, 50 or endless, with streak, accuracy and speed.
- **Scales & chords:** scales and modes, triads, 7ths, sus chords and inversions,
  each hand or both, with fingering. With the metronome on it counts in and
  shows how early or late each note was.
- **Songs:** open a `.mid` file; its notes fall onto the keys, colored by hand.
  Wait (it stops until you play the right notes), Play along (scored), or Listen;
  play one hand and let the app play the other, at 25 to 150% speed, with an A–B
  loop (Shift+drag the seek bar).
- **Progress:** best scores per exercise and song, practice time, and the last two
  weeks.

## Threading rules

The audio callback runs every few milliseconds and must never wait on anything.

- **The UI owns the project.** All editing and undo happen on the UI thread. After
  each committed edit, `engine::Compiler` builds an `Arrangement`: every clip, note
  and automation point already converted to samples and sorted, and the track tree
  flattened children-first with a bus per group (bus 0 is the master).
- **UI → engine:** `Command`s through one lock-free ring buffer (`rtrb`), in order.
  `Command::Arrangement` carries a whole snapshot; the rest are small (fader moves,
  seek, play/stop, auditioned notes).
- **Nothing is freed on the audio thread.** When a snapshot arrives the engine moves
  each surviving track's processors (instrument, fader) into it, which is a pointer
  move, and sends the old snapshot back through a second ring buffer. The UI drops
  it. If that queue is full the engine waits before swapping rather than free
  anything itself.
- **Snapshots don't reset sound.** Instruments, faders and effects live across
  swaps, so editing while playing doesn't cut notes or reverb tails. Effects move by
  their device id, so adding, removing or reordering one leaves the others ringing.
  New tracks and effects arrive with processors built (and their buffers allocated)
  on the UI thread. A track whose notes changed gets all-notes-off, as do all tracks
  on seek and stop.
- **Live gestures don't rebuild.** Dragging a fader or a knob sends small commands;
  the snapshot catches up, as one undo step, when the drag ends.
- **engine → UI:** meters (master and per track, and each effect's and
  instrument's own readings) and the playhead through atomics. Effects that draw
  their sound (the analyzer, the EQ's spectrum, the waveshaper's scope) also push
  what goes in and comes out into a lock-free ring per effect; the UI drains them
  every frame. If it falls behind, the newest audio is dropped, never waited for.
  A device's own meters and ring are only filled while the UI reads them (its
  panel is showing, give or take a second and a half), so hidden devices don't
  spend the audio thread's time on readings nobody sees.
- **Tracks render on several cores.** Each track renders its block into a
  buffer of its own; a group sums its children's, and the master sums the
  top-level tracks'. A group therefore waits for its children, and an effect
  listening to another track waits for that track; everything else is
  independent, so the audio thread and a small pool of worker threads
  (`engine/src/pool.rs`) take tracks from a lock-free ready queue as their
  dependencies finish. The output is identical to rendering in order, bit for
  bit, whatever the thread count. One worker per physical core but the audio
  thread's, performance cores only on a hybrid CPU (on the development laptop,
  two P-cores and eight E-cores, one worker makes a block 1.8× faster and any
  more make it slower; on uniform cores it scales to five or six times with
  eight threads); `CANOPY_AUDIO_THREADS=n` overrides it, 0 for a single
  thread. Workers spin briefly between blocks, then sleep until just before
  the next callback is due (the engine keeps track of the period), so they
  are warm when it comes; with nothing to share they park for good, so an
  idle engine costs nothing.
- **Sidechains:** an effect can listen to another track. That track's sound
  after its effects (before its fader, so a muted "ghost" kick still works) is
  copied to a send buffer the listening effect reads, in the same block, since
  the listener waits for its source. Where that would be a loop (two tracks
  listening to each other, a track listening to its own group), the listener
  reads the block before instead.
- **Instrument files load off the audio thread.** A sampler's .sfz and its samples
  are decoded on a worker thread; the next snapshot hands the instrument the
  loaded bank, and the old one is dropped on the UI side.
- **MIDI keyboards skip the UI.** Each device's driver thread (midir) parses what it
  gets and sends notes, the pitch wheel and the sustain pedal straight to the
  audio thread through a second lock-free queue (`engine::LiveSender`; the senders
  share it behind a mutex the audio thread never touches). The engine plays them
  on the target track, applies the pedal, and reports what it played, with the
  playhead, back to the UI for recording. Other controllers go to the UI, which
  moves whatever they're linked to as a live gesture.
- **No allocation, locks, or I/O in `Engine::process_interleaved`, any
  `Processor::process` or `Instrument::process`.** Allocate in `new`/`prepare`.
  Debug builds abort if the audio callback or a render worker allocates or frees
  (`assert_no_alloc`), and `crates/engine/tests/realtime_safety.rs` and
  `crates/instruments/tests/realtime.rs` check this in tests, snapshot swaps
  included. The only waiting the audio thread does is for the last track of a
  block to finish on a worker.
- **Plugins run their own code on the UI thread too.** The UI thread is the
  plugins' main thread: they're loaded, activated, asked for their state and
  their editors' windows run there, and `hosting::PluginManager::pump` runs their
  callbacks every frame (a waker repaints when they ask for it). On the audio
  thread an adapter (`hosting::PluginEffect`, `PluginInstrument`) passes each
  block's parameter changes, notes and transport to the plugin's processor from
  buffers allocated up front. Debug builds let plugins allocate inside their own
  calls (their business, not canopy's), and still check everything around them.
  A plugin that asks to restart (its latency changed) is silent for a snapshot or
  two: the old instance has to come back to the UI thread and be deactivated
  before the new one starts. Exports and freezes get instances of their own, made
  on the UI thread and run on the export thread.
- **Decoding happens on `media` worker threads.** The UI polls for finished loads
  each frame; audio data is shared with the engine behind `Arc`s and only ever
  dropped on the UI side.
- **So does clip processing.** A clip that doesn't play its file as is (reversed,
  looped, stretched, pitched) plays a buffer rendered for it on the `media`
  renderer thread. Until a new render arrives the clip keeps playing its previous
  one, so dragging a speed control doesn't drop out. Gain, fades, polarity and
  channel mode are applied while playing and never need a render.

## Tests

```sh
cargo test --workspace
cargo test -p dsp -p engine --no-default-features   # no ALSA needed
```

`crates/ui/src/ui_tests/` drives the real app headlessly with `egui_kittest`
(mouse and keyboard input, rendering through software Vulkan) and checks what the
engine plays. To see what each step looks like:

```sh
CANOPY_SCREENSHOTS=/tmp/shots cargo test -p ui ui_tests
```

`ui_tests/tour.rs` is a screenshot tour of every main view (menus, dialogs,
drags in progress, empty states) at 1280×800 and 1920×1080, for looking at the
UI as a whole: `CANOPY_SCREENSHOTS=<dir> cargo test -p ui tour`.

## Using it

Help → Keyboard Shortcuts (F1) lists every shortcut and mouse gesture.

- **Transport:** Play, Stop and Record at the top left. Stop goes back to where play
  started; pressed again while stopped, to the very start (as Space twice does). The
  display beside them shows the playhead as bar.beat.sixteenth (green while playing)
  and as time.
- **MIDI clips:** double-click an empty spot on a MIDI track. Double-click a clip to
  edit its notes in the piano roll.
- **Audio clips:** drop audio files (wav, flac, mp3, ogg, aiff) onto an audio track,
  or anywhere else to make a new track. Or File → Import Audio.
- **Audio clip fades and volume:** hover a clip for its handles: the squares on the
  top corners set fade lengths, the dots on the fades bend them, and the bar at the
  top center sets the clip's volume. Double-click a handle to reset it.
- **Audio clip tools:** double-click an audio clip for the Clip tab, or right-click
  it. Reverse; Loop (or just drag the end past the end of the file); invert
  polarity; mono or swapped channels; Normalize; Chop at transients or to the grid;
  Consolidate, which renders the selected clips and the gaps between them to one
  new file. Alt-drag inside a clip slides its audio without moving the clip.
- **Stretching:** Shift-drag a clip's edge. Resample mode (the default, like FL
  Studio's) is varispeed: faster is higher. Stretch keeps the pitch, and its own
  Pitch control transposes without changing length. Beats cuts at the hits and
  moves the slices, for drums. Type the audio's own tempo into the Clip tab and
  turn on Sync to have it follow the project's tempo.
- **Crossfades:** drop an audio clip over the edge of another and both fade over
  the overlap instead of one being trimmed. The fades go away when they're pulled
  apart.
- **MIDI keyboards:** every MIDI input device is used unless you switch it off in
  the MIDI menu (plugging one in is noticed within two seconds). The keyboard plays
  the selected MIDI track, or the last one selected, with sustain pedal and pitch
  wheel (±2 semitones). Press R (or the ● button) to arm recording: while playing,
  what you play is recorded into the clip under it, or a new clip of whole bars.
  Ctrl+M toggles the metronome.
- **MIDI controllers:** right-click any knob, slider or fader → Link to Controller,
  then move a knob on your controller. Links are saved with the project;
  right-click again to unlink. A controller's changes become one undo step once
  it's left alone for a moment.
- **Exporting:** File → Export Audio (Ctrl+R) writes the mix, the selected tracks,
  or each track to WAV (16-bit dithered, 24-bit, or 32-bit float), with or without
  the tails that ring past the end, of the whole song or just the loop.
  Right-click a track → Export… for just that one. Track files are what you'd
  hear soloing the track, through its own fader and its groups' but not the
  master's, so together they add up to the mix. They all start at the song's
  start, so they line up when imported elsewhere.
- **FL Studio projects:** File → Import FL Studio Project… reads an `.flp` (FL 9
  to 25) into a new project: channels become tracks (playing the closest
  built-in instrument; FL's Sampler, DirectWave and sforzando channels play
  their samples through the SFZ Sampler), patterns become clips where the
  playlist has them, audio clips keep their samples, mixer inserts become
  groups (or fold into the one track they serve), effects map to the built-in
  ones, and volume and pan automation comes along. Third-party plugins (VSTs,
  CLAPs) that are installed here come in as themselves, with the settings FL
  saved; the ones that aren't get a stand-in. The dialog lists each plugin with
  what it becomes (the plugin itself where it's installed, or a built-in; change
  any, or skip it) and what can't come over: some built-ins' settings (they
  start at their defaults), tempo automation, markers, mixer sends, and
  automation of plugin settings.
- **Plugins:** CLAP, VST3 and VST2 effects and instruments. canopy looks for them
  in the standard folders when it starts (View → Plugin Manager… lists what it
  found, what it couldn't read, and where it looks; Rescan after installing
  something). Add an effect from "+ Effect" → Plugins → its maker, and pick an
  instrument from the Plugins menu beside a MIDI track's instrument. "Edit" on
  the plugin's title bar opens its own editor in a window of its own (Plugin
  Manager → Keep editors on top keeps them above canopy's). The panel shows its
  parameters as knobs and choices with the plugin's own values (search them when
  there are many); they automate, link to controllers and take presets like any
  other device's, and a preset keeps the plugin's own state too. What you change
  in the plugin's editor is one undo step per gesture; a preset loaded in the
  editor, or other changes to its state, become one when they stop. Projects keep
  each plugin's state; one that isn't installed keeps its settings, says so, and
  passes the sound through (an instrument is silent). On Linux, Windows plugins
  come in through yabridge (see `crates/plugins/README.md`).
- **canopy's instruments in other hosts:** `tools/package-clap.sh` writes
  `dist/clap/canopy.clap`, one CLAP file with every built-in instrument but the
  Sampler (Poly Synth, Wavetable on its factory tables, Drum Synth, Guitar,
  Simple Oscillator), listed as "Canopy Guitar" and so on; `--install` also copies
  it to `~/.clap`. `--windows` builds `dist/clap-windows-x86_64/canopy.clap` for
  64-bit Windows (with cargo-xwin, as for canopy.exe; the C runtime linked in);
  put it in `C:\Program Files\Common Files\CLAP` or
  `%LOCALAPPDATA%\Programs\Common\CLAP`. They play as they do in canopy (the same blocks, parameter
  timing and sustain pedal), with every parameter automatable (and floats
  modulatable, in hosts that can) under canopy's names and value text. There's
  no editor: hosts show their own controls.
- **Freezing:** right-click a MIDI clip → Freeze to Audio renders it (with its
  instrument's tail) to a WAV next to the project, puts it on a "(frozen)" audio
  track below, and mutes the MIDI clip, which stays editable. 0 mutes and unmutes
  clips.
- **Loop:** drag along the ruler's top strip (or Shift+drag anywhere on it) to loop
  that stretch; drag the loop to move it and its ends to change it, and click it to
  turn it on or off. L (or Loop in the transport bar) does that too, with no region
  yet looping the four bars from the playhead's; Shift+L loops the selected clips.
  It's how you're listening, so it isn't an undo step, but it's saved with the
  project.
- **Looping in the piano roll:** its ruler is the same ruler, on the same ticks, so
  the loop you see there is the song's one loop (the arrangement shows the same
  stretch). Drag along its top to loop part of the clip and play: playback goes
  round it while you edit. L, or the piano roll's Loop button, turns it on and off;
  turning it on keeps a loop that's over the clip and otherwise loops the whole clip,
  and puts the playhead in it. Shift+L loops the selected notes (out to the grid).
- **Clips:** drag to move (also to other tracks of the same kind), drag an edge to
  trim. Clicking in a lane sets the marker (and, while stopped, the playhead):
  Ctrl+E splits there and Ctrl+V pastes there. Clips placed over others trim them,
  as in Ableton. Hold Alt while dragging to skip the grid; drag against the edge to
  scroll.
- **Tracks:** click a header to select (Ctrl to add, Shift for a range), drag
  headers to reorder or into a group, double-click the name to rename, right-click
  for color, grouping, automation and delete. A MIDI track's header names its
  instrument after the track's name (a sampler's, its .sfz file). Its volume and
  pan fields fill with the track's color up to where they're set: drag them, or
  double-click to type.
- **Piano roll:** double-click to add a note, double-click or right-click to remove
  one, drag to move, drag the right end to resize, drag on empty space to select,
  drag the velocity stems below. Click the keyboard to audition. Right-click empty
  space for a menu (paste, select all, loop, zoom to fit, tools, delete). Each clip
  remembers its zoom and scroll.
- **Note bends:** B (or the piano roll's Bend button) switches to Bend mode, where
  dragging across a note draws its own pitch curve right in the roll: it holds the
  note's pitch until where the drag began, then follows the pointer, a point per
  grid step, snapped to semitones (Shift: to the cent; Alt: off the grid). Drag a
  point to move it, Alt-drag up or down on a segment to curve it, double-click a
  point to remove it, right-click a note to clear its bend. Each note bends on its
  own, so a chord can bend one note; outside Bend mode the curve shows as a thin
  line over the note.
- **Zoom to fit:** Z zooms to the selected clips (in the piano roll, the selected
  notes or the whole clip), Shift+Z to the whole song; or the bracket buttons by
  Follow and in the piano roll's bar. Zooms glide there. The arrangement's zoom and
  scroll (and row height) are saved with the project and come back on opening it;
  like folds they aren't undo steps, and looking around alone doesn't ask to be
  saved.
- **Scale:** pick the song's key with the button next to the tempo (a root and one
  of twelve scales, or none). The piano roll shows its notes as the light rows and
  tints the root's.
- **Chords:** the box in the piano roll's header picks what a double-click writes:
  a note, or a triad, 7th, 9th, sus2, sus4, power chord or octave on it, built from
  the scale's own notes (C major's if none is set), so it always fits the key.
- **Piano roll tools** (the Tools menu; FL Studio's shortcuts), on the selected
  notes or all of them:
  - Quantize (Ctrl+Q at once; Alt+Q with grid, strength, starts and ends).
  - Naturalize (Alt+R: random timing, velocity and length).
  - Chop (Alt+U: cut at every grid line).
  - Arpeggiate (Alt+A: up, down, up and down, or random, over 1 to 4 octaves).
  - Strum (Alt+S).
  - Strum Pattern (Alt+P, below).
  - Legato (Ctrl+L), Glue, Fit to Scale, Reverse, Flip (Alt+Y).

  The ones with settings open a window that previews them on the notes as you
  change them; Apply keeps the result as one undo step, Cancel puts them back.
- **Strum patterns** (Tools → Strum Pattern…, Alt+P): draw chords as long held
  notes, select them, and pick a rhythm: each chord is strummed in it, as one undo
  step. The hand keeps the bar's time, so a chord that changes mid-pattern picks it up
  where it is (or tick "Start again on each chord"); a chord lasts until the next.
  Down or up isn't written: it comes from where a stroke falls (down on the beats and
  eighths, up between; in sixteenths once the pattern strikes between eighths), which
  is how the Guitar's Alternate strumming reads it too. The window draws the pattern's
  steps with their arrows, previews the notes as you change anything, and Listen loops
  the chords' bars and plays them.
  - **Syntax**, one character per step, upper case accented: `D` or `U` strum (the
    same: the direction is the step's), `C` the chord without its bass, `B` only the
    bass note, `X` a dead-note chuck, `P` a palm mute, `-` a rest (silence), `_` hold
    the stroke before. Spaces and `|` only group. `D_du_uDu` is "Old Faithful"; a
    pattern is as long as its steps (six eighths for a bar of 3/4).
  - **Settings:** grid (1/8, 1/16, 1/8 triplet), swing (off-beats late: 33% is a
    triplet shuffle, 50% a dotted one), gate (how long a stroke rings; holds add whole
    steps), the velocity of accents and of the rest (shares of each chord note's own),
    upstrokes leaving out the lowest one or two notes, and a light, repeatable humanize.
  - **Articulations:** on the Guitar (key-switches on), dead notes and palm mutes
    switch its articulation with key-switch notes, only where it changes, and the clip
    is left as it was after the chords. Other instruments play them as short, quiet
    strums, so it works for piano comping and synth stabs too.
  - **Presets:** Folk / pop, Straight eighths, Driving sixteenths, Island strum,
    Reggae skank, Ska upstrokes, Funk sixteenths, Rock palm-mute chug, Pop punk,
    Shuffle, Country boom-chicka, Bossa nova (two bars), Ballad, Waltz (3/4) and 6/8
    strum. Hover one for what it is.
  - Hear it: `demos/guitar/15_pattern_tool_folk.wav` and `16_pattern_tool_funk.wav`
    (`cargo run --release -p engine --example strum_patterns`).
- **Envelopes:** drag the points of the envelope in the device view: the peak for
  attack, the corner for decay and sustain, the end for release, and the hollow dots
  to bend a stage. Double-click a point to reset it.
- **Knobs:** drag up or right to turn one up (Shift or Ctrl for fine moves), or use
  the mouse wheel; double-click to type a value, Ctrl+click resets it; right-click to
  automate it or link a controller. Tab to a knob and the arrow keys turn it (Shift
  for fine steps), Home and End take it to either end; holding a key down is one
  undo step. A knob's name turns the track's color when it's automated.
- **Instruments** (the box at the top of a MIDI track's device view): each sits on
  cards of controls in the color of what they do (oscillators blue, teal and violet,
  filters coral, envelopes green, LFOs blue, macros orange), with small knobs that
  show their value where their name is while you hover or turn them (and in their
  tooltip). Every built-in fits a 1280-pixel-wide window; when the device view is
  short, its displays shrink and the page scrolls up and down.
  - **Poly Synth:** a simple subtractive synth: its wave drawn, its filter's curve
    (drag it for cutoff and resonance), its envelope.
  - **Wavetable:** the main synth, in the spirit of Serum. Three wavetable
    oscillators (14 built-in tables; drag the display to move through one), each
    with two warps in a row (sync, bend, PWM, mirror, quantize, FM/AM/RM from
    another: A hears B, B and C hear A) and up to 16 unison voices, which can
    stack in octaves or fifths and spread the warp across them. Each can play its
    own table from a WAV file (Load… in the oscillator's … menu, or drop it on its display;
    2048-sample frames, or what Serum's `clm` chunk says, up to 256 of them),
    saved with the project by path; a sub, noise, and two filters (low/high/band-pass,
    notch, combs, formant, ladder) in series or in parallel, with each source sent
    to either (the filter card's Routing menu). Both curves follow their modulation
    in one display. The pages' tabs are in the instrument's title bar; under them,
    the modulation sources as chips to drag onto knobs. Envelopes &
    LFOs: three envelopes with delay and hold (the first is the amp), four LFOs
    (free or synced, retriggered or one-shot), each a stock shape or one you draw.
    Matrix: sixteen slots routing envelopes, LFOs, velocity, the wheels, four macros
    or a per-note random to almost anything, each with a curve and polarity; the
    dot on each amount, and on modulated knobs, shows where it is right now. It
    lists the routes in use and a "+ Route" row for the next. Each envelope's and
    LFO's chip sketches its shape. Mono, legato and glide on the Voicing page; the
    macros (drag a macro's tile onto a knob) and the volume at the right of every
    page.
  - **Drum Synth:** fourteen synthesized drums on the General MIDI drum notes
    (kick 36, snare 38, closed hat 42, open hat 46, …). Click a pad to hear it and
    see its knobs; pads light up as they play. The closed and pedal hats choke the
    open one. In the piano roll the rows are named after the drums, and an empty
    clip opens on the kit. A pad's six layers are chips sketching their source;
    the chosen layer's source, pitch envelope, filter, amp envelope (drawn) and
    output are cards beside the pad's hit preview.
  - **SFZ Sampler:** Load SFZ… picks an .sfz instrument (sforzando's format:
    regions, velocity layers, round robins, keyswitches, release samples, loops).
    Its samples load in the background; the key map shows each region over the
    keys and velocities it plays, with its root key marked, and lights what's
    playing; click a key to hear it. Reload picks up edits to the
    file. The path is saved with the project. Or drop an .sfz file onto the
    timeline: on a MIDI track, that track plays it; anywhere else, it gets a new
    track named after it. In the piano roll, keys it doesn't play are dimmed and
    its keyswitches labelled.
  - **Guitar:** a physically modelled guitar. Notes go to strings the way a
    guitarist would play them (the fretboard shows where), chords are strummed
    (down-strokes at high velocity), close notes on one string slur or slide, the
    mod wheel adds vibrato, and there's an optional amp (clean, crunch, lead). The
    fretboard spans the panel, with the String, Playing, Noises and Amp cards under it.
- **Effects:** + Effect in the device view adds one to the selected track (groups
  too: they process everything in them). Click Master, above the master meter, for
  the master's chain. Sound goes through them left to right, after the instrument
  or clips and before the fader. Its switch bypasses it. Every parameter can be
  automated or linked to a controller, and double-clicked to reset.
- **Organizing effects:** each effect has a title bar. Click it to select the
  effect (Ctrl-click and Shift-click for more). Drag it to move the selected
  effects along the chain, or onto a track's header or the master strip to move
  them there; hold Ctrl to copy instead. Its buttons duplicate, open the menu
  (also on right-click: presets, duplicate, copy, cut, paste, move left or right,
  move or copy to another track, fold, remove) and remove. After clicking in the
  device view, the keyboard works on the selected effects: Delete, Ctrl+D,
  Ctrl+C / X / V (pasting after the selection, on any track and in another
  project), Left / Right to select along the chain and Ctrl+Left / Right to move.
  Effects keep their automation and controller links when moved or copied. The
  arrow (or double-clicking the bar) folds an effect down to its bar, to keep a
  long chain in view.
- **DSP load:** the status bar shows how much of its time the audio thread needs
  at its busiest (yellow from 50%, red from 80%; at 100% the sound breaks up).
- **Presets:** right-click an effect's title bar (or use Presets beside an instrument's
  picker) to save its settings under a name, load one back, or go back to the
  defaults. They're kept in the app's data folder
  (`~/.local/share/canopy/presets/`), one folder per effect or instrument, so any
  project can use them. Most effects and instruments come with a few presets of
  their own, listed first: reverb rooms and halls, slapback and dub delays, vocal
  and bus compression, an OTT that's easy to back off, a kick pump, a 16th gate,
  drum kits, amp tones, and Wavetable patches (supersaw, pluck, pad, bell keys,
  Reese, wobble and sub basses, a glide lead).
  - **Sidechain:** effects that listen to another track (Compressor, EQ, Ducker,
    Vocoder) have a Sidechain button beside their name: pick the track there.
  - **EQ:** up to 24 bands over the sound's spectrum (before, faint, and after).
    Double-click the curve to add a band there, or + Band; drag a handle sideways
    for frequency and up or down for gain, use the wheel over it for Q, right-click
    it for its type, Dynamic mode or Remove. A dynamic band moves its gain (by its
    Range) when its frequency gets louder than its Threshold, in this track or the
    sidechain; the bar on its handle shows how far it can move and the ring where
    it is.
  - **Compressor:** Clean, FET (fast, aggressive), Opto (smooth, slow release
    after long reduction) or Vari-Mu (gentle, widening knee). The transfer curve
    shows where the sound sits (drag it for the threshold, scroll for the ratio);
    the graph shows the last seconds of level and gain reduction.
  - **Clipper:** a mastering clipper (hard, smooth knee or tanh) with up to 16×
    linear-phase oversampling and a ceiling held as true peak by default. The
    display shows what's clipped off in red, with clipping, true-peak and
    "louder by" readouts; Delta plays only what's clipped off.
  - **Limiter:** a true-peak brickwall with 2 ms lookahead and auto release.
    Effects that look ahead show how late they play beside their name; the
    other tracks are delayed to match, so everything stays in time.
  - **Multiband:** three bands (OTT-style by default) that compress downward above
    one line and upward below the other: drag the splits and the lines.
  - **Ducker:** precise sidechain ducking. Hits: every time the sidechain hits,
    the sound ducks along the drawn shape (attack, hold, release, curve), the same
    each time; Level: it follows how loud the sidechain is; Tempo: it ducks on the
    beat with no sidechain. Lookahead ducks before the hit arrives.
  - **Waveshaper:** soft and hard clipping, tube, foldback, sine fold, rectify and
    bitcrush, oversampled, with the curve and a before/after scope.
  - **Shaper:** draw a shape for volume, a filter's cutoff or pan, at a note value
    in time with the song (right-click the shape for ready-made ones).
  - **Vocoder:** the track's sound (a synth) speaks with the sidechain's (a voice).
  - **Reverb:** the display shows the decay; drag it, or the dot for the pre-delay.
  - **Delay:** synced to the tempo or free; the display shows the echoes (drag
    sideways for time, up and down for feedback).
  - **Analyzer:** MiniMeters-style spectrum, waveform colored by frequency,
    oscilloscope, stereo goniometer with correlation, loudness (EBU R128: momentary,
    short-term, integrated, range, true peak) against a target, and levels. View
    picks what's shown and how; Pop Out opens it in a window that stays open
    whatever track is selected.
- **Automation:** right-click any control (a synth parameter, an effect's, the
  envelope, volume, pan) → Automate to open its lane; a track can show several. The A button shows or
  hides a track's lanes. Double-click to add a point, drag to move, double-click a
  point to remove it. Drag the dot in the middle of a segment to bend it. Press B
  (or hold Ctrl) and drag to draw.
- **Editing automation:** click points to select them (Shift or Ctrl to add more),
  or drag a box on empty lane space. Dragging a selected point moves them all;
  Delete removes them; right-click → Invert or Reverse Points. Right-click →
  Generate fills the selected span (or the bars in view) with a sine, triangle,
  saw, square, or random steps or glides, at a period from 1/32 to 4 bars, between
  a low and high level, with a phase; it previews as you change it, Apply keeps it
  as one undo step, and Reroll gives new random values.

### Drag and drop on Wayland

winit, which eframe uses for windows, doesn't deliver dropped files on Wayland, so
canopy receives them itself (`crates/ui/src/wayland_drop.rs`): it shares winit's
Wayland connection and listens with its own data device. KWin sends drags only to
a client's first data device, so `main` creates canopy's before the window (and
egui's clipboard, which ignores drags) exists. `CANOPY_DEBUG_DROPS=1` logs what it
sees; `CANOPY_NO_WAYLAND_DROPS=1` turns it off.

`tools/wayland-drop-test/run.sh` checks the whole path for real: it starts a
headless KWin, runs canopy in it, and drags a WAV onto it with KWin's fake input.

## Shortcuts

| Key                     | Action                                    |
| ----------------------- | ----------------------------------------- |
| Space                   | Play / stop (stop again to go to the start) |
| R                       | Record from a MIDI keyboard on / off      |
| Ctrl+M                  | Metronome on / off                        |
| Home                    | Move the playhead to the start            |
| Ctrl+Z / Ctrl+Shift+Z   | Undo / redo (Ctrl+Y also redoes)          |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste clips (notes, effects) |
| Ctrl+D                  | Duplicate selected clips (notes, effects) |
| Ctrl+E                  | Split selected clips at the marker        |
| 0                       | Mute / unmute selected clips              |
| B                       | Draw mode for automation lanes (in the piano roll: Bend mode) |
| Left / Right            | Nudge selected clips (or notes) by the grid |
| Up / Down (Ctrl, Shift) | Transpose selected notes by 1 (an octave) |
| Ctrl+A / Esc            | Select all / none                         |
| L                       | Loop on / off (in the piano roll: the clip) |
| Shift+L                 | Loop the selected clips (or notes)        |
| Z / Shift+Z             | Zoom to the selection / the whole song    |
| Delete                  | Delete selected clips, notes, tracks or effects |
| Ctrl+N / Ctrl+O         | New / open project                        |
| Ctrl+S / Ctrl+Shift+S   | Save / save as                            |
| Ctrl+I                  | Import audio                              |
| Ctrl+R                  | Export audio                              |
| Ctrl+T / Ctrl+Shift+T   | New audio / MIDI track                    |
| Ctrl+G / Ctrl+Shift+G   | Group selected tracks / ungroup           |
| Ctrl+4                  | Snap to grid on/off                       |
| Ctrl+1 / Ctrl+2         | Finer / coarser grid                      |
| Shift+Tab               | Switch clip / device view                  |
| Tab                     | Move the keyboard focus to the next control |
| F1                      | Keyboard shortcuts                        |
| Ctrl+wheel              | Zoom time                                 |
| Alt+wheel               | Zoom track height (or piano key height)   |
| Shift+wheel             | Scroll sideways                           |
