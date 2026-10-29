# Progress — 28 October 2026

Where every piece of ongoing work stands, so it can be picked up in a new session.

The architecture plan for the patcher, modulation and plugin devices is in
[plan-routing-modulation-plugins.md](plan-routing-modulation-plugins.md). Section 11 there is
the detailed status of that work.

## Read this first

- **Git:** first committed and pushed to https://github.com/Matipolit/Canopy-vibe (public,
  branch `main`) on 19 October 2026. `fl projects/` (1.3 GB of FL projects and stems) and
  `demos/` (112 MB of rendered WAVs) are ignored and stay local. Commit only when the user asks.
- **The workspace builds** (fixed 9 October 2026). The Wavetable agent had left three tests in
  `crates/instruments/src/builtin/wavetable/osc.rs` calling the warp functions without the new
  remap-curve argument. They now pass a straight and a bent curve.
- **Agents ran out of usage twice.** Six or seven agents in parallel hit the claude.ai usage
  limit twice. Run two or three at a time.
- **Build output.** Keep build output on disk (`target/<agent-name>`), not in the session
  scratchpad: `/tmp` is a RAM-backed tmpfs and filled up once. Cleaned up on 10 October 2026:
  the agents' build directories and the incremental cache are gone, and `target/` went from
  156 GB to 47 GB. Every before/after screenshot from the UI, Wavetable and effects work is now
  in `target/screenshots/`. Cleaned again on 19 October 2026 (88 GB to 11 GB): `target/debug`
  (50 GB of stale test binaries) and the finished experiments' folders (`lead`, `vst2`,
  `orig`, `bisect`, `cpu-*`, `dist-*`, `lt-release`, `screenshots`) are gone. Kept: `release`,
  `dist`, the Windows builds, `test-plugins`, `bench`, `backup`.
- **Agents from this session can't be resumed** in a new one. Start fresh agents with briefs that
  point at this file and the plan.

## The user's priorities

The user set these on 8–9 October 2026.

1. **Wavetable synth**: a full Serum 2 / Vital replacement.
   - Polished, visually responsive UI.
   - The effects people love from Serum, built as external track effects that the synth's LFOs
     and macros can drive (a general modulation system).
   - A robust LFO and macro system.
2. **A patcher-like interface for effects**, so they can be routed in multiple paths.
3. **Guitar**: "100% believable as a real guitar". Use reference recordings found online; the
   exported stems contain no plain guitar.
4. **A static Windows build.**
5. **FL import strategy.**
   - Rather than imitating complex third-party instruments, host the real ones (CLAP and VST3;
     on Linux, Windows plugins come in through yabridge).
   - FL compatibility fixes come later. Known bugs: audio clip speeds are wrong, and drum audio
     clips get imported as sforzando devices.
   - Reference stems are in `fl projects/right_here_2_export_stems/`, named by mixer track.
   - The user wonders whether perfect FL compatibility is even a sensible goal.
6. **After everything else**, a holistic performance pass: audio thread, UI thread, project and
   engine.

The user's standing preferences are in Claude's memory (`importer-extends-builtins`,
`verify-plugin-identities`): extend built-ins when an FL plugin needs a missing feature, and look
up third-party plugins online before mapping them.

## Workstreams

| # | Workstream | State | Where it stopped / what's next |
|---|---|---|---|
| 1 | Serum-style effects and the modulation-safety audit | **Done** | Leftovers below |
| 2 | Plugin-hosting library (`crates/plugins`) | **Done** | Not yet used by canopy; that's workstream 9 |
| 3 | Architecture plan | **Done** | `docs/plan-routing-modulation-plugins.md` |
| 4 | Foundation: plan phases 0–1 | **Done** | Interfaces, model, engine hooks, UI stubs |
| 5 | Wavetable synth to Serum 2 / Vital level | **Done for this round** (10 Oct) | Half-done work finished; roadmap leftovers below |
| 6 | Realistic guitar | **Done** | Leftovers below; listen to `demos/guitar/` |
| 7 | Static Windows build | **Done; runs on the user's PC** (12 Oct) | `tools/build-windows.sh`; CI first triggered by the push of 19 Oct |
| 8 | Patcher (plan agent A) | **Barely started** | Was starting the crossover in `dsp` |
| 9 | Modulation (plan agent B) | **Barely started** | Had read the code; the knob ring API already exists |
| 10 | Plugin integration (plan agent C) | **Done** (12 Oct) | Plugins play in the DAW; VST2 added. Untested with real third-party plugins and on Windows; leftovers below |
| 11 | FL import fixes | **Started** (12 Oct) | Installed plugins now import as themselves; the bugs in priority 5 remain |
| 12 | Performance pass | **Second round done** (28 Oct) | Engine, opening, undo, UI frames; leftovers listed |
| 15 | UI polish and UI performance, third pass | **Done** (28 Oct) | Theme, fonts, transport, cards; frames 35–65% cheaper |
| 14 | Built-in instruments as CLAP plugins | **Done** (19 Oct) | `crates/canopy-clap`, `tools/package-clap.sh`; leftovers below |

### 1. Effects — done

New built-in effects. Each has a panel with a live display, factory presets and tests.

- **`canopy.filter`** (`effects/src/multifilter.rs`): 26 filter types, including SVF, ladder,
  acid, dirty, comb, flanger, phaser and formant.
- **`canopy.distortion`**: 17 modes, a filter before or after, and oversampling (shared halfband
  code in `halfband/`).
- **`canopy.hyper`**: Hyper/Dimension.
- **`canopy.frequency_shifter`**: a Bode-style frequency shifter. The FL importer maps
  MFreqShifter to it.
- **`canopy.convolve`**: partitioned convolution with 8 generated impulses.
- **`canopy.reverb`** gains Plate and Room types and modulation parameters. Hall sounds exactly as
  before.

Other changes:

- **`Effect::modulate` / `Effect::retire`.** Every effect implements fast modulation, ramped
  within one 64-frame block. `set_param` keeps its ~20 ms knob smoothing; the rule is documented
  in `effects/src/lib.rs`.
- **Modulation sweep test.** `effects/tests/modulation.rs` sweeps every parameter of every
  effect. It found and fixed:
  - jumps in the utility's side delay and in the delay's time;
  - a panic in the chorus;
  - spikes at block edges in the multiband;
  - runaway pitch in the plate reverb;
  - two errors in the frequency shifter's sideband math.

**Left to do:**

- loading the user's own impulse files into Convolve (effects have no data-loading mechanism
  yet);
- Serum's splitters (the patcher's split nodes will cover them);
- the Vintage, Basin and Nitrous reverb types;
- an X-Shaper-style drawable curve.

### 2. Plugin hosting (`crates/plugins`) — done

- **What it covers:** CLAP (`clap-sys` 0.5.0), VST3 (`vst3` 0.3.0, MIT) and, since 12 October
  2026, VST2 (a clean-room ABI in `src/vst2/abi.rs`) effects and instruments on Linux and
  Windows:
  - scanning in a separate process, with a cache in `~/.cache/canopy/plugins.json`;
  - real-time-safe processing with sample-accurate notes and parameters;
  - CLAP `PARAM_MOD`, emulated for VST3;
  - state, latency and restarts;
  - editors in separate X11 or Win32 windows, with keep-on-top;
  - the Linux run loops: CLAP timers and fds, VST3 `IRunLoop`.
- **Tests:** against fixture plugins in `crates/plugins/test-plugins/` (`clap-gain`,
  `vst3-gain`, `vst2-gain`), and Vital (installed here) in all three formats. Not tested:
  yabridge/Wine, running on Windows.
- **VST2** (12 October 2026, `src/vst2/`): ids are the unique id as 8 hex digits (only unique per
  vendor); values normalized; parameter text only for the current value; state in a container
  of ours (`CNPYVST2`: the bank chunk, or every value plus the program); blocks split at
  parameter events; editors driven by `effEditIdle` at ~30 Hz from `Host::idle`. A VST2 library
  is unloaded when its last instance closes, because JUCE's VST2 wrapper hangs closing a new
  instance in a library that stayed loaded (found with Vital). Shell plugins (Waves) and
  accumulate-only plugins are refused. The crate's README has the details.
- **Try it:** `cargo run -p plugins --example plugin_cli -- scan | params <name> | render <name>
  out.wav --note 60 | edit <name>`. yabridge setup is in `crates/plugins/README.md`.
- **API summary:** in the agent's report, condensed in plan section 11 and the crate docs.
  `Host` (main thread, `idle()` / `set_waker`), `PluginInstance`, `Processor` (audio thread,
  `process_stereo`), `PluginEvent`, `plugins::scan`, and `plugins::fixtures::build()` for other
  crates' tests.

