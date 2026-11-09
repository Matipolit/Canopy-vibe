# Progress — 28 October 2026

Where every piece of ongoing work stands, so it can be picked up in a new session.

## Standalone Shaper CLAP with visual editor — 9 November 2026

`tools/package-clap.sh --effects --install [--profile dist]` builds a separate
`canopy-effects.clap` containing **Canopy Shaper** (`canopy.shaper`). The default
instrument-only `canopy.clap` keeps its original IDs/order and host controls.
The effects bundle has stereo input/output (including in-place processing),
no note ports, normalized continuous parameters, stepped enums and modulation,
host tempo/playhead sync, parameter-ID state and a conservative eight-second
Time-module tail. Processing uses the same <=64-sample engine blocks as Canopy.

The embedded native editor offers Volume/Filter/Pan/Time tabs, enabled/rate,
drawable curves with right-click presets and playhead, depth/phase/curve smoothing,
filter and time settings, mix and output smoothing. All points/controls send CLAP
value events and gesture begin/end, retry rejected host output events, request
inactive parameter flushes and mark project state dirty. Automation and loaded
state feed the same atomic values the editor reads. Audio/GUI communication uses
preallocated atomics; GUI code never accesses the audio processor. Editor windows
can resize, hide/show, close/reopen and coexist in multiple instances.

`canopy-widgets` now owns the shared curve editor, theme and licensed fonts;
DAW modules re-export it, and analysis image rendering uses the shared font path.
`egui-baseview` 0.7.0 supplies embedded windows. `vendor/baseview` is 0.3.7 with
one documented EGL fix: explicit X11 platform selection prevents Mesa crashes
when the desktop sets `EGL_PLATFORM=wayland`, without changing the host process's
environment. Linux X11/XWayland was tested at desktop scaling; Windows/Cocoa
handles are implemented but untested. Additional host zoom is not supported.

Verification: effect audio parity at 44.1/48/96 kHz, varied host blocks, tempos
and seek positions; state round trips; stereo in-place ABI; real-time allocation
checks including GUI event emission; pointer drawing in all modules, enable and
preset clicks, minimum editor size, instance independence and event backpressure.
The native host test opened two editors, closed/reopened them, and captured an
actual window for visual inspection. Control geometry, paint output and native
window captures were checked. Existing instrument CLAP suite passed; DAW UI suite passed
224 tests (3 environment-dependent tests ignored). The final distribution bundle
is installed in `~/.clap/canopy-effects.clap`; README documents loading and use.

## Sample-level ducking and optional smoothing — 8 November 2026

Ducker has an appended **Peak** trigger mode (existing mode indices unchanged).
It links the absolute stereo peaks without phase cancellation and uses them
directly for linear headroom ducking, bounded by Depth. Zero attack/hold/release
has no detector or gain lag; lookahead 0 and Listen below 20 kHz give the raw
sample response. Nonzero envelope times remain available. The factory preset
**Sample-level peak duck** sets the raw configuration. Legacy modes expose their
previously fixed detector attack/release and gain smoothing, each with a zero
option. Peak's separate Release reaches zero while the legacy release keeps its
5..2000 ms automation mapping. Ducker version 2.0 migrates mode automation and
macro/modulation ranges from three to four choices. Mode changes fade; continuous controls retain
their parameter ramps unless the channel disables them.

The smoothing audit's other controls are now explicit, with old defaults:
Shaper Output smoothing (1.5 ms); audio Clip view Automatic edge fades (on);
Sampler Minimum release/Voice fade (5 ms), Automatic loop crossfade (on, explicit
SFZ settings still win); Drum Synth Kit → Fades (5 ms retrigger, 8 ms choke);
Opto detector smoothing (10 ms) and release memory (on). The clipper already
allows its true-peak safety stage to be bypassed with Off or Sample peak.

Track/group context menus and the master device view expose **Smooth parameter
changes**. Turning it off skips `dsp::Smoothed` ramps in that channel's render,
including instrument/effect parameters, fader/pan/mute and Patcher cable gains.
The render policy is thread-local, scoped and restored around each node/master;
it does not alter another channel, worker or engine. Audio filters, explicit
envelopes, delay-tap slews and algorithm-specific transitions keep their controls.
Automation scheduling remains at engine-block boundaries. Cache fingerprints and
cache renders retain the ramp policy. New mixer/clip flags default off and are
omitted from old project files; new device parameters are appended by index.

Verification passed: 173 UI tests (3 ignored), 223 effect unit tests (including
11 Ducker cases), 313 DSP/instrument/project unit tests, 15 project compatibility
tests, 42 engine integration tests (effects, clip edges, graphs, parallel render,
cache, allocation safety), 41 drum sound tests (1 ignored), 25 sampler playback
tests and 6 instrument allocation tests. Effect latency (5), modulation sweeps
and cost (2), and allocation safety (4) also pass. After preserving the legacy
Ducker release range and adding version-2 mode migration, the Ducker units,
compatibility tests, effect integrations and Ducker panel test were rerun.
Installed on 9 November 2026 as Arch package
`canopy-git 0.1.0.r26.gb6ab4dc.local20261109180721-1`, built directly from the
working tree with the dist profile. All four installed binaries match the staged
package; pacman reports no missing files. The new Peak preset is present in the
installed application.

UI follow-up on 9 November: two Duckers in one rack shared the Smoothing
disclosure's egui ID, so clicks interfered and appeared ineffective. The first
single-device test missed it. All new disclosures now salt their IDs with the
device owner (`Panel::id`). A two-Ducker regression fails before the fix and
passes after it, using physical pointer clicks to expand/collapse independently
and editing the second device's detector release without changing the first.
Rendered screenshots were visually inspected with both sets of controls open.
UI suite: 174 passed, 3 ignored. Rebuilt, reinstalled, and verified the installed
application against the staged package; no missing pacman files.

UI bugs reported on 8 November 2026 are recorded in [bugs.md](bugs.md).
Both are fixed locally: lane widgets are clipped away from track headers,
and opening a MIDI clip immediately gives the piano roll shortcut focus.
UI suite: 171 passed, 3 ignored, including all 18 regression tests and three
new tests for these bugs (`cargo test -p ui ui_tests -- --test-threads=2`).

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
- **Build output.** Only the default `target/` (CLAUDE.md: extra target dirs filled the disk
  on 1 Nov 2026), never the session scratchpad: `/tmp` is a RAM-backed tmpfs and filled up once. Cleaned up on 10 October 2026:
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

7. **LLM control and versions (set 29 October 2026)**, in this order, each its own spec, plan
   and build:
   1. History panel: every undo step listed, click one to jump to it. **Done** (section 18).
   2. An MCP server so an LLM can do "essentially anything" in the running app: a `crates/mcp`
      crate (hand-written JSON-RPC, a tool table shared by a stdio bridge binary
      `canopy-mcp` and the app), the app listening on 127.0.0.1 with a token file, calls run
      on the UI thread, every edit one `Session::edit` undo step tagged as the LLM's, renders
      for analysis off the UI thread. Composable tools (read, edit, analyse), not one tool per
      task. **Done** (1 Nov; section 23; spec `docs/superpowers/specs/2026-10-29-mcp-design.md`,
      plan `docs/superpowers/plans/2026-10-29-mcp.md`, all 17 tasks).
   3. The analysis (spectra, loudness, sibilance, transients, clashes between tracks) as a GUI
      view too, on the same code.
   4. Versions: a sidecar store beside the project (`song.canopy.history/`, deduplicated)
      keeping undo across sessions, named commits, branches (switching = an undo-like swap),
      and a structural diff by ids.
   5. Merging branches, and undoing one step from the middle (both three-way merges by id).

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
| 11 | FL import fixes | **In progress** (4 Nov) | Measured against right-here_2's stems: timing exact, samples and levels fixed; FL's own effects (Gross Beat first) next, then third-party stand-ins |
| 12 | Performance pass | **Second round done** (28 Oct) | Engine, opening, undo, UI frames; leftovers listed |
| 15 | UI polish and UI performance, third pass | **Done** (28 Oct) | Theme, fonts, transport, cards; frames 35–65% cheaper |
| 16 | Compact, more visual instrument panels | **Done** (29 Oct) | Every built-in fits a 1280 window; section cards, compact knobs, new displays |
| 17 | Slicer (Slicex-style loop slicer) | **Done** (29 Oct) | `canopy.slicer`; leftovers below |
| 18 | History panel | **Done** (29 Oct) | First of priority 7; the MCP server is next |
| 23 | MCP server, with analysis | **Done** (1 Nov) | Second of priority 7; the GUI Analysis view is next |
| 24 | Linux packages (.deb, Arch PKGBUILD) | **Done** (1 Nov) | `tools/package-linux.sh`, `packaging/` |
| 14 | Built-in instruments as CLAP plugins | **Done** (19 Oct) | `crates/canopy-clap`, `tools/package-clap.sh`; leftovers below |
| 22 | Tracks cached as renders while idle | **Done** (1 Nov) | 3.5× less playback CPU on the test song; plugins and groups left live; leftovers below |

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
  Windows' own DLLs.
- **29 October 2026: CI had failed on every push since 19 Oct**, always in `plugins`'
  `tests/scanning.rs`, so `project`'s and `ui`'s tests had never run on Windows. Fixed: the
  scanner now reports a Windows crash (an NTSTATUS exit code such as 0xC0000005) as a crash
  rather than "exited with code …"; the cache test sets the new modification time by hand
  (`fs::copy` keeps the old one on Windows); the yabridge test is Unix-only (and Windows can't
  delete a loaded DLL). CI's tests now run with `--no-fail-fast`. The next run (40885272039) passed
  everything, `ui`'s 174 tests included (WARP renders them), but one: RON wrote `\r\n` line
  endings on Windows, so a project saved there differed byte for byte from one saved on Linux
  (and a test editing the text failed). `to_file_string` now always writes `\n`; checked on
  the Windows build of `project`'s tests under Wine.

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

