# canopy: notes for agents

A personal DAW in Rust (egui + cpal), Linux first, Windows via a static build.
Everything an agent needs to orient is here, so read this before exploring.
Longer references: `README.md` (layout, threading rules, every feature from the
user's side), `docs/progress.md` (where each workstream stands; **update it when
you finish or stop a piece of work**), `docs/plan-routing-modulation-plugins.md`
(the patcher/modulation/plugin architecture; section 11 is its status),
`PLAN.md` (the original timeline design and what changed from it), `IDEAS.md`.

## Build, run, test

```sh
cargo run                                   # the DAW (debug: DSP at opt-level 1, deps at 3)
cargo run --release
cargo run --release -p learn                # the keyboard-learning app
cargo test --workspace                      # everything; UI tests render headlessly (software Vulkan)
cargo test -p dsp -p engine --no-default-features   # no ALSA needed
cargo test -p ui ui_tests                   # the headless end-to-end app tests
CANOPY_SCREENSHOTS=<dir> cargo test -p ui tour      # screenshot tour of every view
cargo test --release -p effects --test performance -- --nocapture   # CPU per effect
cargo test --release -p instruments --test cpu -- --ignored --nocapture      # CPU per instrument
cargo run --release -p engine --example golden -- out.txt   # hash every device's output (see "Shared code")
cargo build --profile dist                  # what ships (fat LTO, 1 CGU, line tables)
tools/build-windows.sh                      # static canopy.exe via cargo-xwin (see README)
tools/package-clap.sh [--install|--windows] # dist/clap[-windows-x86_64]/canopy.clap: built-in instruments as CLAP
cargo run -p flp --example flp_dump -- [--model|--import] x.flp
cargo run -p plugins --example plugin_cli -- scan | params <name> | render <name> out.wav --note 60 | edit <name>
```

- Rust edition 2024, toolchain stable (1.98 at the time of writing). `let` chains are used.
- Env-gated tests: `CANOPY_FLP_SAMPLES=<dir of .flp>` (flp real-file tests),
  `CANOPY_PROJECTS=<dir of .canopy>` (project round-trip), `CANOPY_TEST_PLUGINS`
  (real plugins), `CANOPY_SCREENSHOTS=<dir>` (UI tests save PNGs).
- Timing tests (`instruments/tests/cpu.rs`, `effects/tests/performance.rs`, the cost test in
  `effects/tests/modulation.rs`) only assert limits in release builds; some are `#[ignore]` and
  need `-- --ignored`. Each test binary's timings take a lock (`common::timing()`), so they never
  run at once.
- The `pipewire` and `asio` features switch the audio backend; default is cpal/ALSA (or WASAPI).
- Build output: the default `target/` is 50 GB and fine to use. If you run several
  agents, give each `CARGO_TARGET_DIR=target/<agent>` so they don't block each other
  on the build lock. Never build into `/tmp` (RAM-backed, 16 GB). Run at most 2–3
  agents at once (6–7 in parallel has hit the usage limit).
- **Git:** the repo is on GitHub (`Matipolit/Canopy-vibe`, public, branch `main`), first pushed
  19 Oct 2026. Commit and push only when the user asks. `fl projects/` and `demos/` stay local
  (see `.gitignore`).

## Crates (workspace, `crates/*`)

Dependency order, lowest first. Each crate's `src/lib.rs` has a doc comment saying what it is.