### 3 and 4. Plan and foundation — done

The foundation built:

- **Interfaces:** `Instrument::{modulate, retire, mod_outputs}`, `ModOutput`, `Format` (BuiltIn,
  Clap, Vst3), and external descriptor registries in `effects` and `instruments`.
- **Project model:**
  - new fields: `Track::{routing, modulators, modulations}`, `Project::master_routing`,
    `Device::plugin` and `TrackKind::Midi::plugin`;
  - `routing.rs`, `modulation.rs` and `plugin.rs`;
  - file format 2, written only when a project uses these features; old projects re-save
    byte-identically;
  - repair on load, and fuzz invariants.
- **Engine:** `graph.rs` and `modulation.rs` as stubs; `AudioLookup` plugin hooks; a `retire()`
  pass at swap; the new commands.
- **UI stubs:** `ui/src/patcher/` (`READY = false`), `ui/src/modulation/`, `ui/src/fx_view.rs`,
  `ui/src/plugins/`.

**The rule for phase 2 and 3 work:** every chain or graph edit ends with
`normalize_routing(owner)`. Plan section 11 lists exactly what each later agent must keep.

### 5. Wavetable synth — done for this round (10 October 2026)

Owns `crates/instruments/src/builtin/wavetable.rs` and `wavetable/*`,
`crates/instruments/tests/wavetable*`, `crates/ui/src/devices/wavetable/`, `widgets/knob.rs`
(the additive ring API), `widgets/dahdsr.rs`, `widgets/node_editor.rs`,
`ui_tests/wavetable_features.rs` and the Wavetable block of `ui/src/presets.rs`.

- **What the synth has:**
  - three unison oscillators with two warp slots each (22 warps, remap curves among them),
    12 spectral morphs, unison stacking and five detune modes, a sub and noise;
  - two filters (19 types) in serial or parallel, with per-source routing;
  - six DAHDSR envelopes and eight LFOs (stock, drawn steps, nodes, chaos; five modes);
  - eight macros and a 64-slot matrix (curve, polarity, aux, bypass);
  - 129 destinations: nearly every knob, including the envelopes' times and sustain, the
    LFOs' rate, phase, smoothing, rise and delay, unison spreads, key tracking and volume;
  - `MOD_OUTPUTS` for the plan's modulation engine; DC blocking; 2x oversampling;
  - user wavetables, a table editor, an FX page, a preset browser and about 60 presets.
- **Finished on 10 October** (what the stopped agents had left half-done):
  - the destinations for envelope times and sustain, LFO phase, smooth, rise and delay,
    spreads, key tracking and volume (`EnvMods`, `LfoMods`). Volume follows the smoothed knob,
    so moving it under a Volume slot doesn't step;
  - the matrix's destination menu offers them: columns, then grids of envelope stages and
    LFO settings;
  - presets use them (velocity or key on the amp decay, random LFO phase, a macro on vibrato
    rise, aftertouch on volume). Four presets had attacks below the 0.5 ms floor;
  - tests: knob-versus-slot equivalence for every new kind of destination, layout guards for
    the appended destinations, a UI drag-and-drop test, and new destinations in the CPU test;
  - `voice.rs` takes `NoteStart` and `Waves` structs instead of `too_many_arguments`;
    the scratch tuning helpers are gone.
- **Roadmap items not started:**
  - Serum 2's other oscillator types (multisample, granular, spectral) and its arpeggiator
    and clip sequencer;
  - per-note MPE (pitch bend and slide per voice; they are channel-wide now);
  - more filter models (diode and others);
  - synth FX buses as extra instrument outputs (plan 7.4);
  - the envelope editor drawing modulated times;
  - the Vital importer mapping Vital's envelope and LFO destinations (`flp`, not the
    synth's files);
  - a written Serum 2 / Vital gap table.
- Modulating the track's effects from the synth's exported LFOs needs workstream 9: the
  handles exist, but the engine's `ModProgram` is still a stub.

### 6. Guitar — done (9 October 2026)

- **Strings:** a physical model with dual-polarization waveguide strings, Lagrange fractional
  delays, shelf loss filters fitted to recordings, dispersion allpasses, tension-modulation pitch
  glide and sympathetic strings.
- **Excitation and body:** pick or finger, with pluck-position notches, and a 64-mode body.
- **Models:** steel, nylon and electric. The electric has single-coil or humbucker pickup combs
  and loaded resonance.
- **Amp:** the new **`canopy.amp`** effect, shared with the electric. It has Bassman and JCM800
  tone stacks, a power stage with sag, cabinets and a microphone position.
- **Playing:**
  - six-string fretboard assignment;
  - strums, legato (hammer-ons, pull-offs, slides), bends and vibrato;
  - palm mute, harmonics and dead notes, by key-switches C0–D#0 or by parameter;
  - release noises and squeaks;
  - deterministic per-note variation.
- **Checked against real recordings.** The comparison tables are in
  `docs/research/guitar/comparison.md`.
- **Parameters:** ids 0–11 are unchanged; 12–23 were appended. The demos in `demos/guitar/`
  come from `crates/instruments/examples/guitar_demos.rs`.
- **CPU:** 2.2–3.5% of a core for six-string chords.

**Leftovers:**

- low wound strings lack inharmonicity above the 14th partial (needs a better dispersion filter);
- the nylon low E is too stiff and decays too fast;
- the electric's A2 upper partials ring about 8 s against 4–5 s recorded;
- there are few clean steel references;
- the README doesn't list the Amp effect;
- the FL importer could pick the electric model for Slayer-type plugins;
- aftertouch vibrato needs the `midi` crate to parse aftertouch.

The reference audio was only in the session scratchpad; re-download it from
`reference-recordings.tsv`.

### 7. Windows build — done; builds here

- **12 October 2026: the user ran the `dist` exe on their Windows PC, and it runs fine.**
- **12 October 2026:** clang is installed and cargo-xwin's Microsoft CRT/SDK cache is in place,
  so `tools/build-windows.sh` builds without the licence flag. With plugin hosting wired in
  (VST2 included), the `dist` build still makes one static `canopy.exe` (27 MB) importing only
  Windows' own DLLs. CI has never run because nothing is committed or pushed.

- **Toolchain:** `x86_64-pc-windows-msvc` with `+crt-static`, cross-built with cargo-xwin
  (installed).
- **To build:**
  1. `sudo apt install clang`.
  2. `tools/build-windows.sh --accept-microsoft-license`. The first build accepts Microsoft's
     CRT/SDK license; only the user may do that.
  3. The output is `dist/canopy-windows-x86_64/canopy.exe`. The script fails if the exe imports
     any DLL that doesn't ship with Windows.
- **No-license alternative:** `sudo apt install mingw-w64 && tools/build-windows.sh --gnu`.
- **Fixed for Windows:**
  - MMCSS audio-thread priority;
  - optional ASIO (`asio` feature);
  - drop position (`windows_drop.rs`);
  - no console in release builds;
  - an embedded manifest (per-monitor DPI, long paths), via `crates/ui/build.rs`;
  - a MIDI device that's in use is retried;
  - a test that was Unix-only.
- **CI:** `.github/workflows/windows.yml`. Instructions are in the README's "Building for
  Windows".
- **Limitations:** no device picker, no exe icon, and Windows paths don't relink on Linux. Notes
  for plugin hosting on Windows are in the agent's report: separate CRT heaps per plugin DLL,
  and DPI contexts for editors.
- **To test on Windows:** audio (status bar shows WASAPI), MIDI, drag-and-drop position,
  dialogs, save/open, FL import, high-DPI.

### 8–10. Patcher, modulation, plugin integration — next big build

Follow plan section 8 (phases 2–3), with these file owners:

- **A (patcher):** `project/src/routing.rs` and the routing parts of `devices.rs`;
  `engine/src/{graph,chain,compensation}.rs`; `dsp` crossover; `ui/src/patcher/*`;
  `ui/src/effect_rack.rs`.
- **B (modulation):** a new `crates/modulators`; `project/src/modulation.rs`;
  `engine/src/{modulation,fader}.rs`; `ui/src/modulation/*`; `ui/src/fx_view.rs`;
  `ui/src/devices/panel.rs`; `ui/src/device_view.rs`; the timeline automation target lists.
- **C (plugins):** a new `crates/hosting` glue crate; the lookups in `ui/src/renders.rs` and
  `ui/src/export.rs`; the plugin manager in `session.rs`; `ui/src/plugins/*`.

A and B each got as far as reading the code; nothing of theirs needs keeping. C is done: see
the next section.

### 10. Plugin integration — done (12 October 2026)