**Graph engine — done (2 November 2026).** `engine/src/graph.rs` plays a `Routing` as a real
graph (plan §5.2), replacing the stub that ran a graph's devices in series:
- Compiled on the UI thread into `Step`s over numbered stereo buffers. Buffer 0 is the
  track's block; a port takes over its cable's buffer when nothing else will read it, else
  gets a fresh one the cables mix into, so a straight-line graph runs in place with no
  scratch and no copies, bit-identical to `Linear`.
- Devices (port 1 is the sidechain: a cable there replaces the `sidechain` send), band
  splits (`dsp::crossover`, now wired into `dsp`: Linkwitz-Riley 12/24/48 dB, bands sum to
  an allpass), L/R and M/S splits (dual mono) and joins, Input, Output; ports sum cables.
- Cable levels glide over 5 ms (`set_cable_gain`, mutes, new cables fade in; a chain that
  just stopped being a line keeps its old links at level). Latency: every cable into a node,
  sidechain included, waits for the latest arrival, in a per-cable `Compensation` line
  sized on the UI thread (`Compensation::sized`; a line that outgrows itself is replaced and
  `take_over` copies its sound). The chain's latency is the arrival at Output.
- Nodes off every Input→Output path get no steps. Crossovers, lines and ramps are
  `StateSlot`s adopted from the same chain's program in the previous snapshot; `GraphState`
  remembers what each chain holds.
- Tests: `engine/tests/graph.rs` (12, under `assert_no_alloc`: line = `Linear`, parallel
  sums, mutes and levels, glides, fade-ins, band/LR/MS round trips, latency alignment and a
  growing line, sidechain cables, skipped nodes, state across a swap), unit tests in
  `graph.rs` and `compensation.rs`, `dsp` crossover tests (they now measure RMS), and splits,
  joins and a sidechain cable changed during playback in `realtime_safety.rs`.
- Not done: the Patcher canvas (`ui/src/patcher`, `READY = false`), split parameters as
  automation targets, and sizing the track-level `Compensation` beyond 8192 samples.

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

### 11. FL import fixes — in progress (5 November 2026)

**The user's goal (4 Nov):** work with the projects that have stems until the importer is
correct. Not bit-perfect, but every audio sample and MIDI pattern at the right tempo and time,
and FL's own plugins matched closely. Third-party plugins: Vital is installed (hosted as
itself); the others get matched with built-ins only once the basics are right.

**How it's measured.** `right_here_2_export_stems/` is FL's "split *playlist* tracks" export
(not mixer tracks): `Track N` is playlist row N, the whole mix with the other rows muted. The
ignored UI test `ui_tests::flp_stems` imports the song with the real Vital and renders it the
same way (a version per row, with only that row's clips and the tracks left playing, six at
a time: ~70 s for all 21); `tools/flp_compare.py` compares (`--fl-delay 5202`: FL's export
runs 5202 samples late on every stem, probably a master plugin's latency; `--renumber 24:+1`:
the stems were exported after a playlist row was added above row 24 and not saved). The
song's saved arrangement has no vocals: the vocal channels are never placed.

```sh
CANOPY_FLP_STEMS="$PWD/fl projects/right-here/right-here_2.flp" CANOPY_FLP_STEMS_OUT=$PWD/target/flp-work/run \
  CANOPY_REAL_PLUGINS=/usr/lib/clap/Vital.clap cargo test --release -p ui flp_stems -- --ignored --nocapture
python3 tools/flp_compare.py "fl projects/right_here_2_export_stems" target/flp-work/run \
  --plots target/flp-work/run/plots --fl-delay 5202 --renumber 24:+1
```

**Fixed (4 Nov):**