| crate | what | notes |
|---|---|---|
| `midi` | `Pitch`, `Velocity`, `Tick` (960 PPQ), `NoteEvent`, raw message parsing | `serde` feature |
| `dsp` | the shared building blocks: `svf` (TPT state-variable filter, `Coefs`, `Crossover`), `one_pole` (TPT one-pole, `DcBlocker`), `noise` (xorshift), envelope (`Adsr`, `Segment`, `curve_coefficients`, `stage_shape`), `Smoothed` (with `Pace`, `MAX_BLOCK`), oscillator (`poly_blep`), gain, and small helpers in `lib.rs` (`flush`, `settle`, `catmull_rom`, `rational_tanh`, `note_hz`) | pure, no threads |
| `instruments` | `Instrument` trait, `ParamDescriptor` (+ `float`/`choice` builders, `param_value`, `checked_value`, `segments`), built-ins: `poly_synth`, `wavetable/` (the big one), `drums/`, `sampler/` (.sfz), `guitar/` (+`amp.rs`), `oscillator` | `builtin/mod.rs` lists them (`find_builtin(id)`) and holds the shared `voice_to_steal` |
| `effects` | `Effect` trait + built-ins (eq, compressor, limiter, clipper, multiband, ducker, reverb, delay, chorus, distortion, multifilter, hyper, freqshift, convolve, transient, utility, waveshaper, shaper, vocoder, amp, analyzer), and what they share: `dynamics.rs`, `meter.rs`, `lookahead.rs`, `truepeak.rs`, `halfband/`, `shaping.rs`, `line.rs` | `lib.rs` header documents the modulation/smoothing rules every effect follows |
| `project` | the document: `Project`, `Track`/`TrackKind`, clips, notes, automation, devices, routing, modulation, `History` (undo), RON `file.rs` with `repair()`, `note_tools/` | no threads/audio/IO; `edit.rs` and `devices.rs` hold the edits |
| `media` | decode (symphonia) on worker threads, resample, peaks, `process.rs` (reverse/loop/stretch via ssstretch), `renderer.rs`, `wav.rs` | opt-level 3 even in dev |
| `engine` | the audio thread: `arrangement.rs` (`Compiler`: Project → `Arrangement` snapshot, with the render `Schedule`), `audio.rs` (`Engine::render`, `render_node`, `MAX_BLOCK = 64`), `pool.rs` (worker threads and the lock-free ready queue; `default_workers`), `chain.rs` (effect slots, meters, taps), `compensation.rs`, `fader.rs`, `live.rs` (MIDI in), `bounce.rs` (offline export), `device.rs` (the only cpal user), `graph.rs`/`modulation.rs` (stubs for the patcher and modulation) | |
| `plugins` | CLAP/VST3/VST2 hosting library: scan (separate process, cache in `~/.cache/canopy/plugins.json`), instances, RT processing, state, editors (X11/Win32) | knows nothing of canopy |
| `hosting` | plugins as canopy devices: `PluginManager` (an instance per plugin device/instrument, kept in step with the project by `pump`; state, undo, restarts, editors' own edits as `Update`s), `PluginEffect`/`PluginInstrument` adapters, `describe.rs` (leaked descriptors, `ParamMap`), `Catalog` (background scan), `WithPlugins` (adds plugins to any `AudioLookup`), `Offline` (instances for exports/freezes) | UI thread = plugins' main thread |
| `canopy-clap` | the built-in instruments as CLAP plugins for other hosts: one `cdylib` (`canopy.clap`), `bridge.rs` runs an instrument as the engine does, `values.rs`/`state.rs`/`plugin.rs` | `tools/package-clap.sh [--install]` |
| `flp` | FL Studio `.flp` import: `reader.rs` → `parse.rs`/`model.rs` → `plugins.rs` (stand-ins) → `import.rs` (`Project`); `vital.rs`, `vst.rs`, `native.rs` read saved plugin states; installed third-party plugins come in as themselves (`Choice::Plugin`, `vst::hosted_state`) | |
| `ui` | the egui app, binary `canopy` (`main.rs`, `app.rs`) | see below |
| `learn` | the keyboard-learning app, binary `canopy-learn`, on the same engine | mostly pure, unit-tested logic |

### `ui` map

- `session.rs`: owns `Project` + `History` + `EngineHandle` + loaded audio. `Session::edit(label, |p| ..)`
  is **the** way to change the project (one undo step, marks `needs_rebuild`); `begin_live`/`end_live`
  bracket drags. `update()` runs once per frame: garbage, loads, banks, MIDI, then `rebuild()`
  (compile + send snapshot) if anything changed, then meters and taps.
- `app.rs`: `CanopyApp::ui` is the frame: panels, detail views (`Detail::{Clip, Piano, Device, ...}`),
  shortcuts, windows. Only requests repaints while playing or animating.
- `timeline/` (ruler, headers, lanes, clips, automation, interaction, viewport), `piano_roll.rs` +
  `piano_roll/{tools,pattern}.rs`, `clip_panel.rs`, `device_view.rs`, `effect_rack.rs`.
- `devices/`: one panel per effect/instrument, all built on `devices/panel.rs` (`Panel`: knobs,
  choices, switches with automation menus, meters, taps). `devices/wavetable/` is the synth UI.
- `widgets/`: hand-painted controls. `knob.rs` (ring API for modulation), `chrome.rs` (shared small
  pieces: icon buttons, toggles, menu items, focus ring, drag labels), scopes, spectrum, envelope…
- `theme.rs`: the design system (`space`, `radius`, `stroke`, `text`, `size`, `anim`, `color`,
  and `surface`, the palette every egui colour is built from). Fonts are Inter and JetBrains
  Mono from `ui/assets/fonts`; `text::medium`/`semibold` for heavier weights.
  Use its names; never invent literal sizes. Animations use `theme::{hover,fade,glide}` which
  request frames only while moving.
- `renders.rs` (processed-clip renders), `banks.rs` (.sfz/wavetable loading), `taps.rs`,
  `export.rs`, `midi_input.rs`, `presets.rs` (factory presets, ~3000 lines), `flp_import.rs`.
- `plugins/`: plugin menus (top of "+ Effect", beside the instrument picker), the plugin panel
  (searchable knobs with the plugin's own value text), the Edit button for editor windows, the
  missing-plugin card, View → Plugin Manager. `pump` runs before `Session::update` every frame.
- Stubs awaiting the next workstreams: `patcher/` (`READY = false`), `modulation/`, `fx_view.rs`.
- `ui_tests/`: headless end-to-end tests with `egui_kittest` (`Rig`), one file per area, plus `tour.rs`.

## Shared code: use it, don't copy it

Instruments and effects were written one at a time and had grown copies of the same algorithms;
those now live in one place each. Reach for them before writing a new one:

- **Filters**: `dsp::svf` (TPT/Simper SVF: `Coefs::new`/`butterworth`, `SvfState::tick`/`outputs`,
  `Shape` for gliding responses, `Crossover` for Linkwitz-Riley splits, `prewarp`, `tan_fast`),
  `dsp::one_pole` (`pole`, `tpt_lowpass`, `DcBlocker`). Denormals: `dsp::flush`, `dsp::settle`.
- **Noise**: `dsp::xorshift32`/`noise::xorshift64`, `noise::unit`/`bipolar` for floats.
- **Envelopes and curves**: `dsp::Segment` is the curved stage both `dsp::Adsr` and the
  wavetable's `env.rs` run; `dsp::curve_coefficients` the curve the mod matrix and LFOs bend by.
- **Several at once**: `dsp::simd::F32x4` (SSE on x86-64, an array elsewhere; lane by lane the
  same IEEE arithmetic, so bit-identical to scalar code) with `transpose`, and its `exp2`/`log2`
  (within 3e-7) for dB gains a block at a time. `dsp::split_index` for a float position's index
  and whole part: `as usize` on a float costs a dozen instructions per sample on x86.
- **Smoothing**: `dsp::Smoothed` everywhere (`set_target` at the knob's pace, `modulate` within
  `dsp::MAX_BLOCK`, `move_to(.., Pace)`, `settle()` to jump to the target).
- **Effects**: implement `Effect::receive(index, value, pace)` (`set_param`/`modulate` call it),
  keep values in `Values<COUNT>` (it tracks freshness: `played()` in `process`, `refresh()` in
  `reset`), and end descriptors with `..EffectDescriptor::BASE`. Meters: `meter::PeakMeter`/
  `RmsMeter`; lookahead gain: `lookahead::Lookahead`; oversampled shaping: `shaping::Oversampler`;
  delay lines and glides: `line.rs` (`Ring`, `Tap`, `slew`, `contain`).
- **Instruments**: `checked_value` at the top of `set_param`, `segments(events, len)` to split a
  block at its events, `builtin::voice_to_steal` for the steal policy, `float`/`choice` for
  descriptor tables.
- **Tests**: `effects/src/testing.rs` (unit tests), `effects/tests/common/` and
  `instruments/tests/common/` (integration tests: noise, events, levels, spectra, timing).

**Refactoring proof**: `cargo run --release -p engine --example golden -- before.txt` hashes
every device's output and meters over ~24,000 renders (every enum choice, every float swept and
modulated). Run it before and after; `diff` must be empty. Merging two near-copies is only a
refactor if it is: several remaining pairs (`bessel_i0`, the guitar amp's filters, the chorus's
sine, the struct-of-arrays SVFs in the vocoder and reverb) differ in rounding or layout on
purpose and stay apart.

## Architecture rules (don't break these)

1. **The UI thread owns the `Project`.** The engine plays a compiled, immutable `Arrangement`
   (ticks already converted to samples, track tree flattened post-order, one bus per group, bus 0
   is master). Every committed edit recompiles and sends a whole snapshot via one `rtrb` queue.
2. **Nothing is allocated or freed on the audio thread or its workers.** `Engine::process_interleaved`,
   every `Instrument::process`, `Effect::process`, `set_param`, `modulate` are RT-safe: allocate in
   `new`/`prepare`. `assert_no_alloc` aborts debug builds otherwise, and `*/tests/realtime*.rs`
   test it (snapshot swaps included). Old snapshots go back to the UI through a garbage queue.
   Tracks render in parallel (`pool.rs`): each node into its own buffer, groups summing their
   inputs, dependencies from the compiled `Schedule`; the output must stay bit-identical to a
   single thread (`engine/tests/parallel.rs`). Anything shared between nodes during a block
   (sends, other nodes' outputs) goes through the schedule's ordering, never a lock.
3. **Snapshots don't reset sound.** Processors (instruments, effect instances by `DeviceId`, faders,
   compensation delays) are moved from the old snapshot into the new one; new ones are built on the
   UI thread. `Compiler` keeps `live`/`devices` maps for this.
4. **Live gestures send small `Command`s** (fader, knob, seek) and only commit (one undo step, one
   rebuild) when the gesture ends.
5. **Undo = whole `Project` clones** (`project/src/history.rs`, 200 steps). Every model type must be
   `Clone + PartialEq` and cheap to clone (audio lives in `media` behind `Arc`s, not in the project).
   Clips' notes and automation points are `project::Shared` (copy-on-write: a mutable borrow copies
   only if a copy of the project still shares them), so a step costs what it changed.