Plugins (CLAP, VST3, and now VST2) are devices in the DAW: effects in any chain (the master's
too) and instruments on MIDI tracks. Vital (installed here) plays through all of it in all three
formats.

- **`crates/hosting`** (new, the plan's glue crate):
  - `PluginManager` (UI thread, the plugins' main thread). `pump(&project)` every frame, before
    `Session::update`: runs `Host::idle`, picks up a finished scan, and reconciles the instances
    with the project. It loads one per plugin the project names (keyed by `Owner::Effect(DeviceId)`
    / `Owner::Instrument(TrackId)`), keeps the last 16 removed ones so undo gets the same instance
    back, and loads a state when the project's blob changes (undo, preset, paste; compared by
    `Blob` identity, then bytes). Pushing the project's values sends only those that differ from
    the plugin's own, so momentary "reset" switches don't fire. It returns `Update`s for the
    session: `Param` (live, inside a gesture), `Commit` (the gesture ends: one undo step named
    "Change <plugin>"; changes without gestures commit after 400 ms of quiet), and `Edit` (a
    preset loaded in the editor: values and state as one step; a state that changed on its own,
    debounced, ignored right after parameter moves since those dirty the state too).
  - Restarts: a plugin asking for one gets a new generation; the compile that follows can't
    activate it while the engine holds the old adapter, so the slot is silent; when the old one
    comes back in the garbage and is dropped (deactivating it), `pump` asks for another compile.
    `LatencyChanged` also asks for one. A plugin that won't activate is marked failed (shown on
    its panel) and not retried.
  - `describe.rs`: registers leaked descriptors per plugin (`effects`/`instruments::
    register_external`). Hidden and read-only parameters are left out. Enumerated parameters (and
    switches) with fewer than 128 steps become choices whose project value is the step index
    (`ParamMap` converts); everything else keeps the plugin's own units (CLAP plain, VST3/VST2
    normalized). This mapping is part of the file format now. The descriptors' `create` makes a
    pass-through or a silent stand-in; the engine always asks the lookup.
  - `adapter.rs`: `PluginEffect` / `PluginInstrument` around a `Processor`. Parameter changes and
    notes go into a buffer sized up front (params × 2 + 512 events), only changes are sent, and
    `modulate` becomes `ParamMod` relative to the knob's value. The transport comes from the
    engine (tempo, beat, playing) with bars counted as 4/4 (the block carries no time signature).
    Calls into plugin code run under `assert_no_alloc::permit_alloc`: plugins may allocate in
    their own process (many do) without aborting debug builds; canopy's code around them is still
    checked.
  - `Catalog`: scanning on a thread of its own at startup (`ScanCommand::current_exe()`, so
    `ui/src/main.rs` calls `plugins::scan::run_scanner_if_requested()` first thing; the Windows
    exe scans with itself). Duplicates (bridged and native, two folders) are listed once.
  - `WithPlugins` adds the plugins to any `AudioLookup`; `Offline` holds separate instances for
    exports and freezes (the live instance's current state, activated with `offline: true`).
  - `Instrument::set_position(beat, playing)` (new, default no-op) is called before every
    `process` by the engine and by `render_midi_clip`.
- **UI** (`ui/src/plugins/`): "Plugins ▸ vendor ▸ plugin (FORMAT)" at the top of "+ Effect" and
  beside the instrument picker; the plugin panel (knobs and choices with the plugin's own value
  text via `Knob::text`, a search box past 12 parameters, 24 at a time); "Edit" on the title bar
  toggles the editor window, titled "<track> — <plugin>"; "Touched: <param>"; the missing card
  (not installed / still looking / wouldn't load, with Plugin Manager… and Remove); View → Plugin
  Manager… (found, couldn't read, rescan, retry failed, keep editors on top, folders).
- **Session**: the compile lookup is `WithPlugins`; `replace_project` calls `manager.forget()`
  (ids restart per project); freezing renders a plugin instrument with an offline instance and
  copies plugin effects' data to the frozen track; presets keep a plugin's state
  (`presets::save/load` take and return a `Blob`).
- **Export**: projects that name plugins have each job's snapshot compiled on the UI thread
  (`Exporting::serve`, called from `Session::update`) with offline instances; the export thread
  renders clips missing renders first and asks through a channel. Projects without plugins
  export exactly as before.
- **Automation menus**: a plugin's parameters are grouped (`hosting::describe::param_groups`,
  a registry beside the descriptors, since `groups` is a plain function): by the plugin's own
  modules/units, else by the words names begin with ("Oscillator ▸ Oscillator 2 ▸ Detune"),
  sorted with numbers counted (`track_header::sort_groups`). Vital's 903 parameters make about
  20 submenus.
- **Real plugins**: `hosting/tests/real_plugins.rs` (ignored; `CANOPY_REAL_PLUGINS=<files>`)
  loads, plays, saves and reopens real plugins. Vital (installed here in all three formats under
  `/usr/lib/{clap,vst3,vst}`) passes as CLAP and VST2 (903 parameters) and VST3 (2983), ~70 ms
  to load.
- **Tests**: `hosting/tests/plugins.rs` (the fixtures through the engine: settings, live knob,
  save/reopen in VST3 and VST2, undo restoring state, an editor gesture as one undo step, a
  latency restart, not installed, offline render alongside the live instance, a VST2 instrument)
  and unit tests; `ui/src/ui_tests/plugins.rs` (effect from the menu with undo/redo, every format
  from the same menu, instrument from the picker playing a note, the missing card and Remove, a
  preset keeping state).

**Left to do:**

- Try more real plugins (Surge XT, u-he, yabridge-bridged Windows ones), and everything on
  Windows: nothing has run there yet.
- VST2 parameter choices are named by number (a VST2 plugin can only show its current value
  as text), and the panel's knobs show the plugin's text only while it matches.
- A plugin whose description is registered but that has no instance (it failed to load, but
  another instance of it loaded) is silent rather than passed through: the engine can't tell
  "restarting" from "absent".
- Note-effect plugins (arpeggiators) aren't offered: canopy has no note chain.
- Multi-out instruments play their main output only.
- Bars are 4/4 for plugins whatever the project's time signature.
- No per-plugin crash protection (in-process hosting, as the plan accepted).
- The FL importer could now load the real plugins an .flp names, with their saved state
  (plan section 9, question 14).

### 11. FL import fixes — started (12 October 2026)

The user's known bugs, not yet fixed:

- wrong audio clip speeds;
- drum audio clips turned into sforzando devices.

Compare against the stems.

**Done: installed plugins import as themselves** (plan section 9, question 14). FL's wrapper
state (records after a 32-bit kind) names the plugin: 51 is a VST2 unique id (`i32`, LE), 52 a
VST3 class id (16 bytes, in COM order when saved on Windows), 58 a CLAP id; 54/55/56 are the
name, path and vendor; 53 is the plugin's own state. Which identity counts goes by the path (a
VST2 may carry a class id too). `flp::parse` puts it in `VstInfo::plugin` (`flp::PluginId`), and
`flp::vst::hosted_state` turns record 53 into the host's state (`plugins::state`, now the one
definition of every format's state and id):

- CLAP: 4 bytes of FL's, then the plugin's stream (Vital's JSON, 777 KB in right-here_2).
- VST3: records after a 32-bit kind: 1 (64 bytes of FL's), 3 the component's state (2 in some
  plugins that have no 3), 4 the controller's.
- VST2: a 21-byte header (-9, flags with bit 3 for "chunk", -2, the chunk's length, a `u32`,
  the program), then the bank chunk or a parameter count and `f32` values. Checked against every
  VST2 in the user's projects.

`Choice::Plugin` ("the plugin itself") is offered and chosen by default in the dialog for every
plan item whose `PlanItem::plugins` are in the catalog (`flp_import::installed`); instances whose
build isn't installed fall back, one by one, to the built-in their name suggests, and the report
says so. Imported plugin devices carry only their state; `hosting` fills in their parameter
values from the loaded plugin (`Update::Fill`, not an undo step), since the engine would
otherwise set them to their defaults. `Project::add_device` now accepts a plugin that isn't
loaded yet (as a file can name one).

Tested: synthetic files (`flp/tests/synthetic.rs`: a CLAP, a VST2 without chunks and a VST3 not
installed), and `right-here_2.flp` with the real Vital here (ignored test
`ui_tests::flp_import::installed_plugins_come_in_as_themselves`, with `CANOPY_FLP_WITH_PLUGINS`
and `CANOPY_REAL_PLUGINS`): all 10 Vital channels load as the CLAP with FL's patches (16–58
values from the defaults, all different) and play.

Left: FL's automation of plugin parameters (its clips name the parameter by index), FL's
slot mix knob on plugin effects, and sidechains into plugin effects.

### 12. Performance pass — first round done (11 October 2026)

Measured first, then fixed what the profiles showed. `CLAUDE.md` ("Measuring performance")
has the commands; the numbers are from the development laptop (i7-1365U).

- **Benchmarks:** `crates/engine/examples/bench.rs` builds a 33-track song (8 Wavetables with
  unison, 4 drum kits, 2 guitars, 6 poly synths, 8 audio tracks, an EQ and compressor on every
  track, reverbs and a delay on the groups, a mastering chain, fader automation everywhere) and
  times undo's clone and compare, save and load, the first and later compiles, and rendering as
  a share of one core. `-- --save <dir>` writes it as `song.canopy` with its audio, which
  `ui_tests/perf.rs` (`#[ignore]`, `CANOPY_BENCH_PROJECT=<file>`) opens at 1920×1080 to time a
  frame of each view.
- **Profiler:** `perf` isn't allowed on this machine (`perf_event_paranoid` is 4), so
  `tools/sample.py` samples with gdb (SIGPROF from a thread, a backtrace per stop) and
  `tools/sample_summary.py` summarizes the folded stacks. Set `SAMPLE_THREAD` for a test binary,
  or a stop that walks every wgpu thread takes seconds.
- **What the profiles said.** The audio thread is in the DSP inner loops where it should be
  (Wavetable oscillators 24% of the render, the guitar's waveguide 17% for two tracks, reverbs
  11% for four, compressors 10% for 32); the per-track plumbing had the waste. The UI thread was
  spending most of a frame registering three egui widgets for every clip in view, and the EQ
  recomputing its response curve per pixel every frame.
- **Fixed:**
  - `engine/src/compensation.rs`: a track with no delay passes its block through (it still goes
    into the ring, so a delay that starts later fades in from real history); the ring is a
    power of two, so wrapping is a mask, not a division per sample.
  - `engine/src/fader.rs`: when nothing is ramping, one pair of gains per block instead of two
    cosines per sample.
  - `instruments/.../wavetable.rs`: the per-tick `Controls` are gathered only when a setting,
    the tempo, a remap curve, a ramping smoother or the pitch wheel changed (`controls_stale`);
    a `debug_assert` rebuilds and compares when skipping, so the debug test suite checks the
    cache stays right.
  - `instruments/.../wavetable/tables.rs`: the built-in bank's 19 tables are generated on
    scoped threads. The first compile with a Wavetable went from 200 ms to 70 ms (it was most
    of the hitch when opening a project).
  - `ui/src/timeline/lane.rs`: a clip's widgets (body, edges, handles, context menu) are
    registered only while the pointer is over it, or everywhere while a button is down, a
    gesture is in progress or a popup is open. `clip.rs` no longer collects the notes into a
    `Vec` per clip per frame.
  - `ui/src/widgets/eq_curve.rs`: the curves and the selected band's fill are kept in egui's
    memory (`Curves`) and recomputed only when a value, an offset or the size changes.
  - `ui/src/device_view.rs`: copies the track without its clips (`Track::without_clips`)
    instead of the whole track every frame.
- **Numbers** (before → after, same machine, unpinned):
  - render of the bench song: 24.4% → 22.8% of one core;
  - first compile (builds every processor): 200 ms → 70 ms; later compiles 0.3 ms; undo clone
    0.05 ms; save 8 ms and load 19 ms for a 3.5 MB file;
  - UI frames at 1920×1080: arrangement idle 2.3 → 1.9 ms, whole song in view 3.6 → 2.4 ms,
    playing 4.2 → 3.0 ms; piano roll 2.9 → 1.9 ms; device view 4.1 → 2.9 ms playing,
    3.7 → 2.5 ms stopped.
- **Tried and dropped:** `-C target-cpu=x86-64-v2` and `v3` (SSE4.1 rounding instructions,
  AVX2 and FMA) changed the render by less than 1%. The DSP loops are feedback chains that
  don't vectorize, and `floor`/`ceil` calls aren't where the time goes.
- **Multi-core rendering, done the same day** (`engine/src/pool.rs`; README "Threading
  rules"). Each node renders into its own `out` buffer, groups and the master sum their
  inputs, and a `Schedule` compiled with the snapshot (children before groups, sidechain
  sources before listeners unless that closes a loop, which falls back to reading the block
  before) runs the nodes from a lock-free ready queue on the audio thread plus a worker
  pool. Output is bit-identical to single-threaded (`engine/tests/parallel.rs`), sidechains
  are now exact whatever the track order, and sends are double-buffered for the late case.
  Measured on the i7-1365U (2 P-cores, 8 E-cores): one worker takes the bench song from
  21.3% to 12.0% of a core (1.8×); more workers made it slower, not through the scheduler
  (about a microsecond per block) but because the nodes themselves took up to 2.7× longer
  with the package clocked down and the E-cores 3× slower, so the default is one worker
  per physical performance core but the audio thread's (`default_workers`, from sysfs on
  Linux; a guess of half the hardware threads elsewhere), capped at 15, with
  `CANOPY_AUDIO_THREADS` to override. The ready queue starts each block with the dearest
  nodes (their last measured cost) so the block doesn't end waiting for a big one. Workers
  get `THREAD_PRIORITY_TIME_CRITICAL` on Windows (where cpal's audio thread runs under
  MMCSS), nothing special on Linux (nor does the audio thread).
  - **Scaling on uniform cores**, checked with `bench -- --poly 32` (32 Poly Synths, no
    effects: compute-bound, tiny state) pinned to the eight E-cores as a uniform testbed:
    1.9× with 1 worker, 3.4× with 3, 4.3× with 5, 5.6× with 7 (8 threads). The pool's own
    cost is about a microsecond a block; what stops the real song scaling here is the
    instruments' memory traffic and the 15 W power limit, which a desktop won't have.
  - **Waking workers.** Between callbacks workers park, and a parked thread takes 100 µs
    and more to come back (longer on a laptop whose cores went cold), so the first block
    of every callback ran without them: paced like a sound card (`bench -- --busy`) the
    gain fell to 1.5× here and 3.3× on the E-core test. Now the engine tells the pool when
    each callback starts, the pool keeps a running estimate of the period, and a worker
    about to park sleeps (`park_timeout`) until 250 µs before the next callback is due,
    then spins up to 450 µs to meet it; an early callback wakes it as before, and after a
    late one it parks until woken. That restored the back-to-back numbers (1.65× and
    5.6×). It costs a worker a few percent of a core while a project with two or more
    tracks is loaded, and nothing with an empty project.
  - **The laptop itself.** With the `powersave` governor and `balance_performance` EPP,
    the audio thread's own callback takes 2.2× longer when it sleeps between callbacks
    than back to back (2470 µs against 1090 single-threaded), because its core clocks
    down in the gap. That is outside the engine; a `performance` governor or EPP fixes it,
    and it's what the DSP load meter shows in real playback here.
- **Instruments and effects, one by one (11 October 2026).** Profiled every built-in with
  `effects/examples/profile_effect.rs` and `instruments/examples/profile_instrument.rs`
  (they time one device over noise or chords, `--repeat n` for a profiler, `--out`/`--compare`
  to prove a change left the output identical to the bit). Everything kept is bit-identical
  to what it replaced, checked that way and with the sound tests.
  - **Kept:** the reverb's 16-line network tick as array passes the compiler vectorizes
    (0.64% → 0.53% of a core); the wavetable's frame lookup hoisted out of the sample loop
    and the second frame skipped when the position sits on one (default patch 2.34% → 2.08%,
    2×8 unison 3.81% → 3.34%, user table 5.34% → 4.40%); the halfband oversampler's two
    allpass chains as separate loops and only the paths in use flushed (waveshaper 0.74% →
    0.69%, distortion likewise); the waveshaper's bias offsets worked out only when the bias
    or curve moves; a fast path in `Ring::get_between` for the usual unwrapped read (delay
    0.09% → 0.08%, hyper, chorus a few percent); the shaper's `rem_euclid` as `x - floor(x)`
    (0.235% → 0.214%); the vocoder's and EQ's detectors on plain comparisons instead of
    `f32::max`; the multifilter no longer copies its tuning struct every sample.
  - **Tried and dropped** (slower or no faster, so reverted): fusing the EQ's bands into one
    per-sample loop (band by band already overlaps enough); the drum layer rendered in stages
    over runs (the extra passes cost more than the per-sample matches saved); the reverb's
    software `ceil` and bounds checks (not where the time was).
  - **Where each stands** (% of one core at 48 kHz, release): EQ 0.04 flat / 0.15 six bells /
    1.2 at 24 dynamic bands; compressor 0.11; delay 0.08; reverb 0.53; waveshaper 0.69 (4×
    oversampling); chorus 0.35; filter 0.14; shaper 0.21; transient 0.1–0.28; limiter 0.31;
    multiband 0.34; frequency shifter 0.32; amp 0.55; distortion 0.73; hyper 0.83; vocoder
    0.87; convolve 1.0; clipper 1.9 (4×, true peak). Poly Synth 0.3 (four-note chords);
    drums 1.35 (busy groove); wavetable 2.1 (six-note default patch); guitar 2.0 per track.
  - **What's left is not scalar work.** The profiles show each device at its natural scalar
    cost: the EQ in its state-variable filters, the compressor and multiband in a dB-domain
    `exp2`/`log2` per sample, the waveshaper and distortion in their oversampling filters,
    the guitar in a feedback loop that can't be pipelined, the drums in a long per-sample
    chain of small pieces. The next tier would change the sound slightly or need SIMD by
    hand: fast `tanh`/`exp2` approximations (the drums' sine already is one), 2× rather than
    4× default oversampling in the waveshaper, exploiting the clipper's symmetric FIR taps,
    and interleaving the guitar's six strings in one loop.
- **Left for later**, in the order they'd pay:
  - the reverb network (`Network::tick`, 0.66% of a core per instance, the dearest effect);
  - the guitar's waveguide loop (2.2% of a core per track);
  - the piano roll copies its clip every frame (`Shown`), the clip panel its clip; only
    matters for clips with thousands of notes;
  - track headers (33 of them are about a third of an idle arrangement frame) and egui's
    per-widget bookkeeping in general;
  - `History` keeps up to 200 whole-project clones (the bench song is about 1 MB each).
    Done in the second round: notes and automation points are shared between them.

- **Second round: the audio thread, opening a project, undo (28 October 2026).** Measured
  against the build before it (`target/baseline/bench`, and a worktree of it with line tables
  for profiles), pinned to a P-core, best of several alternating runs; the laptop was shared
  with other builds, so only quiet stretches count. Results are in the table at the end.
  - **A profiler that tells the truth: `tools/pcprof.c`.** Preloaded into the program, a
    CPU-time timer notes the instruction each thread is at; `tools/pcprof_report.py` names
    them with addr2line, inlining included. It costs nothing measurable, unlike the gdb
    sampler, which turned out to distort: stopping the program from gdb makes glibc's maths
    routines (`sinf`, `exp2f`, `cos`, `log10f`, the FMA versions) look ten times dearer than
    they are, so the first round's and this round's starting profiles overrated the LFOs'
    `sin`, the fader's pan law, the compressor's `exp2` and the meters' `log10`. A scalar
    inline `exp2`/`log2` for the compressor, limiter and multiband, and a polynomial sine
    for the wavetable's LFOs, were both slower or no faster in real timing and were dropped.
    `tools/sample.py` got two fixes (a SIGPROF before the program's `exec` killed it; no shell
    in between) and `SAMPLE_PCS`.
  - **Guitar strings side by side** (`guitar/waveguide.rs`: `Jobs`, `run_together`, `Lanes`;
    `dsp::simd::F32x4`). Each pass over the strings (vertical planes, then horizontal) first
    gathers every voice's loop, then runs them four to a vector register: one loop is a chain
    of filters each waiting on the last, four side by side cost about two thirds of four in a
    row. Within a control interval the readings come first (an interpolator holding still is
    a four-tap filter along the line), then the loss filter and each allpass as a pass, then
    the writes; lines keep a mirror of their first samples so any reading is contiguous.
    Bit-identical (a unit test runs every kind of loop both ways; every guitar golden render is
    unchanged). Fewer than three loops run one at a time (two side by side were slower).
    Six-note chords 2.08 → 1.40% of a core (−33%); a single held note 0.76 → 0.82% (+8%: the
    gathering costs more than it saves there). On a shared machine the pass ordering helps the
    bench's two guitars by about a quarter.
  - **Compressor in passes** (`compressor.rs`). A block goes through the detector (the
    sidechain high-pass and the level), the level in dB, the attack/release recurrences, the
    gain, and the output as separate passes, so the logarithms and exponentials run four at a
    time (`dsp::simd::log2`/`exp2`, within 4e-7) instead of a library call each per sample.
    0.115 → 0.079% of a core per instance with its meters on. Not bit-identical: outputs
    differ by at most 2e-7 (−134 dB); every compressor golden hash changes (all 21), nothing
    else's.
  - **Meters and taps only while someone looks** (`engine/src/chain.rs` `Meters::watch`/
    `watched`; `Effect::set_metering`). The UI reads a device's meters (and drains its tap)
    only while its panel shows, and each read keeps them coming for about a second and a half
    (`WATCH_BLOCKS`), as does a new device; otherwise the engine skips `meters()` (and its
    dB conversions) and the tap, and tells the effect, so the compressor skips its RMS and
    peak meters and the EQ its output peaks. A tap that had stopped drops what it held when
    read again. The UI drains only the taps of panels drawn in the last few seconds
    (`Session::drain_taps`, `TAP_WATCH`), so the rest stop on the audio thread too.
  - **EQ bands holding still are left alone** (`eq.rs`, `Band::settled`): with nothing moving
    or listening, the 24 bands' per-chunk bookkeeping (smoothers, detectors, followers) is
    skipped. At its defaults the EQ went 0.046 → 0.011% of a core; bit-identical.
  - **Float positions to indices through `i32`** (`dsp::split_index`). `x as usize` and back
    compile to a dozen instructions with branches on x86 (no unsigned 64-bit conversion);
    the wavetable's table reads and frame positions, its comb filter, the reverb's sixteen
    line reads, the guitar's loops and pickups, the sampler and `Ring::get_between` now go
    through `i32`. Bit-identical; the wavetable −6%, the reverb −13%, hyper −12%, chorus −6%.
  - **Reverb lines no longer a power of two apart** (`LINE_STRIDE_PAD`): sixteen lines written
    at the same offset all fell in one set of the L1 cache. Bit-identical, a few percent.
  - **Engine plumbing**: the fader works out its pan law once a block while only the level
    moves (bit-identical); each automation lane keeps a cursor instead of a binary search per
    block (`Automation::value_following`, with a test against the search).
  - **Opening a project** (the first compile builds every processor). The wavetable bank is
    made lazily: a background thread makes the 19 tables, those instruments ask for
    (`Bank::want`, real-time safe) first; the UI thread makes what it draws (`Bank::table`);
    the audio thread only reads (`Bank::ready`, which could wait only in the program's first
    fraction of a second). Its FFT keeps a plan per size (the bit-reversal swaps and the
    twiddles, made by the same rotation as before, so bit-identical, tested against the old
    transform) and runs twice as fast. The guitar's dispersion table (2,304 searches with
    `atan2`) is worked out once per sample rate and shared. MIDI events are sized up front
    and sorted unstably (equal keys are equal events).
  - **Undo shares what an edit didn't touch** (`project::Shared`, copy-on-write `Arc`): clips'
    notes and automation lanes' points. Reading is unchanged; a mutable borrow copies only if
    another copy of the project shares it; saved and loaded as the value, so files are
    byte-identical. `crates/ui` needed twelve one-word changes (`.clone()` → `.to_vec()` where a
    `Vec` was wanted). 200 one-clip edits of the bench song: 84 → 15 MB kept by undo. The UI
    pass (below) also shares each track's list of clips, and passes shared notes and points
    straight through `set_notes`/`set_automation`: 6.7 MB.
  - **Tried and dropped**: the wavetable's unison voices four to a vector (the table reads
    are gathers; no faster); two guitar loops side by side; scalar inline `exp2`/`log2`;
    more audio workers (still slower on this laptop, as in the first round).
  - **Results** (bench song, i7-1365U; "before" is `target/baseline/bench`, built from the
    commit before; render is the best of four alternating 20-second runs):

    | | before | after |
    |---|---|---|
    | render, pinned to one P-core | 21.9% of a core | 16.9% (−23%) |
    | render, default one worker, unpinned | 11.9–14.5% | 9.8–11.0% (about −20%) |
    | first compile (builds every processor), pinned | 188 ms | 35 ms (−82%) |
    | first compile, unpinned | 60 ms | 17 ms (−71%) |
    | later compile | 0.26–0.29 ms | 0.27 ms (0.6 ms pinned while the bank still fills) |
    | save / load | 7.4 / 17.2 ms | 7.4 / 17.2 ms (untouched) |
    | `Project::clone` | 0.048 ms | 0.025 ms |
    | undo after 200 one-clip edits | 84 MB | 15 MB (6.7 MB with the UI pass's shared clip lists) |
    | an edit's undo bookkeeping (copy, compare) | 0.156 ms | 0.035 ms |

    By device (pinned, alone, `--release`): guitar chords 2.08 → 1.40%, a single guitar note
    0.76 → 0.82%, compressor 0.115 → 0.079%, EQ at its defaults 0.046 → 0.011%, reverb 0.59 →
    0.51%, wavetable (3+3 unison) 2.34 → 2.20%. Under `dist` (fat LTO) the same or better:
    guitar chords 1.82 → 1.19%, a single note 0.59 → 0.63%, compressor 0.112 → 0.070%, EQ
    0.040 → 0.011%, reverb 0.54 → 0.44%, wavetable 2.11 → 1.97%.
    Golden (`engine/examples/golden.rs`, 24,372 renders): only the compressor's 21 hashes
    changed (the vectorized `log2`/`exp2`, ≤ 2e-7 in the output), everything else identical.
  - **Left**: the guitar is still a quarter of the bench (sympathetic voices keep six to
    twelve loops running; AVX would do eight at a time, behind a runtime check); the
    wavetable's oscillators (the table reads); the reverb's sixteen-line network; RON load
    and save (the parser's whitespace and identifier handling is most of a load).

### 15. UI polish and UI performance — done (28 October 2026)

The user asked for "a UI polish and performance improvement pass, until both are very
noticeable improvements from today", performance meaning the whole app. One agent did the
audio side (section 12, second round); this is the UI side and what joins them. Measured
against the commit before (`target/baseline/ui-perf` is that commit's test binary with the
new `ui_tests/perf.rs`), pinned to a P-core, alternating runs.

- **Look** (`theme.rs`, `widgets/chrome.rs`): fonts are now Inter (Regular, Medium,
  SemiBold) for words and JetBrains Mono for changing numbers, bundled in `ui/assets/fonts`
  with their OFL licences; `theme::text::medium`/`semibold` give the heavier weights (track
  names, clip titles, ruler numbers, headings). `theme::surface` is a palette of layered
  greys (canvas, panel, raised, control at rest, hovered, held, border, two inks) and the
  accent, and `apply` builds every egui style colour from it: borders, shadows on windows
  and menus, white text on the accent for selected buttons (`theme::accent` is now the
  constant, not egui's selection stroke). The fonts are set on the first pass, which then
  discards itself (text in the new families can't be laid out until the next).
- **Transport**: painted Play, Stop and Record buttons (`chrome::transport_button`,
  `Glyph`; Stop stops, or goes back to the start when stopped, like Space twice), a
  position display on the canvas ground (bar.beat.sixteenth large, green while playing),
  menus and status bar on the darkest ground and the transport raised between.
- **Arrangement**: track headers draw volume and pan as small bars filled in the track's
  colour under the value (`level_field`, `fader_position`), names in Medium; clips with
  bolder titles and brighter note miniatures; the ruler's bar numbers in Medium.
- **Device view**: each instrument and effect sits on a card (`chrome::card`) with its title
  bar across the top, instead of separators between them.
- **Piano roll**: ivory and ebony keys with seams and a face on the black keys; notes with
  a lit top edge and their name inside when there's room.
- **Meters and status**: the master meter shows its colour bands faintly at rest; the DSP
  load is a small gauge beside its number; empty-state hints and key caps restyled.
- **Faster frames:**
  - rows scrolled out of view (most of a long song) build no header or lane widgets; their
    clips' rectangles are still noted for rubber bands (`lane.rs`, `timeline/mod.rs`);
  - a clip's note miniature is one mesh, not a shape per note (`clip.rs`);
  - piano-roll notes register their widgets only near the pointer, as clips do (`piano_roll.rs`);
  - taps are kept only for panels drawn in the last five seconds, and a tap batch notes "had
    sound" once, not with a clock read per frame (`session.rs`, `taps.rs`): playing, the
    UI was spending 0.64 ms a frame copying 33 EQs' audio into rings nobody drew;
  - the wavetable's 3D view keeps its frames behind an `Arc` instead of copying them out of
    egui's memory every frame; knob captions are laid out once when they fit.
- **Opening**: resampling a file to the session's rate is cut into stretches converted on
  up to four threads, bit-identical (`media::resample`, tested against one pass); with the
  lazy wavetable bank, opening the bench song until every file has loaded went 252 → 109 ms.
  `media/examples/load_time.rs` times decode, peaks and resampling of one file.
- **Undo**: each track's clip list is `Shared` too, and `set_notes`/`set_automation` take
  anything that becomes a `Shared` and only sort or clamp when they must, so passing a lane's
  points back keeps them shared.
- **`ui_tests/perf.rs`** now also times opening (until every file has loaded), egui's
  tessellation of each frame, each frame's CPU time from `/proc/thread-self/schedstat`
  (which doesn't grow while other programs have the core), resident memory, and 200 edits
  with the engine taking each snapshot; with `CANOPY_SCREENSHOTS` it saves a picture of
  each view. `tools/sample.py` got the same startup fix as section 12.
- **Results** (`ui_tests/perf.rs` on the bench song at 1920×1080, CPU ms per frame, before →
  after; tessellation separately):

  | | frame | tessellation |
  |---|---|---|
  | arrangement, idle | 1.79–1.99 → 1.23–1.44 | 0.77–0.85 → 0.46–0.70 |
  | arrangement, whole song in view | 2.27–2.62 → 1.40–1.43 | 1.04–1.55 → 0.82–0.85 |
  | arrangement, playing | 2.92–3.16 → 1.66–1.67 | 1.09–1.37 → 0.92–0.93 |
  | piano roll, playing | 1.77–1.94 → 0.63–0.74 | 0.73–0.80 → 0.44–0.49 |
  | device view, playing | 2.76–3.13 → 1.61–1.67 | 1.08–1.39 → 0.88–0.96 |
  | device view, stopped | 2.32–2.79 → 1.47–1.49 | 1.10–1.13 → 0.87–0.89 |
  | open until every file loaded | 252–271 ms → 109–112 ms | |
  | 200 edits: time each, memory kept | 6.5–7.5 ms, +81–83 MB → 4.3 ms, +4 MB | |

- **Left for later**: the device panels are now most of a frame (the Wavetable's 54 knobs
  and three tables); a cached, pre-tessellated mesh for static displays would help both the
  frame and tessellation. egui's own buttons still don't fade their hover. RON load and save
  are untouched (section 12).

### 13. Code structure pass — done (11 October 2026)

The instruments and effects were written one at a time and had grown copies of the same
algorithms. Each now lives in one place; `CLAUDE.md` ("Shared code") lists where. Every change
was checked bit-identical with `engine/examples/golden.rs`, which hashes every built-in's audio
and meters over 24,372 renders (every enum choice, every float swept and modulated): the diff
against the renders before the pass is empty.

- **`dsp` took the shared algorithms:** the TPT state-variable filter, its coefficients and a
  Linkwitz-Riley `Crossover` (`svf.rs`, which absorbed `effects/src/filter.rs`); the TPT
  one-pole and a `DcBlocker` (`one_pole.rs`); xorshift and its float conversions (`noise.rs`,
  ten copies before); the curved envelope `Segment` that `Adsr` and the wavetable's
  envelopes both run, and `curve_coefficients` for the mod matrix and LFOs; one `Smoothed`
  (the effects' copy, with `modulate`/`Pace`, merged into it, plus `settle()` for the forty
  places that jumped to the target), `MAX_BLOCK` (which `engine::MAX_BLOCK` now is), and
  `flush`, `settle`, `catmull_rom`, `rational_tanh`, `poly_blep`, `note_hz`.
- **Effects:** the `Effect` trait's `set_param`/`modulate` both call one `receive(index, value,
  pace)`, and every effect keeps its values in `Values<N>`, which also tracks freshness. New
  shared modules: `meter.rs` (`PeakMeter` replaced eight hand-rolled peak fallbacks),
  `dynamics.rs`, `lookahead.rs` (the limiter's and the clipper safety's), `shaping.rs` (the
  waveshaper's and distortion's curves and oversampler), `halfband/`. Descriptors end with
  `..EffectDescriptor::BASE`.
- **Instruments:** one `float`/`choice` builder for every descriptor table, `checked_value`
  (six copies; it also stops the plain oscillator taking NaN), `segments` to split a block at
  its events (five copies; the oscillator's could panic on events out of order), and one
  `voice_to_steal` policy for the Poly Synth, the wavetable and the sampler.
- **Tests:** `effects/tests` went from 16 files to 6 and `instruments/tests` from 16 to 10,
  merged by family (real-time safety, CPU, extremes, latency) over shared `tests/common/`
  helpers. Fewer binaries to link, and the timing tests in one binary take a lock so they never
  run at once.
- **Size:** 170,227 lines of Rust before, 168,410 after: `dsp` grew by 680 lines and the
  instruments, effects and their tests lost 2,530.
- **Kept apart on purpose,** because merging would change the output: the four `bessel_i0`s
  (different series and stopping rules), the guitar amp's filters and `soft_clip`, the chorus's
  sine, the guitar's per-sample DC blockers, the vocoder's and reverb's struct-of-arrays
  filters (kept for vectorization), the utility's f64 crossover, and `stage_shape` (clip
  fades divide where the segments multiply by a reciprocal).
- **Timing** (each device against the build before the pass, pinned to one P-core, best of
  three). Under `dist`, what ships: the waveshaper and distortion 21% faster, everything else
  within ±2.5% (EQ −2%, compressor, reverb, delay, wavetable, drums, guitar level). Under
  `--release`: within ±2% for nearly everything, the analyzer 25% faster, the drums 4% faster;
  the EQ and transient shaper 7–10% slower (EQ at its defaults 0.043 → 0.047% of a core, six
  bells 0.148 → 0.162%). Every call in the EQ's loop is inlined in both builds; the loop just
  compiles differently now that the filter lives in `dsp`, and the difference is gone under
  `dist`. The Poly Synth was 4% slower until `dsp::Svf` went back to plain fields (nesting a
  `Coefs` and an `SvfState` in it cost that); it is now 1.5% faster than before.
- **Found on the way, and fixed: fat LTO slowed the EQ.** With six bells the EQ took 0.21% of
  a core under `dist` against 0.16% under `--release`, before this pass as after it. Sampled
  instruction by instruction, both builds run the same inlined band loop, but under fat LTO the
  compiler stopped keeping the filters' memory in registers: every sample stored it and loaded
  it back, on the filters' feedback paths, which a recursive filter can't hide. Thin LTO, or
  none, didn't do it. The user chose to fix it in the code rather than the profile: the band
  loop (`cascade` in `eq.rs`) copies every filter's memory into locals for the block, with one
  loop per filter count so the count is known when compiled, and still runs sample by sample
  (filter by filter, a 48 dB cut was 14% slower: the processor can't overlap the next sample's
  first filter with this one's later ones). Bit-identical (all 512 EQ golden renders). % of a
  core, before → after:

  | EQ | `--release` | `dist` |
  |---|---|---|
  | defaults | 0.038 → 0.041 (noise: no band runs) | 0.045 → 0.044 |
  | six bells | 0.159 → 0.164 | 0.213 → 0.163 |
  | two 48 dB cuts, four bells | 0.251 → 0.251 | 0.287 → 0.252 |
  | six dynamic bells | 0.352 → 0.356 | 0.398 → 0.369 |

  The 3% left on six bells under `--release` is the compiler pairing the two channels'
  arithmetic differently; it varied between 2% and 6% over runs.

### 14. Built-in instruments as CLAP plugins — done (19 October 2026)

The user asked for canopy's instruments to be packaged as CLAP plugins, all in one file,
tested with the guitar.

- **What:** `crates/canopy-clap` is a `cdylib` (and an `rlib` for its tests) on the raw C ABI
  (`clap-sys`, like the `clap-gain` test plugin). `tools/package-clap.sh [--install]` builds
  it in release and writes `dist/clap/canopy.clap` (and `~/.clap/canopy.clap`); `--windows`
  cross-builds `dist/clap-windows-x86_64/canopy.clap` with cargo-xwin (static C runtime;
  imports only kernel32, ntdll and an api-ms-win synch set, checked by
  `tools/check-windows-dlls.sh`, now shared with `build-windows.sh`). The factory
  offers every built-in but the Sampler, which needs an `.sfz` (`canopy_clap::packaged`):
  ids are the built-ins' (`canopy.guitar`), names "Canopy Guitar" and so on.
- **How it runs** (`bridge.rs`): as the engine runs an instrument. Host blocks are cut into
  blocks of `dsp::MAX_BLOCK`; parameter events apply at the start of the engine block they
  fall in (dense automation doesn't shorten blocks); CC64 is resolved by the wrapper, as the
  engine does for live input; CC 120/123 and wildcard note offs are `AllNotesOff`; the
  transport drives `set_tempo`/`set_position`. Activation does what the compiler does with a
  new instrument (prepare, tempo, every value, `reset`).
- **Parameters** (`values.rs`): id = index (append-only, so hosts' automation stays put).
  Floats are offered normalized 0–1 (so log knobs turn right in generic editors) and are
  modulatable (`PARAM_MOD` → `Instrument::modulate`); enums are stepped and enumerated. Text
  both ways is the descriptor's `format`/`parse`. Values are kept in plain units in atomics,
  so defaults and states are exact; the main thread's changes reach the audio thread at its
  next block.
- **State** (`state.rs`): JSON `{instrument, version, params: {id: plain value}}`; missing
  ids load at their defaults, unknown ones are ignored, another instrument's state is refused.
- **Tests:** unit tests of the bridge against a recording mock instrument (chunking,
  parameter timing, modulation, pedal, MIDI, transport), values and state; `tests/host.rs`
  loads the built file through the `plugins` host: what it offers, the guitar's parameters
  and text, state through the host, every instrument sounding, and **the guitar playing a
  riff (chords, pedal, bends, tone/drive/model changes) bit-identical to the built-in run as
  the engine runs it** (checked to fail on a 0.1% parameter difference); `tests/realtime.rs`
  runs every packaged instrument through the bridge under `assert_no_alloc`.
- **Windows:** Wine 9.21 staging is now installed here (`~/.local/bin/wine`), so cross-built
  tests run: `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER=wine WINEDEBUG=-all cargo xwin test
  --target x86_64-pc-windows-msvc -p canopy-clap` passes all 22 (the guitar bit-identical to
  the built-in there too). The packaged release DLL, through the Windows `plugin_cli` under
  Wine, renders the guitar within 8.5e-6 (-85 dB) of the Linux render: the platforms' libm
  rounds differently, as it does for canopy.exe.
- **Not measured:** its CPU (other agents were running). It should equal the built-in's: the
  same code in the same blocks, plus event translation. Whether `--profile dist` is better or
  worse for it than `release` is untested (see section 13's LTO note).
- **Leftovers:** no editor (an egui one inside the plugin would be its own project); no
  Sampler (needs a file parameter or editor); the Wavetable has only its factory tables;
  no
  `voice-info`, `note-name` or presets for hosts; untested in a third-party host
  (clap-validator, Bitwig, Reaper) and on real Windows (only Wine).

## Done earlier in this session

- **Knobs:**
  - double-click a knob, the wavetable's mod-amount bars or its curve boxes to type a value;
  - Ctrl+click resets;
  - "Type Value…" and "Reset to Default" in the right-click menu;
  - `ParamDescriptor::parse` reads units, note names and percentages.
- **Log-scale parameters** can start at 0 (`ZERO_KNEE`).
- **Drums:**
  - Attack and Hold reach 0 ms;
  - every hit is identical by default;
  - a per-pad "Vary hits" switch brings back analog-style variation.
- **Transient shaper** (`canopy.transient`), and the FL importer maps Transient Processor and
  kHs Transient Shaper to it.
- **FL importer:**
  - OTT, TDR Kotelnikov and Nova, Valhalla Supermassive and the Kilohearts plugins are read from
    their saved states;
  - Fruity Delay 3, Balance, Stereo Enhancer and Shaper, Center, Chorus and Flangus too;
  - Vital's wavetables come in as user tables, and its chorus, flanger and phaser come across;
  - the report lists unimported clips by name.

## Strum pattern tool — done (11 October 2026)

The user asked how to make strumming patterns on the Guitar; every strum was placed by
hand. Piano roll → Tools → Strum Pattern… (Alt+P) strums held chords in a rhythm.

- **Tool:** `project/src/note_tools/patterns.rs` (`note_tools::strum_pattern`): the
  pattern syntax (`D/U C B X P - _`, upper case accented) with errors, chords grouped on
  the grid, the pattern kept in the bar's time across chord changes (or restarted per
  chord), swing, gate and ties, accent velocities, upstrokes skipping the bass, seeded
  humanize, and the Guitar's key-switches for dead notes and palm mutes (only on
  changes; restored after). Directions aren't written: they follow from position, the
  way the Guitar's Alternate strum direction reads them. 15 presets, with sources.
- **UI:** `ui/src/piano_roll/pattern.rs` (the window: preset menu, the text field with
  live errors, a painted step preview with arrows, settings, Listen), wired in
  `piano_roll/tools.rs`. Tool windows now recompute their preview only when a setting
  changes (the note-making tools used to give the clip new ids every frame).
- **Tests:** 17 unit tests in `patterns.rs`; `project/tests/strum_pattern_guitar.rs`
  plays every preset on the Guitar and checks each strum's direction and articulation
  from its meters; `ui_tests/piano_tools.rs` (hint, error, presets, Listen, Apply,
  undo). Demos 15 and 16 in `demos/guitar/` come from `engine/examples/strum_patterns.rs`.
- **Known limits:** the Guitar can't see the bar, so a phrase that starts off the beat
  (Ska upstrokes) begins with a down-stroke; it would need the song position, which
  effects get (`effects::Context::beat`) and instruments don't yet. On sixteenths,
  swing past 50% puts a swung up-stroke nearer the next down, and the Guitar reads it
  as one. Eighth triplets are read down-up-up.

## UI polish — done (10 October 2026, two passes)

The user asked for "a general ui-polish pass. clear layouts, more uniform sizes and margins,
subtle animations, usability stuff like the ability to loop in a piano roll". Two agents
started it and were stopped; a third finished it.

- **Design system:** `crates/ui/src/theme.rs` (documented at the top; keep its names stable,
  device panels use it). `space` (XS 2, S 4, M 8, L 12, XL 16), `radius`, `stroke` (HAIRLINE,
  EMPHASIS, INDICATOR), `text` (TINY 9, SMALL 10.5 = egui's `small()`, BODY 13, HEADING 16),
  `size` (CONTROL 18, BAR 24, RULER 24, …), `anim` (FAST 0.08 s, NORMAL 0.12 s, ZOOM 0.15 s,
  `ease_out`), shared `color`s, and `hover`, `fade`, `glide`, `glide_marker` for motion.
  `widgets/chrome.rs` has the shared painted pieces: fold arrows, icon buttons, toggles,
  switches, menu items with shortcuts, shortcut tooltips with key caps, dialog buttons,
  empty-state hints, drop and drag labels, and the keyboard focus ring. Device panels' and
  displays' font sizes and the meters' colors now come from the theme.
- **Animations** (all ask for frames only while moving): clip and note selection and hover
  fades; track header hover; effect folds slide open and shut; automation lanes and an
  unfolded group's tracks grow open; drop lines (effect chain, track list) and the clip-drag
  preview glide between snapped places; zoom to fit, Ctrl+wheel zoom and scrollbar clicks
  glide; the detail tabs' underline slides; the focus ring glides from control to control.
  Device panels used to poll every 100 ms forever; they now listen for two seconds after
  anything happens, then the window sleeps (`every_view_sleeps_when_left_alone` checks the
  window asks for no frame at all, in each view).
- **Piano-roll looping:** the piano roll's ruler is the arrangement's ruler on the same ticks,
  so its loop brace is the song's one loop (`project.loop_region`), and both views show the
  same stretch. Drag along its top to loop part of the clip, drag the loop or its ends, click
  it to turn it on or off; L or its Loop button turn it on (keeping a loop over the clip, or
  else looping the clip) and put the playhead in it; Shift+L loops the selected notes. The
  engine plays the project's loop, so playback goes round it while you edit. Tests in
  `ui_tests/polish.rs`, including playback round the loop while notes are added.
- **Usability:** zoom to fit (Z, Shift+Z, and Fit buttons in the transport and the piano
  roll); each clip remembers its piano-roll view; Ctrl/Shift/Alt+wheel mean the same over the
  arrangement and the piano roll (tested); shortcut tooltips with key caps; labels by the
  pointer saying where a drag lands (clip position and track, "Move to …"); empty states (no
  tracks, no clip, no effects, empty automation lane); a right-click menu on the piano roll's
  grid; menus with shortcuts at the right; a visible keyboard focus ring (Shift+Tab no longer
  leaves focus on a stray control); Help → Keyboard Shortcuts (F1) with a filter, its columns
  aligned.
- **Screenshot tour:** `ui_tests/tour.rs` shows every main view, menus, dialogs, drags in
  progress and empty states at 1280×800 and 1920×1080:
  `CANOPY_SCREENSHOTS=<dir> cargo test -p ui tour`.
- **Second pass (10 October 2026)**, finishing what the first left:
  - **Knob** (`widgets/knob.rs`, API unchanged, ring API included): name and value at
    `text::SMALL`, `TINY` (then an ellipsis) when they don't fit; `knob::LABEL_HEIGHT`,
    `VALUE_HEIGHT` and `knob::height` give its rows (`panel::KNOB_TEXT` is built from
    them). Hover fades (`theme::hover`); `theme::accent`; `stroke::HAIRLINE` outline,
    `stroke::INDICATOR` drop highlight. With the keyboard focus it draws a round ring
    (`stroke::EMPHASIS`, fading in) and tells the app not to draw its rectangle
    (`chrome::rings_own_focus`). Arrow keys turn it a wheel notch a press (Up/Right up,
    Down/Left down, Shift or Ctrl fine), Home/End go to either end. The value changes
    live; the response reads as changed only when the keys are let go, so a held,
    repeating key is one undo step through the callers' usual commit. While a knob has
    the focus, the app's shortcuts on those keys (Home, nudging clips, stepping the
    effect selection) leave them to it (`knob::takes_key`). Help lists the keys.
  - **Wavetable panel** (`devices/wavetable/*`): theme spacing, text sizes, radii,
    strokes and the accent throughout; source tiles and the matrix's bars at the
    control heights; section names, menu group names and matrix headings as
    `chrome::caption`s; the matrix's clear button is the app's painted icon button;
    Load…/Edit… are full-height buttons; tiles sit `space::XS` apart within a group and
    further between groups. Layout and behaviour unchanged.
  - **Collapse animations:** hiding an automation lane (its × or the A toggle) and
    folding a group now shrink the rows shut, the same ease and time as opening
    (`anim::NORMAL`, `ease_out`). The rows going away are drawn where they were
    (`timeline::Closing`, newest first under the row they were under), aren't found by
    drags, and ask for frames only while moving.
  - **Saved arrangement view:** `Project::arrangement_view` (`ArrangementView`: left
    tick, zoom, row height, vertical scroll) is kept by the timeline every frame
    (`Session::keep_arrangement_view`, no undo step, doesn't mark the project unsaved),
    carried over undo and redo in `ViewState`, ignored by `end_live`'s "did anything
    change" check, written only when set (old files re-save as before; still format 1;
    invalid views refused, `EditError::InvalidView`, and dropped on load), and restored
    on opening (`TimelineView::restore_view`, from `reset_views`).
  - **Literal gaps:** drums, guitar, amp and creative panels, then a sweep of the
    rest: `space`/`radius`/`stroke`/`text` values and the accent instead of literals.
    New shared pieces: `panel::knob_slot` (a menu or a few buttons in a row of knobs,
    named where a knob's name is and centred on where its circle is: drums' Choke,
    Filter, Bursts, Type; guitar's Direction, Legato, Amp; the shaper's filter type;
    the utility's Bass mono and delay side), `panel::DISPLAY_INSET` (a display's
    corner note), `panel::CONTROL_ROW` (the switch row display heights leave room for),
    `theme::size::LEVEL_BAR` and `scopes::level_bars_width` (every panel's level bars
    are now the same width). Drum pads and layer tabs fade their hover.
  - **Tour:** now also shows a focused knob, the Wavetable's Oscillators, Envelopes &
    LFOs, Matrix and Voicing pages, and the Amp, Waveshaper, Shaper and Vocoder panels.
  - **Tests:** `ui_tests/polish.rs`: `a_focused_knob_turns_with_the_arrow_keys`,
    `holding_an_arrow_key_turns_a_knob_as_one_undo_step`,
    `hidden_lanes_and_folded_groups_shrink_shut_then_rest`,
    `the_arrangement_opens_scrolled_and_zoomed_as_it_was_saved`, and
    `every_view_sleeps_when_left_alone` now hides lanes and folds a group too;
    `project::file`: `the_arrangement_view_is_saved_and_old_files_open_without_one`.
- **Left for later:**
  - egui's own buttons don't fade their hover (egui draws them);
  - the piano roll's per-clip views still live only in memory (not saved);
  - display internals (curve line weights, inner margins) keep their own numbers;
    the Wavetable's `widgets/dahdsr.rs`, `node_editor.rs` and `shape.rs` weren't
    themed;
  - a row hidden while it's still growing open starts shrinking from full height.