- Audio clip trims are milliseconds of the sample as recorded, not quarter notes (the
  "wrong speeds" bug). The sampler's time stretching is read (`CHANNEL_PARAMETERS` bytes
  96..112: length in 1/192 quarters, an integer in FL 20 and a float by FL 26; pitch; a log2
  multiplier; mode) and becomes the clip's `Stretch` (Resample, or Stretch for Auto/élastique,
  Beats for the slice modes; a set length syncs to the tempo as an original BPM). Reverse
  and swap (`CHANNEL_FX_FLAGS`) and Normalize (byte 80; the probe now reports each file's
  peak) are carried over; samplers played from patterns get the Resample pitch and the
  normalize gain in their SFZ (pitch-keeping stretches can't be, and are reported).
- FL 21+ playlist records hold each audio clip's fades (song milliseconds), their tensions,
  a linear gain, and a word whose low bits switch the fades on (bytes 36..60).
- Channel volume follows the insert fader's curve (0 dB at 10000): a square law agreed only
  between 6000 and 10000.
- Reeverb 2's separate dry and wet levels: the Reverb has Dry and Wet knobs now (2.0; older
  files' mix migrates in `project::migrate`, automation, macros and presets included), and
  the Delay an Output, so Delay 3's levels come across as mix plus output.
- `flp::Import` says which track each insert became (`inserts`) and which playlist row each
  clip came from (`playlist_tracks`).

Measured after this (median level difference while both play): rows 4, 7, 10, 29, 31, 32,
34, 35 within ±0.4 dB with flat spectra; 1, 8, 13 within 1 dB, 9, 11, 17 within 2; timing exact (0 ms, correlation
0.85 to 0.98) wherever the instruments are the real ones. The "drum clips as sforzando"
report came from samples that weren't found: found ones are audio clips.

- Gross Beat: the Shaper has a Time module now (played from a history of the input,
  `tim_*` parameters after the mix, `tim_range` in beats), and `native::GrossBeat` reads
  the chosen envelopes (header words at bytes 4 and 8; 72 slots of points: delta beats and
  value as doubles, tension, mode 2 = hold) onto the Shaper's time and volume shapes. Its
  time envelope's bottom is 8 beats back: fitted on rows 9 and 12, where 4 or 12 line the
  transients up worse. Row 12 now lines up within 3 ms (correlation 0.73).

**Left, in order:**

1. Gross Beat's other modes (pulse, wave, stairs) are read as plain curves, and its
   envelopes are sampled onto 32 steps a bar; its own mix and smoothing knobs aren't read.
2. Reeverb 2: done, measured against FL (see below). Not carried over: early reflections
   (15 dB under the tail at ER 128, 24 at the default 64; timed by size, not pre-delay),
   diffusion (±5% on the decay), modulation (rare), separation below 0 (329 of 1493
   instances: FL scales the sides by `1 - sep/64`, up to twice as wide, which canopy's
   Width can't reach; it would take a wider Width range and a migration), dry above 128
   (canopy's Dry stops at 100%), and FL's filters' floor near Nyquist (the 16 kHz octave
   comes out ~10 dB low).
3. FL's other effects here: Maximus, Limiter, Chorus, Flangus, Vocodex, Love Philter,
   Multiband Delay, Patcher (skipped now), Stereo Shaper, and FL Slayer (the Guitar plays
   it). Across all of the user's projects the most used are EQ 2 (81), Chorus (22), Maximus
   (20), Reeverb 2 (17), Limiter (16), Vocodex (13), Delay 3 (12).
4. Then the third-party stand-ins (OTT, Melda, kHs, TDR, Valhalla, the Splice instrument),
   behind most of what's left on rows 3, 14, 16, 17, 23.

**Probes against FL itself (4–5 Nov).** The Windows PC (`ssh win`; FL 26.1 at
`C:\Program Files\Image-Line\FL Studio 2026`) renders `.flp` files from the command line
(`target/win/ps/render.ps1`: `FL64.exe /R /Ewav /O<dir>` run as a scheduled task in the
desktop session, 16-bit 44.1 kHz whatever the export settings say; a watchdog kills FL
and deletes the output past 60 MB or 120 s, and the task is deleted afterwards).
`cargo run -p flp --example probe` lists effect states and writes projects that play a
signal (as one audio clip) through one FL effect: `write` one, `batch` one project per
line of states. FL only loads written files once the writer gives the master an output
(event 147 = 0), each effect a "new plugin" record (event 212), each playlist item its own
number (bytes 32..36, from 1), audio clips their FL 26 fields, and the build (event 159);
those are in `flp::write`. A project with more than one playlist item still plays only the
first, so each probe is its own project (6 s a render). `cargo run --release -p effects
--example apply_effect` runs one canopy effect over a recording, to fit canopy's knobs to
FL's measurements without an import. The scripts and data are in `target/flp-work/probe/`
(`fl.sh`, `cn.sh`, `rt.py` per-octave RT60 and levels, `map.py` and `fit.py` the mapping
prototyped against every probe). A scan of the PC's 570 projects
(`target/flp-work/states.jsonl`) ranks FL's effects by use: EQ 2, Limiter, Reeverb 2,
Maximus, WaveShaper, Chorus, Transient Processor, Vocodex, Delay 3, Compressor.

What Reeverb 2 does, from 90 renders of a noise burst (and an impulse) (all in `native::Reeverb2` and
`convert::reeverb2`):
- Decay: the RT60 is the knob times 1.83 (size 1) to 1.41 (size 100), 1.46 at size 50,
  never much under 0.35 s. Canopy took the knob as the RT60, hence the short tails.
- High damping (hundreds of Hz, 221 off) is a tank's: each second the tail goes through
  a one-pole low-pass at the corner `1/(0.117 + 0.0047·size)` times, so the extra loss is
  the same dB a second at any decay (long tails darken far more) and stronger in small
  rooms; and the tail is heard through about one more such low-pass. The Reverb has a
  Damping curve choice now: Gentle does this (two first-order shelves per line on the
  SVF the Shelf curve already ran, so no extra cost per sample, and a one-pole on the
  wet at 1.5× the corner). Shelf, the default, is bit for bit what it was.
- Bass (20–300%) multiplies the lows' decay by 0.375–1.42, not by the knob; its corner
  reaches 1.2 (longer) to 2.4 (shorter) times the crossover. The Reverb has Bass decay
  and Crossover now (a first-order low shelf per line, skipped at 1×).
- Wet and dry run on curves of their own (wet 64 is −9 dB, 160 +3.5; dry 64 −12.7, 160
  +5.6); automation spans the knobs' 0..160.
- The tail is louder than canopy's hall at the same wet and RT60 by 6 dB at size 1 to
  0.2 at 100, more under a second; the wet makes up for it.
- Separation was read the wrong way round: +64 is mono, not wide.
Against every probe, canopy's RT60 is now within ±10% per octave (most within ±5%) and the
level within ±1 dB, except tails under 0.5 s at sizes 1 and 100 (±25%, −4 dB). Through the
importer, right-here_2's wet-only Reeverb 2 rings 5.75 s against FL's 5.58 (it was 4.51),
its tail within 2 dB at every point (it was 3 to 9 dB low). EQ 2 (5485 instances, 64 renders of white noise, transfer functions against
canopy's through `apply_effect`): frequency, gain and output were right, and bells
at 100 Hz and 1 kHz matched to 0.1 dB; widths weren't. `convert::eq2_q` now gives each
type its fitted Q: bells are the cookbook's (RBJ) with the bandwidth kept near Nyquist
(widened by `w0/sin w0` at 48 kHz for canopy's bilinear bells: a 14 kHz bell went from
2.1 dB off to 0.5); shelves `1.18 − 0.8·knob` (were a bell's Q, too gentle); passes a
curve of their own (the default width peaks +4.5 dB at the corner, which the importer had
as a flat Butterworth) times `(1/√2)^order` for positive orders and 0.94/0.81/0.70 for
negative ones (FL's steeper shape; within 1 dB); band passes and notches their own
curves. Against every probe: RMS error under 0.3 dB for bells, shelves, positive-order and
plain passes, under 1.1 dB for negative orders; passes with the width knob above 0.9
(374 of 5019) are gentler than canopy's lowest Q (0.1) and stay 2.5 dB off.

Fruity Limiter (1688 instances; 95 renders of a sine stepping up 3 dB at a time and of a
24 dB jump) came in at canopy's Limiter defaults (−1 dB true peak) whatever its settings.
Its state is a version, 18 knobs, the same 18 again (the A/B bank) and display flags;
`native::Limiter` reads the limiter's gain, ceiling (FL keeps 0.2 dB under it, sample
peaks) and release, and the compressor's threshold, ratio (hard knee), attack and release,
each from a measured curve. Half the instances use only the limiter; 307 only the
compressor and 461 both: where the limiter would pass everything under 0 dB untouched and
the compressor is on, `convert::instance_effect` makes it canopy's Compressor (when the
user left the suggestion). The limiter's static curve now matches to 0.6 dB and its
release to 1 ms; the compressor's threshold and ratio to 0.6 dB, its release within 5 ms
of FL's 35 at the default. Not carried over: a negative limiter gain (40; canopy's Input
only lifts), ceilings outside −12..0 dB, the saturation, the noise gate (67), knob 11
(bends the ratio either way), the curves and sustains, and FL's compressor hearing 2 ms
ahead (its limiter's look-ahead delays both). Next by use: Maximus, WaveShaper, Chorus. **Before rendering on the PC:** ask the user unless told to go ahead (they use
the PC), and leave no FL or `canopy-*` scheduled tasks behind.

**a-little-bit-remix (5 Nov 2026)**, copied to `fl projects/` with its folder (no FL render
of it exists; the stems in its `Samples/` are the original song's, played by the remix).
Importing it panicked: the Supermassive mapping still set the Reverb's old `mix`; it now
sets `dry`/`wet` (test in `convert/thirdparty.rs`). `flp_stems` gained
`CANOPY_FLP_STEMS_TRACKS=1` (each canopy track's own output, `tracks/`), which found the
rest. Open: (1) the guitar's EQ 2 (insert 14, slot 4) really holds six bells at 13–20 kHz,
+14 to +18 dB, three of them at 20 kHz two octaves wide; canopy makes ~+75 dB at 16 kHz
(peaks +11 dBFS, bursts above 14 kHz). The probes measured bells only up to 14 kHz, and
`eq2_q`'s `w0/sin w0` widening grows without bound towards Nyquist (×10 at 20 kHz), so
what FL does there is unknown: probe bells at 16–20 kHz, wide and narrow. (2) sforzando's
"piano" names no `.sfz` (a built-in instrument), silent. (3) The drum bus peaks +5.6 dBFS
through Maximus (Multiband, settings not carried over) and Supercharger.

**Done: Slicex on the Slicer (5 Nov 2026).** `flp::slicex` reads the state: a header, the
source path, then the loop as a whole WAV (32-bit float as FL saves it), its slices as the
`cue ` chunk's points, its tempo in `acid`; after it, 68 bytes a slice of per-slice
settings (not read) and the empty second deck. The importer writes the loop to the
generated folder from the first slice it keeps (the Slicer's slices start at frame 0),
sets the slices, Mode Off (FL plays slices unstretched), the root and the original tempo.
Which key slice 0 sits on isn't in the state: slices start on 60 when they all fit below
128 (amen, sesame street: notes on 60–70) and lower otherwise (VOX ALL #2, 564 slices,
notes on 15–40, so from key 0); between, assumed to end on 127. **Check in FL.** The
Slicer now holds 128 slices (a key each, the whole keyboard; was 64): slices 65–128's
Reverse/Pitch/Level/Pan are appended after every first-64 block, so saved indices stay
put (`slicer::slice_param`), and the playing meter has 6 slots of 24 bits. Of more than
128 Slicex slices, the first 128 are kept when they reach every key the channel plays (the
keys FL has them on), otherwise 128 from the lowest played (reported when notes fall
outside). Not carried over: per-slice settings, Slicex's envelopes and filter.

**To investigate (Slicex's key mapping, needs the PC):** a key picks a slice, so FL too can
play at most ~128 of a Slicex's slices (the vocal's other 436 can't sound there either),
*unless* regions are assigned by hand. Slicex's Keyboard panel and Wave Editor let any
region go to any key ("Assign current region to this key", a marker's "Trigger note"),
several regions share a key as layers, and "Assign to all" maps them to the whole keyboard,
white notes, black notes or none (Image-Line manual: Slicex Keyboard, Slicex Wave Editor).
The importer assumes the default, slice `k` on key `first + k`; where a custom assignment
is stored isn't known (the 68-byte per-slice records were identical in every project seen,
so likely there). Probe: one Slicex project per mapping (default, white notes, black notes,
a region moved to another key, two regions layered on one key, more than 128 slices), diff
the states, and confirm which key slice 0 sits on for 69–127 slices. Canopy's Slicer would
need a key per slice (and layers) to carry a custom mapping over.

**Wanted (the user, 5 Nov 2026): a proper Sampler.** FL Sampler channels come in through
one-region SFZ files the importer writes, which carry only what SFZ opcodes can (root,
tune, volume, `offset`). The user wants a real sampler instrument with fine control over
the sample's start, pre-delay and the time after it (FL's sampler settings: start, trim,
the envelope, time stretching), so FL's channels map onto its parameters rather than onto
generated files. Not started; scope to be agreed.

**Done: Expanse, a delay-reverb after Valhalla Supermassive (5 Nov 2026)**,
`effects/src/expanse.rs`, panel in `ui/src/devices/space.rs`, five factory presets. Eight
lines in a Hadamard network, lengths Time × a fan that Warp opens (0: clean echoes, input
and output along the matrix's eigenvectors for 1 so each pass comes back unchanged);
Jot losses from Feedback; Density as input diffusers and an allpass in every line; one-pole
cuts in the loop; a ceiling past 2× full scale; 13 modes as continuous shapes (fan, simple-
fraction patterns, scale, a swell of the input over a share of the time, in-loop smear,
decay stretch, an outer echo loop), so a mode switch glides. Density's diffuser lengths
move at most 0.05 samples a sample (a 20 Hz LFO on it clicked at 0.5). Supermassive maps
onto it (`convert/thirdparty.rs`); mix, warp, feedback, density, mod depth are shares, but
**width, cuts, mod rate, Mode's twelfths and the delay's sync/note laws are guesses to
measure on the PC** (render bursts through Supermassive per knob, as with Reeverb 2), as
is how its modes really sound against these shapes. CPU: ~2.7% of real time measured on
a loaded laptop; `tests/performance.rs` has two cases for it, allowed a reverb's 5%.

**Done: more of FL's effects carried over, without the PC (5 Nov 2026).** a-little-bit-remix
now converts 57 effects (11 at defaults, was 21) and drops 1 automation clip (was 7):
- *Ranges, with migrations* (`project::migrate::RESCALES`, a table of grown ranges by
  effect version; automation, modulation and macros keep their values): the Reverb's
  Width to 200% (3.0; Reeverb 2's negative separation, 329 instances), the EQ's Q down
  to 0.02 (2.0). Fitted against the EQ 2 probes already rendered: FL's pass curve levels
  off at Q 0.071 from width 0.95 (`eq2_q`'s table), within 0.05 dB (was up to 6.8).
- *Extended built-ins*: the Delay's **Input** (how much goes into the lines; Delay 3's
  "Input wet", static and automated, for throws), the Shaper's **Time offset**
  (`tim_offset`, Gross Beat's scratching clock, read even with the time module off;
  its read moves at most 0.9 samples a sample, as the Delay's tap, or a fast LFO on
  it clicked in `tests/modulation.rs`).
- *Automation*: `Conversion::Linear` and `LimiterGain`; Delay 3 input wet, Gross Beat
  scratch (param 8), Maximus pre/post gains, a mixer track's stereo separation (own
  param 0xC2: centre leaves it, left widens, right merges; becomes an automated
  Utility width, 200% to 0, at the end of the chain), Vital knobs whose clips are named
  after another channel (by what follows the last dash).
- *A plugin as several effects*: `Settings::companions` (before or after the main
  device). Cymatics Diablo Lite = Transient Shaper (Punch) into the Clipper.
- *MeldaProduction* (`vst::melda`): zlib, then text XML (older) or binary XML (`\0MBX`,
  `vst::melda_binary`, newer); only non-default `#param`s, in the plugin's units, laws
  from Melda's manuals. MSaturator (1118 in the corpus) and MWaveFolder (317) on the
  Waveshaper, MStereoExpander (165) on the Utility's width, MAutopan (107) as the
  Shaper's pan alone (its default volume pump was on for every imported autopan).
- *Maximus* (1266, `native::Maximus`): version word, 7 numbers, 13 a band (low, mid,
  high, master), the A/B copy, then the bands' curves as FL envelopes. Field 2 of a
  band is post gain (the master's is automation param 48: params follow the state's
  order); gains taken on the Fruity Limiter's measured curve. The Multiband gets the
  gains and ratios of 1 (Depth stays: it scales gains too); curves, saturation,
  times, crossovers not read.
**To measure on the PC:** Maximus's gain law, curves and crossovers; Melda curve laws
(MSaturator's threshold-at-silence drive is a guess, 6 dB); MFreqShifter's Shift (saved
`-1..=1`, manual ±10 kHz: law unknown, so not mapped); Leveling Tool (AdHd, 11 raw
floats) and Supercharger (NI container) need knob-by-knob probes; Diablo's clip drive
(taken as 24 dB at 100%); FL's mixer separation at its left end (taken as 200%).

**Done: the Waveshaper's drawn curve, and FL's WaveShaper and MWaveShaper on it (5 Nov
2026).** Shape "Drawn" (`waveshaper::DRAWN`, appended) plays 65 points over input -1..1
(`curve_p0..64`, `0..=1` for -1..1, a straight line by default, appended after the knobs),
straight lines between, held beyond ±1; each point glides as a knob (`Smoothed` per point,
ticked a chunk at a time while any moves; the bias offsets follow a version counter).
Panel: `ShapeEditor` with `.ends(true)` (points edge to edge) and `.presets(..)`
(transfer shapes) in place of the transfer display. **Fruity WaveShaper** (1091, all read):
version, format word (4 adds 2 bytes at the end), pre/mix/post (`0..=256`, 128 default,
taken as shares of 128: **measure**), bipolar (byte 16; unipolar curves always start at
0,0), oversampling 0–4 (byte 18), point count at 30, 24-byte FL envelope points from 34.
The segment type is byte 20 (as Gross Beat's), always single curve here; byte 23 is only
the tension's sign (1, 255, 2 for none), which looked like a type at first. Curves are
sampled with Pre worked in (Drive can't go below 0 dB), tension as automation's
(`project::segment_value`: **sign convention to confirm in FL**). **MWaveShaper** (90, 88
with graphs): `vst::melda_elements` lists every element (text or binary XML); `<graph>`'s
`<Point x y c>` (0 where left out; `c` taken as the tension of the segment after it),
`#Asymmetric` for bipolar, `#gain`, `#outputgain`, `#Depth` (50% default). Its graph
modes other than Squared aren't modelled.

**Done: canopy's own CLAP instruments, and switched-off playlist tracks (5 Nov 2026).**
A channel FL played through `canopy.clap` (CLAP id = the built-in's id, so
`vst::is_canopy`) comes in as the built-in: `plugins::canopy_instrument` matches "Canopy
<name>" for the suggestion, the plan offers no hosting for it (its identity is left out),
and `vst::canopy_state` reads canopy-clap's JSON (`instrument`, `params` by id in plain
units) straight into the track's params. Not yet: automation of such a channel's params
(FL's index is presumably the CLAP id, which is the descriptor index; unconfirmed, no file
has any). Playlist tracks: `TRACK_DATA` byte 12 is "on" (after the number, colour and
icon), read as `PlaylistTrack::muted`; clips on a track that's off come in muted, and a
canopy track whose clips all came from such tracks is muted itself with its clips unmuted
(but those muted in FL in their own right). a-little-bit-remix: 13 playlist tracks off,
12 canopy tracks muted.

**Done: four fixes from listening to a-little-bit-remix (5 Nov 2026).**
- **Sampler start.** `CHANNEL_PARAMETERS` byte 144 (168-byte record, FL 21+) is an `f64`:
  where the sample starts, as a share of its length (`Channel::sample_start`); the generated
  SFZ gets `offset=` in frames. slick-dnb-snare: 0.2027 of a 417 ms sample, skipping its
  86 ms of silence (it played that late). Only 2 channels across the projects use it. Byte 136
  (`f64`, 1.0 everywhere but one older 158-byte record at 0.33) is unknown. Audio clips of such
  channels don't use it yet.
- **Fruity Phaser** (`native::Phaser`): ten numbers, a version then sweep, min depth, max
  depth, freq range, stereo, stages, feedback, dry-wet (`0..=1024`, 512 default) and out gain
  (4000 in all four instances). Dry-wet becomes the mix (both crossfade linearly), stages 7 and
  11 become 8 and 12. The amen group lost 2.8 dB to the phaser in canopy, mostly the dry/wet
  blend's inherent cancellation (~2.5–3 dB at 50–70% wet); whether FL's loses as much depends
  on its dry-wet law and out gain: **measure**.
- **MSaturator's threshold** now comes back off the output (drive + threshold, output -
  threshold): it moves where the curve bends, not quiet signals' level. Insert 31 (sub bass)
  was 3.2 dB louder for it.
- **Hosted plugins' automation.** FL numbers a plugin's parameters in the plugin's own order
  (channel targets: `param`; slots: `param & 0x7FFF`); hissy bass's "Vital - Macro 1" is 211,
  Vital's CLAP parameter 211 by index (id 49618). `Options::hosted_params` takes each automated
  plugin's ids in that order (`flp::automated_plugins` says which), filled by the app's
  `Dialog::open_with_plugins` through `PluginManager::param_keys` (`describe::param_keys`:
  hidden and read-only ones `None`), loading each plugin once. FL's 0–1 value is the lane's
  normalized value as it is (hosting's descriptors are linear over the plugin's range, or step
  indices). Checked on the real file with Vital.CLAP: hissy bass gets Macro 1's 7 points.
- Still louder than FL, probably: Insert 31's Maximus (+1.9 dB net with the guessed laws) and
  Kotelnikov (+2.4 dB, mapped in its own units, likely right). `installed_plugins_come_in_as_
  themselves` fails on this project at "it plays": its first Vital track's volume is
  automated to silence at the start.

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
  - **On battery it is worse (29 October 2026).** The user heard underruns playing
    `hiphop-beat.canopy` (3 Wavetables, 4 drum kits, 2 guitars, ~40 effects) at 44.1 kHz,
    256 frames, through PipeWire to Bluetooth headphones. `engine/examples/play_project.rs`
    (plays a saved project as a sound card would and reports every callback's time) showed
    0.39 ms a callback back to back but 2.0 ms median, 8.5 ms worst and 61 of 29,768
    callbacks past the 5.8 ms deadline when paced, on battery with power-profiles-daemon's
    `balanced` profile (EPP `balance_power`). Spinning between callbacks, or holding the
    `performance` profile for the run (`powerprofilesctl launch -p performance -- …`), gave
    0.43 ms median, 2.0 ms worst, none late: the underruns were the CPU clocking down
    between callbacks, not the DSP.
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

- **Third round: a data-oriented pass over the render (6 November 2026).** Before more
  caching (section 22), the live render itself. Every step, measurement and revert is in
  `PERF_LEDGER.md` at the repository root; `perf` works on this machine now (CLAUDE.md,
  "Measuring performance"). Measured on the user's projects (`~/Documents/canopy/
  hiphop-beat.canopy`, `test-140.canopy`) and the bench song, single-threaded and as played.
  - **What the counters said:** IPC 3.5–4.2 and 57–68% retiring everywhere: nothing waits on
    memory, the waste is instructions. Per-sample calls and `match`es, scalar gathers,
    horizontal sums, per-sample meters no one read, and partitions too small.
  - **Kept** (all bit-identical unless noted): the clipper's meters only while watched
    (`set_metering`); its linear-phase oversampling a block at a time
    (`halfband::fir::{convolve, History, UpBlock, DownBlock}`, sixteen partial sums per output
    in `dot`'s order, an AVX kernel chosen at run time); its true-peak safety's readings a
    block at a time; the reverb's network a block at a time (`Network::run`: the four taps
    of each line are runs in a row while its read position stays within a sample) and its
    pre-delay (`Ring::push_and_read`); the distortion's and waveshaper's oversampler a block
    at a time with both channels' IIR halfbands as vector lanes (`halfband::iir::Halfbands`,
    `Oversampler::run`) and their curves settled per block (`run_plain` per mode); the
    convolver's FFT vectorized per stage (now `dsp::fft::Fft`, also the wavetable's morphs:
    rounding of the first stages changes), its direct FIR in sixteen sums, its partitions
    (head to 1024, tails 512 to 4096 then 2048: 180 complex multiply-adds a sample for a 3 s
    hall instead of 266; within 1.8e-7) and its multiply-add in AVX; subnormals flushed by
    the processor on the render threads (`dsp::denormals::FlushToZero`: 31 million assists
    in hiphop-beat before).
  - **Results** (against the build before, interleaved): hiphop-beat 22.2 → 15.0% of a core
    single-threaded (−32%), test-140 9.1 → 5.4% (−41%), the bench song 15.5 → 12.6% (−19%).
    hiphop-beat played as the app does (one worker, paced, 256 frames): 13.3 → 10.2%, the
    99th-percentile callback 1.19 → 0.82 ms; under the power-saver profile 11 → 2 callbacks
    past the deadline and 5,150 → 303 past half of it.
  - **Golden**: only `canopy.convolve` (26 renders) and the wavetable's morphs (24) differ.
  - **Left**, in order of payoff: the wavetable's unison oscillators (14% of the bench song);
    the chorus's and hyper's modulated taps read a block at a time; vectorized `tanh`/`exp`
    for the distortion's and waveshaper's curves. The ledger's last section has the numbers.

- **Meters, one design (6 November 2026).** Levels at an effect's edges (input and output
  peak and RMS per channel, the output's true peak) are measured by the engine
  (`chain::run_slot`, `effects::meter::EdgeMeters`), only while the effect's panel is read
  and only what its descriptor asks (`EffectDescriptor::levels`); panels read
  `panel.levels`. An effect's own `meters()` hold what only it knows. `Context::metering`
  replaced the opt-in `Effect::set_metering`. Nine effects' level meters moved to the engine,
  three unread ones were deleted; after `reset` every meter reads as a new instance's. Sound
  unchanged (identical renders); `PERF_LEDGER.md`, change 17. Plugins could get the engine's
  levels too (declare them for `register_external` descriptors) when their panel shows meters.

- **Workers that cost what they save (7 November 2026).** Measured with every thread's CPU
  (`perf stat`), a worker cost ~0.55 ms of CPU every callback whatever the project: its
  300 µs spin past each callback and its 250 µs early wake, 9% of a core at 256 frames and
  half a core at 64. Five designs were prototyped and measured (`PERF_LEDGER.md`, "Worker
  prototypes"). Now a worker parks as soon as a callback ends (the engine says so,
  `Pool::callback_finished`), keeping the early wake; and the workers take part only while
  callbacks need them: in once one takes 40% of the period, out after two seconds under 15%
  (`pool::TakingPart`). hiphop-beat at 256 frames: 26.2% → ~17% of a core (its callbacks
  take 16.5% of the period instead of 10%, far from the limit); at 64 frames the worker's cost fell from 49% to 28% of a core. Pinning
  to the P-cores was tried: a little better with one worker, worse with two; not kept.

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

### 16. Compact, more visual instrument panels — done (29 October 2026)

The user asked for instruments "more compact and visually interesting, especially the
wavetable synth, it's very wide and not that intuitive". Design and plan:
`docs/superpowers/specs/2026-10-29-compact-instruments-design.md`,
`docs/superpowers/plans/2026-10-29-compact-instruments.md`. Only `crates/ui` changed: no
sound, parameter, preset or file-format change.

- **Shared pieces:**
  - `chrome::section` + `section_header`: a card with a strip of its color, colored by
    function from `theme::section` (oscillators A/B/C sky, teal and violet, filters coral,
    and the mod-source groups' green, blue, orange, pink and yellow, which
    `wavetable::knobs::group_color` now reads from there).
  - Compact knobs (`Knob::compact`, `Panel::compact`): 24 px, 46 wide and 40.5 tall (the
    28 px knob was 58 × 59); the value shows in the name's place while lit and in the
    tooltip. A knob with an empty label is named by its parameter for screen readers.
  - `chrome::chip`: a selectable tile with a tiny sketch (envelopes, LFOs, drum layers).
  - `Panel::bar`: the free part of an instrument's title bar (the Wavetable's tabs).
  - Knobs draw with the parent's painter rather than one clipped to themselves: a clip per
    knob cut the frame into a mesh per knob.
- **Wavetable:** tabs in the title bar; the sources as chips with group captions
  (`ENV`, `LFO`, `NOTE`, `CTRL`) on their own row. Oscillators: three 192 px cards
  (switch, table, `…` menu with Load/Edit/options; warp and morph menus; octave and stack
  menus; the 3D table taking the height left; 16 compact knobs four to a row), Sub and
  Noise stacked, the filter card (tabs 1/2, type, a Routing menu that stays open until a
  click outside, the curve, six knobs). The macros column is 100 px (each macro's tile in
  its knob's name row). Envelopes & LFOs: two cards with chips sketching each shape,
  editors taking the height. Matrix: only slots with a source or destination, then the
  first empty one as "+ Route". Voicing: Voices, Expression, Glide, Oscillators cards.
  Browser: 700 px, categories wrap. Width 2364 → about 940 px.
- **Drum Synth** (1447 → ~940 px), **Guitar**, **Poly Synth** (its own panel now,
  `devices/poly_synth.rs`: wave, filter curve from `dsp::svf`, envelope), **Sampler**
  (a key map of the regions over keys and velocities, Amp/Pitch/Envelope cards): see
  README "Instruments". `Panel::menu_slot`, `display_at` and `keys::key_rect` were added
  for them.
- **Tests:** `ui_tests/instrument_widths.rs` (every Wavetable page and each instrument
  fits 1280×800); `the_wavetable_tabs_sit_in_the_title_bar`,
  `the_matrix_lists_only_routes_in_use`, `a_new_route_fills_the_first_free_slot`; knob,
  chip and section unit tests. Tests that clicked moved controls follow them (Load…/Edit…
  in the `…` menu, Stack as a menu, routing in its menu); the sideways-scroll test went.
- **Height:** at 1280×800 a page body has about 230 px; three oscillators of 18 controls
  don't fit that, so the page fits the width and scrolls up and down, the displays at
  their 40 px minimum. At 1920×1080 everything shows. A taller default detail panel would
  remove the scroll at 800.
- **Frames** (`ui_tests/perf.rs`, bench song, pinned, against 27aa13f): device view
  1.49–1.56 → 1.59–1.63 ms CPU, tessellation 0.82–0.86 → 0.92–0.99 ms, meshes 520 → 488:
  the same work for more on screen (all three oscillators and larger displays).
- **Left** (from the final review): the filter display's frequency labels crowd at its
  narrow width; the title-bar tabs don't widen the instrument's card (640 px of room,
  enough today); chip sketches and compact knobs' tooltip text are rebuilt every frame;
  `poly_synth.rs` copies `dsp::svf`'s private damping limits for its curve; the macros
  have no place for their export handle (`crate::modulation::source_handle` is still a
  stub; the modulation workstream will want one); right-dragging the Poly Synth's filter
  display moves it like a left drag.
- **Also tested:** `a_short_device_view_scrolls_to_every_knob` (at the detail panel's
  minimum every instrument's last knob can be reached; a panel whose content doesn't
  scroll stops shrinking instead), `a_long_sfz_name_leaves_the_polyphony_menu_its_room`.

### 17. Slicer — done (29 October 2026)

FL Studio Slicex's core for chopping breaks, per section 2 of
`docs/superpowers/specs/2026-10-29-note-bends-and-slicer-design.md` (plan Tasks 6–9 of
`docs/superpowers/plans/2026-10-29-note-bends-and-slicer.md`). Built on its own branch off
the bends foundation (eec042c), beside the agent doing the other instruments' bends.

- **Data** (`project`): `TrackKind::Midi.slices` (starts in the file's own frames, saved
  only when set, kept across instrument changes, repaired on load); `Project::set_slices`
  sorts, dedups, puts the first at 0 and caps at `slicer::MAX_SLICES` (64).
  `project::slicer` holds the arithmetic: `guess_bars` (whole 4/4 bars of 1–16 between 70
  and 180 BPM, nearest 120), `grid`, `speed` (the track's matched speed, worked out with
  `Stretch::effective_speed` as audio clips' are) and `slice_notes`.
- **Instrument** (`instruments/src/builtin/slicer.rs`, `DataKind::Slices`): parameters are
  11 globals (Mode Resample/Stretch/Off, Original Tempo 40–300, Follow, Root Key as a
  choice of 128 key names, default C3 = 60, Cut on, Volume, Pitch, Attack/Decay/Sustain/
  Release as a straight `EnvelopeParams`), then four blocks of 64 (Reverse, Pitch, Level,
  Pan; ids `sliceN_reverse` etc., numbered from 1). `SliceData` (left/right `Arc<[f32]>`
  at the session rate, starts in its frames, the Resample speed) comes by `set_data`.
  24 voices (16 ringing with Cut off, the rest for fades), Catmull-Rom reads, a 2 ms fade
  at a slice's end, a 5 ms fade for Cut and steals; rate, slice level and pan update every
  16 frames (bends glide over a bend step). Meters: voices, slice count, peak, playing
  slices as bits, each voice's position. Not packaged for CLAP (`canopy-clap` skips
  `DataKind::Slices`).
- **UI data** (`ui/src/slicers.rs`, held by `Banks` as `DataKey::Slices(track)`): files
  decoded by path at the session rate on a thread each; per track, the matched audio
  (Resample/Off: the file; Stretch: `media::process_clip` on its own `Renderer` thread,
  the newest render or the file playing meanwhile), slices mapped into it, and a
  `SliceData` rebuilt only when the audio, slices or speed change (so the engine keeps its
  instrument otherwise). `StretchRenders` stamps each request; a render arriving for an
  older stamp is dropped (`a_stale_stretch_render_never_replaces_a_newer_one`). Updated
  every frame and before every rebuild, so a tempo change with Follow on re-matches.
- **Loading** (`Session::load_slicer`): decodes first, then one undo step ("Load Sample")
  sets the file, guesses Original Tempo and slices at transients (sensitivity 0.5; a hit
  within 10 ms of the start belongs to slice 1). A file that fails to load is reported in
  the status bar and changes nothing; a track whose file is missing or undecodable is
  silent and its waveform says "Can't play this file" with the reason.
- **Panel** (`ui/src/devices/slicer.rs`): the waveform card (as wide as the view leaves
  between the cards and the master column, 320–900 px), Tempo, Slicing, the picked slice
  and Amp cards. A click picks a slice and plays it while held; double-click adds a marker;
  dragging one is a live edit ending as one undo step; double-click (or a click, then
  Delete: `effect_rack::shortcuts` asks `slicer_delete_marker` first) removes one. Drops on
  the waveform load into that track (`devices::slicer_drop`, after the Wavetable's);
  Alt-drop on the timeline (`Action::LoadSlicer`) makes a new MIDI track, which the app
  selects and shows once loaded (`Session::take_loaded_slicer`). Slices to Piano Roll
  writes a whole-bar clip at the playhead's bar.
- **Decisions:** the waveform draws the playing audio's peaks (the Stretch render's,
  computed when it arrives; otherwise the file's), stretched to the card's width, so
  markers (in file frames) line up whatever the speed. Root Key is named as the piano roll
  names keys (60 is C3; the spec said "C5 = 60" in FL's naming). Auto-slicing happens at
  load only, as part of its undo step: slicing whenever a track had none could fight undo.
  Slicer data changes (slices, mode, tempo) build a new instrument instance, cutting the
  slices sounding, as a reloaded SFZ bank does. The sensitivity is a panel setting, not a
  parameter.
- **Tests:** project (`slices_save_only_when_set`, `set_slices_sorts_dedups_and_caps_at_64`,
  `slices_stay_when_the_instrument_changes`, `the_tempo_guess_lands_between_70_and_180`,
  grid and slice notes); `instruments/tests/slicer.rs` (keys to slices, the end fade,
  Resample at double speed an octave up, the pitch knobs, Cut, Reverse, a bend, level and
  pan, no data); realtime (`a_loaded_slicer_never_allocates_while_playing`);
  `slicers` unit tests (the stale render; Resample, Stretch keeps the pitch at half the
  length, Off); `ui_tests/slicer.rs` (grid and play, add/move/remove markers, Slices to
  Piano Roll, Alt-drop, an unreadable file); `instrument_widths` (fits 1280×800, reaches
  Root when short). Golden: every existing line unchanged, the slicer's 594 added.
- **Left:** FL Studio Slicex import (the importer could map Slicex channels to this);
  dragging a slice out to the timeline as audio; per-slice envelopes or start offsets;
  Beats mode; a Delete for a picked marker when the device view wasn't the last view
  clicked (the app's shortcuts go to the timeline then); the sensitivity isn't saved.

### 18. History panel — done (29 October 2026)

- **What:** View → History (Ctrl+H), a panel left of the master meter listing every undo step
  with how long ago it was made; click one to jump there (`ui/src/history.rs`,
  `Session::jump_to`). Steps jumped past stay as redo, so a jump is undoable.
- **Model:** `History` keeps each step's time, lists `steps()` (done, then undone, in the order
  made), `position()`, `jump(project, position)`, `forgot_steps()`; the undo stack is a
  `VecDeque`. `MAX_STEPS` 200 → 1000 (`project::MAX_UNDO_STEPS`): the bench now measures at
  the limit, 1000 one-clip edits of the bench song hold 35.0 MB, 0.020 ms per edit.
- **Tests:** `history` unit tests (steps order, jumps both ways and past the ends, an edit after
  a jump, ids not reused, the limit), `ui_tests/history.rs` (Ctrl+H, click back, to Start,
  forward, Ctrl+Z moves the highlight), `history::ago`.
- **Origins (1 Nov, with the MCP):** each step has a `project::Origin` (`User` or
  `Llm { client }`; `History::edit_from`/`record_from`). `Session::set_origin` says whose
  steps are recorded until it is set again; the MCP pump sets the calling client around each
  call and each waiter's poll, so steps the session records by any route are the LLM's and
  read "LLM: …" (`session::LLM_PREFIX`). The panel tags an LLM step with its client's name and
  puts a run of two or more by one client under a header "client · n steps", indented;
  `get_history` reports `origin` ("user"/"llm") and `client`. Tests:
  `history::steps_remember_their_origin`, `history::tests::runs_of_one_clients_steps_gather_under_a_header`,
  `ui_tests/history.rs::llm_steps_are_tagged_in_the_history`.

### 19. Locating missing audio files — done (29 October 2026)

- **What:** an audio clip whose file wouldn't load gets Locate File… at the top of its
  right-click menu (`timeline/interaction.rs`, `Action::LocateSource`, an `rfd` picker
  starting in the old folder if it's still there). `Session::locate_source` decodes the
  picked file first (an import load key, kept in `locates`); once it has, one "Locate File"
  step (`Project::relocate_source`) points the source at it with the new file's rate, length
  and channels, and also points every other source that failed with `NotFound` at a file of
  the same name in the same folder (those keep their stored rate and length, like
  `relink_sources` on open). A file of another length or rate is taken with a warning.
- **Session:** `source_paths` remembers the file each loaded source came from;
  `request_sources` (now run by every `rebuild`) loads again any source whose project path
  differs, so undoing a locate makes the clips missing again rather than leaving the located
  audio playing, and a late load from a path the source has left is dropped.
- **Media:** `MediaError::NotFound` for a file that isn't there (other I/O errors stay `Io`),
  so the clip can say "<name> not found · right-click → Locate File…" and the status bar
  adds the hint.
- **Tests:** `project` `relocating_a_source_moves_every_clip_that_plays_it`;
  `ui_tests/missing_files.rs` (the menu item only on missing clips, the folder sweep and its
  undo/redo, another length warns, a file that won't decode leaves the clip missing).
- **Left:** no list of every missing file in one window; Windows paths opened on Linux
  aren't found by the sweep (their file names don't split off).

### 20. Duplicating, copying and pasting tracks — done (29 October 2026)

- **What:** a track header's right-click menu has Duplicate, Copy, Cut and Paste; with no
  clips selected, Ctrl+D/C/X/V do the same to the selected tracks (with clips selected they
  stay on clips, as Delete does). A track goes whole: instrument and settings (plugin
  state refreshed first), effects and graph, modulators and routes, automation, clips and
  notes; a group with its tracks.
- **Model** (`project/src/tracks.rs`): `copy_tracks` → `CopiedTrack` (the track and the
  `AudioSource`s it plays), `paste_tracks(copied, position, from_here)` renumbers every id
  (tracks first, so references between pasted tracks follow; devices, nodes, modulators
  as they're met, references included; clips, notes, routes fresh) and adds or finds the
  sources; `duplicate_tracks` pastes each just below its original as "<name> 2" (counting
  up past names in use). A sidechain or follower on another track is kept only when
  `from_here` (same project, by `Session::project_generation`) or that track was pasted
  too. Controller links stay with the original (one controller drives one parameter).
- **UI:** the timeline's `Clipboard` enum (clips or tracks) replaced the app's clip
  clipboard, so Ctrl+V pastes whatever was copied last. Actions `DuplicateTracks`,
  `CopyTracks { cut }`, `PasteTracks(after)`.
- **Tests:** `tracks` unit tests (own ids, routes and automation on the copy's devices,
  names counting up, groups once, another project gets the sources and loses outside
  sidechains, errors); `fuzz.rs` duplicates and pastes tracks (and pastes into a fresh
  project, checked on its own); `ui_tests/track_copies.rs` (menu duplicate sounds the same,
  Ctrl+C/V by selection, cut, paste and undo, Ctrl+D on two tracks). The Wavetable starts
  voices at random phases, so the sound check uses the Poly Synth: two Wavetable tracks
  set up alike by hand differ in level as much as a copy does.

### 21. Native PipeWire — done (29 October 2026)

- **What:** on Linux canopy is its own PipeWire node ("canopy", "canopy-learn": the
  program's name), `Stream/Output/Audio`, role Music, F32LE stereo at the session rate,
  with `node.latency` the chosen buffer and `node.rate` asking the graph for our rate
  (PipeWire converts when another program holds it). No PipeWire: ALSA through cpal as
  before; `CANOPY_AUDIO=alsa` forces that. The old `pipewire` cargo feature (cpal's
  backend, needing libpipewire-0.3-dev and clang) is gone.
- **Code** (`engine/src/pipewire/`): `ffi.rs` loads `libpipewire-0.3.so.0` with
  `libloading` and declares the C types and functions used, by hand (layout test for
  64-bit sizes); `pod.rs` writes the EnumFormat pod (SPA's builder is header-only C;
  byte test, and `pw-dump` decodes it as F32LE 48000 2 FL,FR); `mod.rs` holds a
  `Connection` per stream: thread loop, context, core, a registry listener keeping the
  graph's nodes (outputs are `Audio/Sink`), and the stream, whose process callback
  (`RT_PROCESS`, PipeWire's data thread) has the engine render into the graph's buffer
  under `assert_no_alloc` and records frames, latency (`pw_stream_get_time_n`: delay +
  buffered + this buffer) and graph rate in atomics. `device.rs`: `start(&AudioSettings
  { sample_rate, buffer_frames, output })`, PipeWire first on Linux; `AudioOutput::
  reading()` (also filled from cpal's callback timestamps), `outputs()`, `problem()`.
- **UI:** the status bar's rate picker became the device menu (rate, buffer 64–1024,
  output on PipeWire); all three saved. The status reads "PipeWire · 19 ms" with the
  rest in its tooltip (the user wants UI text kept short). `Session::set_audio_settings`;
  test sessions only note settings (`opens_devices`), `test_outputs` fakes a list.
- **Measured** (`cargo run --release -p engine --example audio_latency`, empty engine,
  48 kHz, a Bluetooth output whose graph ran 256-frame quanta; three alternating runs):
  at 256 frames the predicted output latency went 23.0 → 17.9 ms (one buffer: the
  ALSA plugin's ring buffer is gone) and the process's CPU 3.4 → 1.7% of a core; at 128
  frames 17.6 → 15.3 ms and 5.7 → 2.9%. The two latency figures come from different
  reports (cpal's ALSA delay, PipeWire's own timing), so the difference matches theory
  rather than being measured end to end. The CPU saving was larger than expected:
  the ALSA path's thread hop and copy cost about as much as the rest of the idle process.
- **Tests:** `ffi`/`pod` unit tests; with `CANOPY_TEST_PIPEWIRE=1`, `engine/tests/
  pipewire.rs` (plays, rate asked for, outputs listed and one pinned, buffer size,
  20 restarts; the allocator aborts if the callback allocates) and the registry test in
  `pipewire/mod.rs` (the node appears under the program's name); `ui_tests/
  audio_device.rs` (menu picks output and buffer; no output section without PipeWire).
  Checked by hand: the app shows up as "canopy" in `pw-top`/`pw-dump`.
- **Left:** audio inputs (canopy doesn't record audio); following the graph's rate
  instead of converting; reconnecting by itself when the daemon restarts (the status
  shows "Audio stopped"; changing any device setting reconnects); the name of the
  output the default resolves to (the status says "default output").

### 22. Tracks cached as renders while idle — done (1 November 2026)

- **Why:** the user hears underruns on battery (section 12, "On battery it is worse"):
  the CPU clocks down between callbacks, so a project costing 7% of a core back to back
  takes 35% paced like a sound card. Doing less work per callback is the fix that
  doesn't drain the battery.
- **Engine** (`engine/src/cache.rs`, `arrangement.rs`, `audio.rs`, `bounce.rs`): a node may
  carry a `TrackCache` (`AudioLookup::track_cache`): its sound after its effects and
  before its fader, from sample 0. While the transport rolls, `render_node` reads it
  (times the transport gate) instead of running the instrument, modulation and chain;
  fader, compensation, send and meters run as before. `Processors::cache_mix` crossfades
  over 10 ms when a cache comes or goes mid-song (at once on a transport start or stop);
  the chain is reset (`cache_stale`) before it plays again. The track plays its own
  chain when notes reach it from a keyboard or an audition, when a live knob command
  reaches it (`cache_held`, until the transport stops), and when the UI reads its meters
  (`Meters::is_watched`; not in offline renders). `cache::render_key` hashes what a render
  depends on: the track trimmed of name, color, mixer and volume/pan automation
  (`track_fingerprint`, RON), the tracks its sidechains listen to (transitively), the
  tempo, the rate, and the audio and instrument data the lookup gives (by address, held
  in `RenderInputs`, which is also the render's lookup). `cacheable` refuses groups,
  plugins, modulators, routed chains, empty tracks, sidechains on groups and loops back.
  `cache::render` plays `render_project` (the track and its sidechain sources, muted, at
  the top, empty master) through `Bounce::capturing`, which records the node's pre-fader
  block into room reserved up front.
- **UI** (`ui/src/autocache.rs`): `AutoCache` keys every track (fingerprints memoized by
  comparing the track with a copy) every 250 ms or after an edit; a track whose key held
  for 4 s is rendered on a `SCHED_IDLE` thread (`THREAD_PRIORITY_IDLE` on Windows), one at
  a time. Its thread CPU time over its length is what the track costs live: under 0.25%
  of a core it isn't kept. Renders start playing at the next stop. `cache_for` (called by
  the compiler through `renders::Lookup`) checks the key itself, so a snapshot compiled
  right after an edit never plays a stale render; a stale one rides inactive for 2 s so
  the track fades from it, then is kept (for an undo) until the 1 GB budget needs the
  room. `next_frame` asks the app for a frame when a track will have been idle long
  enough. View → Cache Idle Tracks; `CANOPY_AUTOCACHE=0`; off in test sessions unless a
  test turns it on. The header says "cached"/"caching".
- **Measured** (`engine/examples/play_project.rs -- song.canopy [--cache | --background]`,
  `hiphop-beat.canopy`, 44.1 kHz, 60 s, on AC): 10 of 13 tracks cacheable (3 groups),
  rendered in 16.7 s of CPU (0.08–2.5% of a core per track), 525 MB. Paced at 256 frames:
  live 33.8% of a core, median 1.93 ms, worst 5.35 ms, 492 callbacks over half the
  deadline; cached 8.7%, 0.49 ms, 1.55 ms, none. Under the `power-saver` profile at 256:
  live 34.9%, worst 7.10 ms, 2 late; cached 10.1%, worst 2.58 ms, none; at 128 frames live
  38.3%, 1 late and 1015 over half the deadline; cached 12.5%, worst 1.04 ms, none.
  Rendering continuously at idle priority while playing live didn't hurt (14.7%, worst
  2.74 ms, none late): it keeps the cores clocked up.
- **Tests:** `engine/tests/track_cache.rs` (a song from its caches is the song to the bit
  until a render's tail ends below -80 dB; one cached track among live ones; an inactive
  cache isn't played; the key follows the sound and not the mixer; what isn't cached; a
  dropped cache fades back; caches play only while rolling; a keyboard note, a watched
  meter and a knob each bring the track back); `realtime_safety.rs` (playing from caches
  and leaving them never allocates); `ui_tests/autocache.rs` (an idle track is cached, an
  edit takes it back, undo finds it, the fader doesn't; switching off drops every render).
- **Left:** plugins (their state can change without the project knowing; the obvious
  next step, since bridged plugins are the dearest tracks); groups (a group whose
  children are all cached could be cached whole: the groups' reverbs are most of what
  stays live in the test song); renders on disk, to open a project already cached and
  hold less in memory; exports reading the caches; release tails after Stop on cached
  tracks are cut by the transport fade, as audio clips are.

### 23. MCP server, with analysis — done (1 November 2026)

- **What:** an LLM client reads and edits the running canopy and measures its sound, through
  `canopy-mcp` (a stdio bridge any MCP client starts) and a loopback listener in the app.
  Built from the plan's 17 tasks, by agents one or two at a time; README "Talking to canopy
  from an LLM" is the user's guide.
- **Crates:** `crates/mcp` (protocol, units, the `Host` trait, the tool table, the bridge,
  the token file) and `crates/analysis` (measures, clashes, PNG images). In the app,
  `ui/src/mcp/` (listener, pump, `AppHost`, renders). `engine::Bounce::with_stems` keeps
  each track's output from one offline pass (`engine/tests/bounce_stems.rs`: top-level
  stems sum to the mix to the bit).
- **Tools (33):** reads (`get_project`, `get_track`, `get_clip` paged, `get_device`,
  `list_devices`, `get_automation`, `list_presets`, `get_history`); edits (`add_track`,
  `edit_track`, `arrange_tracks`, `edit_song`, `add_clip`, `edit_clips`, `clip_ops`,
  `write_notes`, `edit_notes`, `transform_notes`, `set_note_bends`, `set_instrument`,
  `set_params`, `add_effect`, `edit_effects`, `load_preset`, `slicer`, `set_automation`),
  `batch` (≤ 200 edits, one undo step, `"$N"` refers to operation N's id), `history`,
  `transport`, and analysis (`analyze`, `find_clashes`, `plot`, `render_audio`).
- **Decisions made while building** (the agents' reports): pitch names C3 = 60, so MIDI 0
  is "C-2" (the plan's tests said "C-1"); note times are beats from the clip's start, as
  `get_clip` reports them; parameter lists of huge instruments (the Wavetable's 1468) are
  cut to the changed ones plus groups, with a `filter`; argument errors come from
  `serde_path_to_error` ("argument `notes[3].pitch`: …"); the client's name reaches the app
  in the bridge's first line (the bridge answers `initialize` itself); render staleness uses
  the session's revision (the history generation doesn't change on undo); render tickets
  are tracked by the pump so cancelling a call cancels its render; clash band levels use a
  Blackman-Harris window (Hann leaked a tone into the next Bark band); the spectrogram's
  colour map gained a violet stop; renders refuse over 10 minutes or 2 GiB of audio.
- **Tests:** `cargo test -p mcp` (unit, every tool against `FakeHost`, schema/struct drift,
  `tests/bridge.rs` with the bridge as a process), `cargo test -p analysis` (synthetic
  signals with known answers; `CANOPY_SCREENSHOTS` keeps the PNGs), `ui_tests/mcp.rs` and
  `ui_tests/history.rs` (a real socket: token, waiting out a drag and a dialog, the switch,
  edits as undo steps, presets, the Slicer, imports, analysis, cancelling, staleness, the
  render cache).
- **Left:** a `plot` marker for a clash's worst regions (spans by hand do it); `source:
  capture` with the Analyzer upgrade (priority 7.3); patcher and modulation tools once their
  engines exist. The Windows build ships `canopy-mcp.exe` (`tools/build-windows.sh`), not
  yet run since.

### 24. Linux packages — done (1 November 2026)

- `tools/package-linux.sh` builds (`dist` by default) and writes
  `dist/canopy_<version>+git<date>.<commit>_<arch>.deb`: canopy, canopy-learn, canopy-mcp,
  `/usr/lib/clap/canopy.clap`, desktop entries, the icon (`packaging/linux/canopy.svg`) and
  the `.canopy` MIME type; `Depends` from `dpkg-shlibdeps` plus the libraries opened at run
  time. `--install-to D` lays the same files out under `D`, which the Arch
  `packaging/arch/PKGBUILD` (`canopy-git`, from GitHub or `CANOPY_REPO`) runs in `package()`.
- `canopy file.canopy` opens the project (`main.rs`), so the desktop entry's `%F` works.
- No licence file exists yet: the packages say "no licence chosen, all rights reserved".
- **Checked (1 Nov):** the .deb (19 MB, 63 MB installed; binaries stripped as Debian's
  are) passes lintian but for missing man pages, installs on this Ubuntu 26.04 (`.canopy`
  files then open in canopy), and its `canopy-mcp` lists the 33 tools. The PKGBUILD built,
  installed and ran the same checks in an `archlinux` container (from a snapshot of the
  working tree committed inside the container); namcap's only error, the licence file, is
  fixed. A .deb built here needs glibc ≥ 2.43, as new as the system that built it.

### 25. Rack presets — done (2 November 2026)

- A track's instrument and effects saved as one preset and loaded onto any track.
  Format: `project::RackFile` (`preset.rs`: `RackInstrument`, `RackEffect`) in
  `presets/racks/<name>.ron`. `Project::rack(track)` takes one, and `Project::load_rack`
  replaces the chain (its automation goes with it) and, if the rack has one, the
  instrument. Instrument params are written as stored, not through
  `set_instrument_params`, since a plugin's descriptor may not be registered yet. Plugins'
  instances come from the host's `reconcile`, as pasted ones do.
- Left out on purpose: automation, sidechains, modulation and the routing graph (a rack
  loads as a straight chain). The MCP tools don't list or load racks yet.
- UI: the Rack menu beside "+ Effect" (`devices/preset_menu.rs`, `rack_button`; the
  save window is shared with device presets). On a track without an instrument only the
  effects load, and the status bar says so.
- Tests: `project` `preset::tests` (4), `ui` `presets::tests::racks_are_kept_apart_from_devices_presets`,
  `ui_tests::effects::save_and_load_a_rack`.

### 26. Patcher: effects as one effect with macros — done (2 November 2026)

FL Studio's Patcher, as asked: a chain of effects, patched as a graph, wrapped into one
effect whose outside is a few knobs. This is how the patcher workstream (A, plan §§7.2, 8)
arrived: the graph engine and canvas work for any chain, but the UI offers the canvas only
inside Patchers (`patcher::READY` is still `false` for track-level graphs).
- **Model** (`project/src/patch.rs`): `canopy.patcher` (`effects/src/patcher.rs`, a
  passthrough with 8 macro params, category Routing) carries `Device.patch: Option<Box<Patch>>`
  = `{ devices, routing, links: Vec<MacroLink>, names }`. Chains are addressed by
  `Chain { Master, Track, Patch(DeviceId) }`; every chain edit (`devices`, `add_device`,
  `insert_devices`, `move_devices`, `routing`, `ensure_routing`, `normalize_routing`) takes
  `impl Into<Chain>`, so `Option<TrackId>` still works. `locate_device` finds any effect;
  `find_device` gives the top-level place (the outermost Patcher's); `Track::device` and
  `walk_devices` look inside. Automation, controller links and routes of effects inside
  live on the outermost track. Copy/paste/rack presets renew ids all the way in (graph nodes
  and macro links follow); a Patcher can't move into itself. `wrap_in_patcher`,
  `unpack_patcher`. Graph edits for the canvas: `connect`, `disconnect`, `set_cable`,
  `add_node`, `remove_node`, `set_split` (validated; a refused edit changes nothing, ids
  included).
- **Macros:** `link_macro(patcher, index, device, param, min, max)` (fractions of the knob's
  range, min > max inverts), `unlink_macro`, `rename_macro`. Setting a macro writes the knobs
  it drives through the project (`apply_macros`, recursive into Patchers inside), so what
  knobs show is what plays; `driven_knobs` is what a live gesture sends the engine
  (`Session::live_device_param`, `live_macro_link`).
- **Format 3:** any Patcher makes the file format 3 (`FORMAT_VERSION`), since older builds
  would load it empty; `repair()` gives Patchers a Patch, repairs the chains inside, drops
  links to effects not directly inside.
- **Engine** (`engine/src/patches.rs`): `flatten` turns a chain holding Patchers into one
  pool (Patchers left out) and one graph: each Patcher's node is replaced by its graph, its
  Input/Output spliced out, cable levels adding (parallel duplicates summed as amplitudes);
  synthetic node ids are `NodeId(device id)` and the top of the range, stable across
  compiles. A bypassed Patcher is a wire, its effects kept unconnected (instances survive).
  Macro lanes compile into lanes on the knobs they drive (`macro_targets`, affine in
  normalized space, through nested Patchers), replacing those knobs' own lanes. Chains
  without Patchers are untouched (Linear fast path).
- **UI:** the Patcher panel (`devices/patcher.rs`): 8 macro knobs (dim while unused), Open.
  Open sets `TimelineView::open_patcher`; the device view then shows "‹ Back", the path, and
  Effects | Graph (`effect_rack::show` on `Chain::Patch`, or `patcher::canvas`); "+ Effect"
  adds inside. The rack menu has Into Patcher / Unpack, and Move/Copy to ▸ lists Patchers and
  "Out of the Patcher". Knob right-click: Map to Macro ▸ (a macro's first link takes the
  knob's place), and on a macro its name field and links with live range editing. The canvas
  (`patcher/{canvas,graph,menus}.rs`): nodes, ports, cables, connect by dragging (refused
  ones red), cable menu (mute, level, delete), split/join menus, Add Effect (plugins too),
  Tidy, pan/zoom/Fit; Delete removes a selected cable (`delete_selected_cable`, called from
  `effect_rack::shortcuts`).
- **Tests:** `project` `patch::tests` (12), fuzz ops 32–35 (Patchers, on their own RNG
  stream), `routing_and_modulation` (format 4 refused); `engine` `patches::tests` (6),
  `tests/patchers.rs` (a Patcher bit-identical to its effects in the chain, bypass, a dry path
  inside, macro automation), `tests/graph.rs` (12, the graph engine); `ui`
  `ui_tests/patchers.rs` (3), `ui_tests/patcher.rs` (8, the canvas).
- **Left for later:** MCP tools can read what's inside a Patcher (`inside` in device
  summaries) and set macros by id, but have no tools to link macros or patch graphs; split
  parameters aren't automatable; track-level graphs have no UI yet (flip `patcher::READY`
  and show the canvas for `Chain::Track` when wanted). Unpacking a Patcher with a graph
  inside keeps only the order. A Patcher's macro presets save macros only, not the inside
  (a rack preset keeps the inside).
- **Found by the fuzz, not fixed (clip code):** an intermediate fuzz run (ops 32–35 drawing
  from the main RNG; seed 981969, step 286) hit "overlap on track" after `set_clip_bounds`
  on crossfaded audio clips (op 5): a clip trimmed next to two crossfading ones left a
  three-way overlap. Not Patcher code; reproduce by making ops 32–35 use `rng`.

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

## Per-note bends: instruments and plugins — done (29 October 2026)

Tasks 3 and 4 of `docs/superpowers/plans/2026-10-29-note-bends-and-slicer.md`: what
plays a `NoteEventKind::NoteBend` (sent by the compiler, Task 2).

- **Built-in instruments** find the voices a note-off of that key would release and bend
  them on top of whatever pitch they have (glide, wheel, vibrato, modulation), gliding to
  each value within a block; a bend arriving before a voice has sounded (the one the
  compiler sends with a note that starts bent) is where it starts. A new note starts
  unbent. Poly Synth and Simple Osc: a per-voice ratio folded into the oscillators' wheel
  ratio (`dsp::Oscillator::modulate_pitch_ratio`). Wavetable: semitones added to the
  voice's pitch modulation at each control tick, so every oscillator and the sub follow;
  in the mono modes each key's bend is kept for falling back to a held key. SFZ
  Sampler: cents in the control update's pitch, a `Smoothed` ticked per control
  interval. Drum Synth: the newest hit of the pad, if that note started it, as a
  per-sample ratio on its layers' pitch. Guitar: the string's own bend, added to the
  wheel's in its loop tuning (the same path as the wheel and vibrato); bends of notes
  still waiting in a strum are kept for their pluck, and a group's bends are handled
  after its note-ons.
- **Plugins:** `plugins::Event::NoteTuning { channel, key, semitones }`. CLAP: a
  `CLAP_NOTE_EXPRESSION_TUNING` note expression (note id -1, matched by key and channel,
  as notes are sent), for plugins taking CLAP notes. VST3: a tuning
  `NoteExpressionValueEvent` (0.5 + semitones/240) on the note's id, only for plugins
  whose controller lists `kTuningTypeID` (`INoteExpressionController`, bus 0, channel 0);
  for those notes carry ids (channel·128 + key), others keep -1. VST2 and MIDI-only CLAP
  plugins don't hear it. `hosting` turns `NoteBend` into it.
- **canopy-clap** takes tuning note expressions as `NoteBend` (cents rounded and clamped
  to `i16`); a wildcard key is ignored. CLAP needs no declaration for it.
- **Tests:** `instruments/tests/bends.rs` (every instrument: a held note bent +12 measures
  an octave up with nothing left at the old pitch; one note of a chord bent leaves the
  other as it is in the unbent chord; two overlapping notes of a key; a note starting
  bent; a bend sent a block at a time glides without clicks); `instruments/tests/realtime.rs`
  bends notes in every case; `plugins` unit tests on the CLAP and VST3 translation and
  `tests/realtime.rs`; `hosting` adapter; canopy-clap's bridge and its host test (the riff
  bends one string on its own, bit-identical through the plugin and in canopy). Golden:
  no existing output changed.
- **Not tested:** a real VST3 plugin that takes tuning note expressions (none at hand;
  the test fixtures don't take them), and a third-party CLAP plugin's response.

### Bends and the Slicer: the piano roll, the merge and the review (29 October 2026)

- **Piano roll Bend mode** (`ui/src/piano_roll/bends.rs`): B or the Bend button; a drag
  across a note draws its curve (held at its pitch until the drag began, snapped to the
  grid and semitones; Shift cents, Alt off the grid), points drag, Alt-drag curves a
  segment, double-click removes a point, right-click clears the note's bend (right-click
  already deletes notes outside the mode, so there is no Clear Bend menu item).
- Built in three parts: the bend data and `NoteBend` compile (main session), the
  instruments and plugins (one agent) and the Slicer (another), each in its own worktree,
  merged into `bends-and-slicer`. canopy-clap's host test now loads the newer of the two
  library copies cargo leaves (a stale `target/debug/libcanopy_clap.so` failed it).
- **Fixed after the final review:** a note starting bent lost its bend for a block when
  its first sample rounded to the tick before it (`push_bends` clamps to the note);
  `reverse` now mirrors held values and segment curves exactly; chop and glue keep a bend's
  implied step (not bent before its first point) a step instead of a ramp
  (`note_tools::explicit_bend`); the Slicer's tempo guess is `None` for files no 1–16 bar
  count fits (one-shots, whole songs), which then play at their own speed instead of a
  clamped 40 or 300 BPM.
- **Left from the review:** the Slicer glides into a bend sent at its note-on (64 frames)
  and bends release tails of the same key with Cut off; VST3 note ids are one per key
  (overlapping same-key notes share one); a drag that leaves only 0-value points saves
  them; loading doesn't sort or clamp bend points from hand-edited files; the bend
  compiler doesn't prefer notes over bends at the per-block event cap, nor reserve room
  for bends; transient detection runs on the UI thread; `paint_bends` samples whole notes
  even off-screen; `a_bend_follows_its_pitch_when_two_notes_overlap` asserts nothing
  after the release.