6. **Ids, not indices.** `TrackId`, `ClipId`, `NoteId`, `DeviceId`, `NodeId`, `ModulatorId`… all come
   from one counter (`Project::alloc_id`) and are never reused, even across undo.
7. **Time:** musical positions are `Tick`s (960/quarter); audio clip offsets/lengths are frames at the
   file's own rate; only `TempoMap` converts. Automation and modulation resolve per 64-frame block.
8. **Effects' parameter rules** (see the `effects/src/lib.rs` header): `set_param` glides ~20 ms,
   `modulate` ramps within one block, filters use TPT/SVF forms, delay taps never jump, enum switches
   crossfade. `effects/tests/modulation.rs` sweeps every parameter of every effect to enforce this.
9. **File format:** RON, `{ format: N, project }`. Format 2 is written only when a project uses
   routing/modulators/plugins; everything else must re-save byte-identically as format 1. Bump
   `FORMAT_VERSION` only when old files would load wrongly; `repair()` fixes what it can on load.
10. **Every chain or graph edit ends with `normalize_routing(owner)`** (a linear graph becomes `None`);
    `project/tests/fuzz.rs` checks it.
11. **Parameters are addressed by `AutomationTarget`** (`Volume`, `Pan`, `InstrumentParam`,
    `DeviceParam`, `ModulatorParam`), shared by automation, MIDI learn and modulation.
12. **Descriptors are `&'static`.** Effects and instruments describe themselves with static
    `ParamDescriptor` tables (log/linear mapping, units, defaults); UI, automation, presets and
    the FL importer all read them. Plugins register leaked descriptors via `register_external`.

## Conventions

- Comments and docs are written as prose for a reader, in the voice of the existing code: full
  sentences, say *why*, no "we"/"I", no TODO noise, no restating the code. Keep that voice.
- Tests are named as sentences (`fn stop_returns_to_where_play_started()`), live next to the code
  for units and in `tests/` for behaviour. New behaviour gets a test; UI behaviour gets a `ui_tests` case.
- Device panels take sizes from `theme` and shared pieces from `widgets::chrome`/`devices::panel`.
- Pin C-ABI crates exactly (`clap-sys`, `vst3`, `libloading`, `libc`, `x11rb`, `windows-sys`).
- `README.md` is the user-facing reference: when a feature or shortcut changes, update its
  "Using it"/"Shortcuts" section. `docs/progress.md` is the agent-facing status: update it too.
- Don't widen scope silently; the user sets priorities in `docs/progress.md`.

## Where things stand (short; details in `docs/progress.md`)

Done: Wavetable synth (Serum/Vital-class), Serum-style effects, physical guitar + amp, plugin
hosting (CLAP, VST3, VST2) wired into the DAW (12 Oct 2026), Windows static build, UI
polish/design system (third pass 28 Oct 2026: fonts, palette, transport, cards), strum
pattern tool, FL importer, a first performance pass (11 Oct 2026: benchmarks, a profiler, the per-track and per-clip waste), and a second (28 Oct 2026: engine
−23%, opening −57%, UI frames −35–65%, undo memory −95%).
FL imports bring installed third-party plugins in as themselves (12 Oct 2026).
The built-in instruments (all but the Sampler) package as one CLAP file (19 Oct 2026).
Next big build: patcher (A), modulation engine (B), per plan sections 8 and 11. Then the
remaining FL import fixes.

## Measuring performance

Measure before and after; never guess. The numbers below are from the development laptop
(i7-1365U, a P-core; pin with `taskset -c 0` for repeatable runs, and run nothing else).

```sh
cargo run --release -p engine --example bench                 # 33-track song: clone, save/load,
                                                              # compile, and render as % of a core
cargo run --release -p engine --example bench -- --poly 32 --busy   # compute-bound scaling test, paced
                                                              # like a sound card (CANOPY_AUDIO_THREADS=n)
cargo run --release -p engine --example bench -- --save target/bench   # also writes song.canopy + wavs
CANOPY_BENCH_PROJECT=target/bench/song.canopy cargo test --release -p ui perf -- --ignored --nocapture
                                                              # UI frame times, view by view, at 1920×1080
cargo run --release -p effects --example profile_effect -- canopy.reverb 10 [param=v] [--repeat n] [--out f] [--compare f]
cargo run --release -p instruments --example profile_instrument -- canopy.wavetable 10 [--notes 48,52] [--out f] [--compare f]
                                                              # one device: time it, profile it (--repeat),
                                                              # and prove a change is bit-identical (--out, then --compare)
cargo test --release -p effects --test performance -- --nocapture       # CPU per effect
cargo test --release -p instruments --test cpu -- --ignored --nocapture   # each instrument against the Poly Synth
```

`perf` is not allowed on this machine (`perf_event_paranoid` is 4). For where the time goes,
preload `tools/pcprof.c` (a CPU-time timer in the process notes the running instruction; docs
at the top of the file) and summarize with `tools/pcprof_report.py`: it runs at full speed and
its shares are right. For call stacks, the gdb sampler `tools/sample.py` (and
`tools/sample_summary.py`), but distrust its shares for glibc's maths (`sinf`, `exp2f`, `cos`):
stopping the program from gdb makes those look ten times dearer than they are (28 Oct 2026).
Build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` first so addresses have names. For
a test binary under gdb set `SAMPLE_THREAD` to the test's name, or every stop walks all of
wgpu's threads.

Reference numbers (28 Oct 2026, engine): the bench song renders at ~17% of a core
single-threaded and ~10% with the default one worker on this laptop (2 P-cores + 8 E-cores:
more workers are slower here, see progress.md section 12; `CANOPY_AUDIO_THREADS=n` to
experiment); a first compile is ~17 ms (~35 ms pinned to one core; the wavetable bank fills in
the background), later ones ~0.3 ms; the bench reports the audio thread's own CPU time too,
which other programs taking turns with it don't inflate. UI frames at 1920×1080 are
0.6–1.7 ms of CPU plus ~0.5–0.9 ms tessellating (28 Oct; `ui_tests/perf.rs` prints CPU time,
which other programs don't inflate, beside the wall clock). Where the time goes and what's left is in `docs/progress.md`, section 12.
`-C target-cpu=x86-64-v2/v3` was measured and gained nothing: don't bother. When optimizing a
device, save its render first (`--out`), keep the change bit-identical (`--compare` says
"identical"), and A/B the timing against a copy of the old binary: single runs vary by ±5%.
Section 12 lists what was tried on each device and what was reverted.

## Gotchas

- `cargo test --workspace` takes a while (UI tests render frames); run a single crate or test name
  while iterating. UI tests use software Vulkan; `quiet_wgpu()` disables validation because parallel
  tests crashed in the Vulkan loader otherwise.
- Wavetable params live in `instruments/src/builtin/wavetable/params.rs` (2800 lines of descriptor
  tables with layout guards in tests): append, never reorder, ids.
- Guitar parameter ids 0–11 are fixed; 12–23 were appended. Same rule everywhere: parameter order is
  part of the file format for automation lanes and presets.
- `media` is opt-level 3 in dev (decoding at 1 is too slow); DSP in other crates is at 1.
- `--profile dist` (fat LTO) can compile a hot loop worse than `--release`: it stopped keeping
  the EQ's filter memory in registers, 30% slower, until the loop held it in locals (progress.md
  section 13). Time devices under both before concluding anything; recursive filters' state
  belongs in locals for the block.
- The user's memories: extend built-ins rather than approximate when an FL plugin needs a feature;
  look third-party plugins up online before mapping them in `flp/src/plugins.rs`.
