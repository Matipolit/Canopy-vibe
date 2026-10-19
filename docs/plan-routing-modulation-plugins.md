# Canopy: plan for the effect patcher, the modulation system and CLAP/VST3 hosting

## 0. Summary

The plan rests on four decisions. The rest of this document details them.

1. **Devices stay a flat pool on the track, and routing is an optional overlay.**
   - `Track::devices: Vec<Device>` keeps every effect and its `DeviceId`.
   - A new `routing: Option<Routing>` describes a node graph over those devices.
   - `None` means the chain is linear, as it is today, so every existing file, rack interaction, automation lane, controller link and engine instance-adoption path is unchanged.
   - Edits that leave the graph a straight line normalize it back to `None`. The rack therefore stays the default view.
2. **Modulation is track-local data.**
   - `Track::modulators: Vec<Modulator>` holds standalone modulators: LFO, envelope, follower, macro, random, MIDI.
   - `Track::modulations: Vec<ModRoute>` holds the routes. Each route goes from a source to an `AutomationTarget`, which is the same addressing automation and MIDI learn already use.
   - The engine evaluates modulation once per 64-frame block, which is the existing `MAX_BLOCK`. It adds the result to the base value (the knob or automation) in normalized space.
   - The result reaches the device through a new `modulate()` trait method that ramps within one block. CLAP plugins get it as non-destructive `CLAP_EVENT_PARAM_MOD`.
   - Instruments export mono modulators through a new trait method, `Instrument::mod_outputs`.
3. **Plugins are ordinary `Device`s and instruments.**
   - `EffectRef`/`InstrumentRef` gain a `format` (BuiltIn | Clap | Vst3).
   - A new `plugin: Option<PluginData>` field holds the name, vendor, path hint and an opaque state blob. Parameters live in the existing `params` map, keyed by the plugin's u32 param id.
   - A glue crate (`crates/hosting`) wraps the plugin-host agent's crate in `effects::Effect` and `instruments::Instrument` adapters, so the engine hosts plugins through the paths it already has.
   - A small process-global descriptor registry, built from leaked `&'static` descriptors, makes every `descriptor()` call site work for plugins without refactoring.
4. **Shared trait and command changes land first, from one agent.** Phase 0 is interfaces and Phase 1 is the foundation. Then three agents work in parallel on files that don't overlap:
   - A: graph and Patcher.
   - B: modulation.
   - C: plugin integration.

---

## 1. Survey of the current code

### Project model (`/home/m.polito/Personal-projects/canopy-vibe/crates/project/src`)

**`track.rs`**
- `Track { id, name, color, mixer, devices: Vec<Device>, automation, kind }`.
- `Device { id: DeviceId, effect: EffectRef, params: BTreeMap<String,f32>, bypassed, sidechain: Option<TrackId>, folded }`. The `params` map stores user-set values in each parameter's own units; missing entries take the default.
- `TrackKind::Midi { instrument: InstrumentRef, params, clips, file, tables }`.

**`lib.rs`**
- `Project` has `master_devices: Vec<Device>`. The master has a chain but no automation lanes.
- Every id (track, clip, note, device, source) comes from one counter: `next_id` / `alloc_id`.

**`devices.rs`**
- Chain edits are all addressed by `DeviceId`: add, remove, copy, insert, duplicate, move, set_param, set_params, sidechain.
- Automation lanes and controller links follow devices when they're moved, copied or removed. `CopiedDevice` = device + its lanes.
- `find_device` walks master then tracks.

**`automation.rs`**
- `AutomationTarget { Volume, Pan, InstrumentParam(String), DeviceParam { device, param } }`.
- Lanes hold normalized 0..1 points with a curve.
- `controller.rs::ControllerLink` reuses `AutomationTarget`. That makes `AutomationTarget` the project's de facto parameter address.

**`history.rs`**
- Undo stores whole `Project` clones and decides whether an edit changed anything with `PartialEq`.
- Any new model type must therefore be `Clone + PartialEq`, and cheap to clone.

**`file.rs`**
- Files are RON, wrapped as `{ format: 1, project }`.
- `repair()` makes ids unique, keeps effects this build doesn't know (they pass sound through, and saving keeps them), clamps values and prunes sidechains.
- `FORMAT_VERSION` is "bumped whenever a change would make old files load wrongly".

### Traits (`/home/m.polito/Personal-projects/canopy-vibe/crates/effects/src/lib.rs`, `/home/m.polito/Personal-projects/canopy-vibe/crates/instruments/src/lib.rs`)

**`Effect`**
- Methods: `prepare`, `reset`, `process(left, right, &Context)` (in place, stereo), `params() -> &'static`, `set_param(index, value)`, `meters`.
- `Context { bpm, beat, playing, sidechain }`.
- `EffectDescriptor` is a `static` with: `params`, `settings` (UI only), `meters`, `tap`, `sidechain`, `latency: fn(&[f32], f32) -> usize`, `create: fn()`.
- `EffectRef { id, version }`. `descriptor()` looks only at the built-in list.

**`Instrument`**
- Methods: `prepare`, `reset`, `process(events, l, r)`, `params`, `set_param`, `set_tempo`, `meters`, `set_data`.
- `InstrumentRef { format: BuiltIn | Clap /* reserved, descriptor() → None */, id, version }`.

**`ParamDescriptor { id: &'static str, name, kind: Float{min,max,default,unit,logarithmic} | Enum{labels,default} }`**
- Has `to_normalized`/`from_normalized`/`format`/`parse`.
- Every descriptor in the system is `&'static`.

**Smoothing**
- Every built-in effect smooths its parameters over about 20 ms. Examples: `SMOOTH_MS` in compressor, delay and clipper; `GLIDE_MS` in eq.
- The Wavetable glides parameters over 20 ms at its control rate (`PARAM_RAMP_MS`).
- The fader also ramps over 20 ms (`fader.rs`).
- Per-block modulation pushed through these smoothers would be low-passed to roughly 8 Hz. That's why a separate fast-ramp `modulate()` path is needed.

### Engine (`/home/m.polito/Personal-projects/canopy-vibe/crates/engine/src`)

**`arrangement.rs`: compiling a snapshot**
- `Compiler::compile(project, impl AudioLookup)` builds an `Arrangement`:
  - `nodes` in post-order, each with `source`, `effects: Box<[EffectSlot]>`, `send`, `latency`, `compensation`, `automation`, and `processors: Option<Box<Processors>>`.
  - `buses` (0 is the master), `sends`, `master_effects`.
- New tracks and devices get processors built on the UI thread. `None` means the engine should adopt the instance it already has.
- `AutomationKind::Effect(slot, param, ParamDescriptor)`: **`slot` is the index in `track.devices`**.
- `AudioLookup` is the "things the compiler needs from the UI" trait: `source`, `rendered`, `instrument_data`. The session implements it in `/home/m.polito/Personal-projects/canopy-vibe/crates/ui/src/renders.rs`, and exports in `export.rs`.

**`chain.rs`: running chains**
- `build_chain` creates instances only for unseen `DeviceId`s.
- `adopt`/`take` move instances from the old snapshot by `DeviceId`, searching every node. That's why moving an effect to another track keeps its tail.
- `run` processes in place and handles bypass (resetting when the effect comes back), sidechains from `sends`, meters and taps.

**`audio.rs`: the audio thread**
- `MAX_BLOCK = 64`.
- `render` per block:
  1. Clear the buses.
  2. For each node: apply automation (**only while playing**), then render the source, then run the chain in place, then copy to the send, then the fader, then compensation, then add to the output bus.
  3. Run the master chain.
- `swap()` moves processors by `TrackId` and effects by `DeviceId`. It pushes changed static values, except automated ones. The old snapshot goes to the garbage queue, and the engine waits if that queue is full.

**`command.rs`**
- Small live commands, including `SetDeviceParam { device, index, value }`, found by linear search.

**Latency (`compensation.rs` + `compensate()` in `arrangement.rs`)**
- `chain_latency` sums the descriptors' `latency(values, rate)` for devices that aren't bypassed.
- Arrival is computed per bus, and each node gets the delay that lines it up there.
- `Compensation` is a delay line of up to `MAX_DELAY = 8192` samples with a 256-sample crossfade.
- `Compiler::latency()` feeds the playhead, recording and `Bounce::range`.
- Sidechain sends aren't compensated.

**Sidechains**
- `sidechain_sources()` gives each listened-to track a send bus. The track's sound after its effects and before its fader is copied there.
- A source that renders after its listener reaches it one block late.

**Meters and taps**
- Atomic `Meters` per device and per instrument, and an rtrb `TapFrame` ring per tapped device.
- Read through `Compiler::device_meters`, `instrument_meters` and `drain_tap`.

### UI (`/home/m.polito/Personal-projects/canopy-vibe/crates/ui/src`)

- **`effect_rack.rs`:** the linear rack. Title bars with select, fold, bypass, sidechain picker and latency label; `DeviceDrag` drag-and-drop within and between chains and onto headers; menus; keyboard ops.
- **`devices/panel.rs`, `Panel`:**
  - Built from static descriptor slices.
  - Knobs, choices and toggles carry the automation/controller menu.
  - Live edits go through `session.live_device_param`, which sends `Command::SetDeviceParam`; `end_live` makes the gesture one undo step.
  - `knob_with(..., live: Option<f32>)` already draws a modulation dot (`widgets/knob.rs::Knob::modulated`).
- **`devices/mod.rs`:** dispatches panels by `panel.kind`, falling back to `generic.rs`.
- **`device_view.rs`:** the mixer column and "+ Effect" button, then the instrument and rack in a horizontal scroll.
- **`app.rs`:** `Detail::{Clip, Device}`, switched with Shift+Tab.
- **Pop-out windows:** the analyzer pops out with `ctx.show_viewport_immediate` (`devices/analyzer.rs::windows`). The Patcher can reuse that pattern.
- **`timeline/track_header.rs::automation_targets`:** lists device params via descriptors.
- **`session.rs`:** `edit`, `begin_live`/`end_live`, `live_edit`, `view_edit` (not undoable, used for folds), `rebuild()`, which compiles with `renders::Lookup`.
- **Event loop:** `main.rs` runs eframe on a winit loop built by canopy itself. The UI thread is the process main thread, which is good for CLAP's and VST3's main-thread rules.

### Wavetable modulation today (`/home/m.polito/Personal-projects/canopy-vibe/crates/instruments/src/builtin/wavetable*`)

- 4 LFOs (modes Free, Retrig and Env; free-running copies live in `global_lfos`), 3 envelopes, and 4 macros, which are parameters.
- A 16-slot matrix: source, dest, amount, aux (scale-by), curve (`matrix_curve`), and polarity Source/Unipolar/Bipolar. It's evaluated per voice every 32-sample control tick.
- `meters()` exposes LFO values and phases, envelope levels and per-slot contributions, from the newest voice or the global LFOs.
- Everything is internal. Nothing outside the instrument can read an LFO except the UI, through meters.

### Constraints that shape the design

1. **Nothing is allocated or freed on the audio thread** (`assert_no_alloc`; `engine/tests/realtime_safety.rs` and the instruments/effects realtime tests).
   - Graph scratch buffers, delay lines and modulator processors must be built on the UI thread.
   - Plugin processors must be stopped on the audio thread (`retire`) before they go back in the garbage.
2. **Processor continuity is by stable id.** New per-snapshot state (split filters, cable delays, modulators) must be keyed by ids and adopted the way effects are.
3. **`&'static` descriptors are everywhere:** `Device::values`, `Panel`, automation menus, `AutomationKind`. Plugin parameters are dynamic, so either they get a registry or every call site gets refactored.
4. **The `slot` index in `AutomationKind::Effect` is the `track.devices` index.** Keeping devices in a pool preserves it.
5. **Serde/RON compatibility.**
   - Avoid `#[serde(flatten)]`, which is fragile in RON.
   - Use `default` + `skip_serializing_if` for new fields; adding enum variants is safe.
   - Struct type names don't appear in the files (`PrettyConfig` has no `struct_names`), so renaming a type is safe.
6. **Undo is whole-project cloning plus `!=`.** Plugin state blobs must be `Arc`-shared.
7. **Concurrent agents share one working tree, and git has no commits yet.**
   - Required struct fields added to shared descriptors (`EffectDescriptor`, `BuiltInInstrumentDescriptor`) would break other agents' builds.
   - Shared interfaces should be additive default methods.
   - Recommendation: make an initial commit now and run each implementer in its own worktree.
8. **The 64-frame block is already the automation resolution**, and automation is applied only while playing.

---

## 2. Research: how others do it, and what the plugin APIs require

### How other DAWs and synths do it

**Bitwig**
- "Unified modulation system": a modulator's routing button puts the device in mapping mode, and you drag on a parameter to set depth and direction. Blue is monophonic, green polyphonic, and modulators can modulate other modulators.
- Containers: FX Layer (parallel chains), FX Selector, and Multiband FX-2/FX-3 (minimum-phase crossovers with no latency; there are complaints about transparency).
- Modulation reaches the same level of nesting or below.
- Lesson: per-device modulators with drag-to-map, and container devices for splits.

**FL Studio Patcher**
- A free node map with three cable types: audio (yellow), events/notes (green) and parameter control (red).
- Audio wires have send levels and can be muted.
- "From FL Studio" / "To FL Studio" are the I/O nodes.
- Controllers such as Formula Controller or Peak Controller drive parameters through cables.
- Lesson: per-cable gain and mute, explicit Input/Output nodes, and modulation shown as cables.

**Ableton Racks**
- Parallel chains, with up to 16 macros (8 shown by default).
- Map mode assigns a macro to any number of parameters with per-mapping Min/Max; Min above Max inverts.
- Lesson: macros as many-to-one controllers with ranges. Canopy's route `amount`/`polarity` covers this relative to the knob.

**Reaper**
- Parameter modulation per FX parameter: a baseline value plus LFO, an audio control signal (follower measured at the FX input), and parameter link, all applied per block. Linking across tracks takes workarounds.
- Reaper 7 adds FX containers and "run in parallel with previous FX".
- Pin routing maps plugin pins to up to 64 track channels.
- Lesson: baseline + modulation is additive, and parameter linking within the track is enough for most needs.

**Serum 2**
- The FX rack has Main, Bus 1 and Bus 2. Oscillators can be split between buses, and the buses feed Main or go direct.
- Three "splitter" modules: Low/High, Low/Mid/High and Mid/Side.
- Any mod source can be dragged onto any FX knob.
- FX are monophonic, "like plug-in inserts after Serum".
- Lesson: the FX tab should be a view of real track devices. Splitters map to the graph's Split nodes. Serum's buses map to future multi-output instruments feeding the graph's Input node.

**Vital**
- While dragging, droppable controls light up and you can audition before letting go; the wheel sets depth before the drop.
- Per-route bipolar/unipolar, stereo, bypass, and a power/morph curve.
- Ring handles: drag to set the amount, and right-click for Remove / Bypass / Make Bipolar / Stereo.

### CLAP host requirements

**Threading** (`plugin.h`)
- `init`, `activate` and `deactivate` are [main-thread].
- `start_processing`, `stop_processing`, `reset` and `process` are [audio-thread & active (& processing)].
- `on_main_thread` is main-thread.
- **Design consequence:** start and stop processing on the audio thread (a lazy start on the first `process`, and a `retire()` call at swap), and deactivate/destroy on the UI thread only after the audio half has come back.

**Parameters** (`params.h`)
- `get_info`, `get_value` and `value_to_text` are main-thread.
- Flags: STEPPED, ENUM, HIDDEN, READONLY, AUTOMATABLE, MODULATABLE, BYPASS.
- GUI edits come back as `CLAP_EVENT_PARAM_VALUE` output events during `process`/`flush`, wrapped in GESTURE_BEGIN/END, after the plugin calls `request_flush`.
- `rescan(ALL)` is only allowed while deactivated; otherwise the plugin calls `request_restart`.

**Events** (`events.h`)
- `PARAM_MOD` sets a non-destructive modulation amount. "The value heard is param_value + param_mod."
- Events are sorted by sample time.
- Polyphonic modulation goes through the note_id/key wildcards; it's out of scope here.

**Latency** (`latency.h`)
- Latency may only change during `activate`. An active plugin must `request_restart`.
- **Design consequence:** a restart round-trip (Section 6).

**GUI** (`gui.h`)
- X11 embedding follows XEmbed.
- Wayland: "embed is currently not supported, use floating windows".
- Floating windows aren't managed by the host; `set_transient` keeps one above another window.
- `request_resize` and `closed(was_destroyed)`.
- Linux timers and fds come from the `timer-support` and `posix-fd-support` host extensions.

### VST3 host requirements

**Parameter changes**
- Automation goes in as one `IParamValueQueue` per parameter inside `IParameterChanges`.
- Points are `(sampleOffset, normalized value)`, and each point defines a linear ramp from the previous one.
- The host also has to update the GUI through `IEditController::setParamNormalized` on the UI thread.
- There is no non-destructive modulation, so modulation means sending base + mod as parameter changes.

**Linux run loop**
- The host must provide `Linux::IRunLoop` (`registerEventHandler`, `registerTimer`).
- It's reached through the `IPlugFrame` passed to `IPlugView::setFrame`, and through `IPluginFactory3::setHostContext` for timers while the editor is closed.
- yabridge's author reports that REAPER needs GUI operations to happen from IRunLoop handlers, and yabridge routes `resizeView` and context menus through them.
- Implement it properly: poll fds and timers on the UI thread.

**Licensing and bindings**
- VST 3.8 (October 2025) is MIT-licensed. Steinberg's trademark rules still apply if canopy uses the VST name or logo.
- 3.8 also previews Wayland support for plugin editors (`IWaylandHost`/`IWaylandFrame`).
- The `vst3` crate (coupler-rs, MIT/Apache) now ships pre-generated bindings with no SDK or libclang needed.

**Other host duties**
- Component/controller split: connect the `IConnectionPoint`s, and call `setComponentState` after `setState`.
- `IComponentHandler::beginEdit/performEdit/endEdit` (UI thread). For VST3 the host must forward these to the processor itself.
- `restartComponent` flags such as `kLatencyChanged` and `kParamTitlesChanged`.
- Pitch bend and CC map to parameters through `IMidiMapping`.

### yabridge (Windows plugins on Linux)

- It bridges VST2, VST3 and, since 5.0, CLAP.
- `yabridgectl sync` writes Linux-native bundles, for example `~/.vst3/yabridge`. The CLAP folder isn't documented in the changelog; it's presumably under `~/.clap`.
- Editors embed through X11 wrapper windows, and yabridge forces Wine to use X11.
- **Recommendation:** canopy has nothing yabridge-specific to do. Scan `~/.vst3` and `~/.clap` recursively, give editors X11 parent windows (XWayland on Wayland sessions), and implement IRunLoop. Document the yabridgectl setup.

---

## 3. Data model (project crate)

### New ids (`ids.rs`)

`id_type!(NodeId); id_type!(ModulatorId); id_type!(ModRouteId);` They come from the same `alloc_id` counter, and `repair()` includes them in its max-id and uniqueness pass.

### Track, Project and Device additions (all optional, so old files load unchanged)

```rust
pub struct Track {
    /* existing fields unchanged */
    /// Every effect on the track (the pool). When `routing` is None this order IS the chain;
    /// otherwise it's display order (kept topological by normalization).
    pub devices: Vec<Device>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<Routing>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modulators: Vec<Modulator>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modulations: Vec<ModRoute>,
}
pub struct Project { /* ... */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_routing: Option<Routing>,
    // master_modulators / master_modulations: later (see open question 1)
}
pub struct Device { /* existing */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginData>,
}
// TrackKind::Midi gains the same `plugin: Option<PluginData>` field.
```

### Routing (new file `routing.rs`)

```rust
pub struct Routing { pub nodes: Vec<GraphNode>, pub cables: Vec<Cable> }
pub struct GraphNode {
    pub id: NodeId,
    pub kind: NodeKind,
    #[serde(default)] pub pos: [f32; 2],      // canvas position: view data, like `folded`
}
pub enum NodeKind {
    Input,               // the track's sound (instrument, clips, group bus); out 0 (more outputs later: multi-out instruments)
    Output,              // to the fader; in 0 sums every cable into it
    Device(DeviceId),    // in 0 main, in 1 sidechain (if the effect listens); out 0
    Split(Split),        // in 0; out 0..n
    Join(Join),          // in 0, in 1 → out 0 (decodes L/R or M/S)
}
pub enum Split { Bands { crossovers: Vec<f32> /* Hz, 1..=3 */, slope: Slope }, LeftRight, MidSide }
pub enum Join { LeftRight, MidSide }
pub struct PortRef { pub node: NodeId, pub port: u8 }
pub struct Cable {
    pub from: PortRef,
    pub to: PortRef,
    #[serde(default)] pub gain_db: f32,       // per-cable send level (FL Patcher-style)
    #[serde(default)] pub muted: bool,
}
```

**Routing semantics**
- Any input port sums every cable into it, so no "Sum" node is needed. Dry/wet of a parallel branch is two cables with gains.
- L/R and M/S splits output dual-mono stereo: (L,L)/(R,R) or (M,M)/(S,S). The matching `Join` decodes from the first channel of each input.
- Band splits use Linkwitz-Riley crossovers, which are minimum phase with no latency, like Bitwig's. The bands sum to an allpass.
- **v1 limit:** split parameters are edited, not automated. A later `AutomationTarget::NodeParam` can add that.

**Invariants**
These are checked by `Routing::validate(&devices)`, enforced by `repair()` (an invalid graph is dropped back to `None`, keeping devices in a topological order), and added to the `project/tests/fuzz.rs` checker:
- exactly one Input and one Output;
- each pool device appears in exactly one `Device` node, and vice versa;
- ports exist;
- no duplicate cables;
- **acyclic** (open question 4);
- node ids unique.

**Normalization**
- After every routing or device edit, `normalize_routing(owner)` runs.
- If the graph is Input→d1→…→dn→Output with unity, unmuted cables, only Device nodes and no extra edges, it sets `routing = None` and reorders `devices` to match.
- If the graph stays non-linear, it reorders `devices` topologically and splices pool devices that were added with no node in front of Output.

### Modulation (new file `modulation.rs`)

```rust
pub struct Modulator {
    pub id: ModulatorId,
    pub kind: ModulatorRef,                    // { id: "canopy.lfo", version } — descriptors in the new `modulators` crate
    #[serde(default)] pub params: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub listen: Option<Listen>, // follower input
    #[serde(default)] pub folded: bool,
}
pub enum Listen { Input, Node(PortRef), Track(TrackId) /* via the existing send buses */ }
pub enum ModSource {
    Modulator { id: ModulatorId, #[serde(default)] output: u8 },
    Instrument(String),                        // a ModOutput id the track's instrument exports ("lfo1", "macro2")
}
#[derive(Default)] pub enum Polarity { #[default] Source, Unipolar, Bipolar }  // same meaning as the Wavetable matrix
pub struct ModRoute {
    pub id: ModRouteId,
    pub source: ModSource,
    pub target: AutomationTarget,              // Volume | Pan | InstrumentParam | DeviceParam | ModulatorParam
    pub amount: f32,                           // -1..=1 of the target's normalized range
    #[serde(default)] pub polarity: Polarity,
    #[serde(default)] pub curve: f32,          // -1..=1, the Wavetable's matrix_curve
    #[serde(default, skip_serializing_if = "Option::is_none")] pub scale_by: Option<ModSource>, // Serum's "aux"
    #[serde(default)] pub bypassed: bool,
}
// automation.rs: AutomationTarget gains `ModulatorParam { modulator: ModulatorId, param: String }`
// (LFO rate automation/modulation, macro automation, MIDI learn of macros — all for free).
```

**Macros** are `canopy.macro` modulators: one `value` parameter that is also the output. The UI shows every Macro on the track as a row of knobs.

### Plugins (new file `plugin.rs`)

```rust
// instruments: `pub enum Format { BuiltIn, Clap, Vst3 }` (InstrumentFormat becomes an alias; add Vst3)
// effects::EffectRef gains: #[serde(default, skip_serializing_if = "Format::is_builtin")] pub format: Format
//   CLAP id = plugin id string; VST3 id = class UID as 32 hex digits; version = plugin's version string.
pub struct PluginData {
    pub name: String,
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub path: Option<PathBuf>, // last seen; id is authoritative
    #[serde(default, skip_serializing_if = "Blob::is_empty")] pub state: Blob,           // CLAP state / VST3 component+controller
}
#[derive(Clone, Default)] pub struct Blob(Arc<[u8]>); // serde as base64 string; PartialEq = ptr_eq || bytes
```

**Plugin parameter values**
- They live in `Device::params` (or `TrackKind::Midi::params`), keyed by the plugin's u32 param id as a decimal string.
- CLAP stores plain values; VST3 stores normalized values.
- Existing `DeviceParam { device, param }` automation, controller links and modulation targets therefore work unchanged.

### Edits, references and undo

**What deletes, moves and copies take with them**
- `remove_device`, `move_devices`, `insert_devices` and `copy_devices` already carry automation lanes and links. They now also carry:
  - mod routes whose target is the device;
  - the device's graph node, spliced out on removal (input cables reconnect to its outputs);
  - inserts on the main path before Output.
- `CopiedDevice` gains `routes: Vec<ModRoute>`.
- Changing the instrument also drops routes to `InstrumentParam` and `Instrument(..)` sources, the same way it drops lanes today.
- Deleting a track prunes `Listen::Track` and sidechain references, extending `prune_sidechains`.

**Undo**
- Undo stays the whole-project clone, so every routing, modulation and plugin edit is undoable through `Session::edit`.
- Live gestures (dragging a ring amount or a cable gain) use `begin_live`/`end_live` plus new live commands.
- Canvas node moves are `view_edit` (not undoable, like folds). `ViewState` carries them over undo the way `device_folds` already does.

### File format

- Linear projects serialize byte-identically to today.
- **Recommendation:** write `format: 2` only when a project uses routing, modulators or plugins. Old canopy builds then refuse those files instead of silently dropping routing, and they still open simple projects.
- Tests:
  - a fixture string in today's format (devices + automation + sidechain) loads and equals the expected model;
  - files with every new feature round-trip;
  - `!text.contains("routing")` for linear projects.

---

## 4. Shared interface additions (Phase 0, additive only)

All of these are default methods, so nobody's build breaks.

### `effects/src/lib.rs` (effects agent's file; apply with their agreement)

```rust
pub trait Effect: Send {
    /* existing */
    /// Like set_param, for a value a modulator moves: called at most once per
    /// parameter per engine block (MAX_BLOCK frames), before process. Reach the
    /// value within that block (ramp ≤ 1 block, not the 20 ms knob smoothing)
    /// and stay stable however fast it moves. Enum params just switch.
    fn modulate(&mut self, index: usize, value: f32) { self.set_param(index, value) }
    /// Audio thread, when the engine stops running this instance (removed, or
    /// replaced) — before it goes back to the UI thread to be dropped.
    /// Plugins call stop_processing here.
    fn retire(&mut self) {}
}
// EffectRef.format (above) + an external registry:
pub fn register_external(format: Format, id: &str, d: EffectDescriptor) -> &'static EffectDescriptor; // leaks; replaces
pub fn find_external(format: Format, id: &str) -> Option<&'static EffectDescriptor>;
// EffectRef::descriptor(): BuiltIn → find_builtin, else find_external. The registry is a
// RwLock<HashMap>, read only on the UI thread (the audio thread never calls descriptor()).
```

### `instruments/src/lib.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModOutput { pub id: &'static str, pub name: &'static str, pub bipolar: bool }
pub trait Instrument: Send {
    /* existing */
    fn modulate(&mut self, index: usize, value: f32) { self.set_param(index, value) }
    fn retire(&mut self) {}
    /// Current value of each exported mono modulator (descriptor's mod_outputs(), in
    /// order) as of the end of the last process: -1..=1 if bipolar else 0..=1.
    /// Audio thread, real-time safe; `out.len()` == that list's length.
    fn mod_outputs(&self, _out: &mut [f32]) {}
}
impl BuiltInInstrumentDescriptor {
    pub fn mod_outputs(&self) -> &'static [ModOutput] { builtin::mod_outputs(self.id) } // match on id in builtin/mod.rs
}
// + register_external / find_external for plugin instruments, as for effects.
```

The lookup is a function rather than a new required field on `BuiltInInstrumentDescriptor` on purpose. A new field would force edits to the Wavetable, guitar and drums `DESCRIPTOR` consts while other agents are editing those files.

### Requests to the concurrent agents

**Wavetable agent**
- Define `pub const MOD_OUTPUTS: &[ModOutput]`: `lfo1..lfoN`, `env1..3`, `macro1..4`, `mod_wheel`, `pitch_bend`, `velocity`, `note`.
- Implement `mod_outputs()` with the same newest-voice / global logic `meters()` uses.
- Add its arm in `builtin/mod.rs::mod_outputs`.
- Implement `modulate()` with a ramp of one control tick rather than `PARAM_RAMP_MS`.
- Knob ring API (whatever shape it lands in, Panel will adapt), ideally:
  - `Knob::rings(&[ModRing { amount: f32 /* signed, normalized */, bipolar: bool, color }])`;
  - `Knob::live(Option<f32>)`;
  - `.drop_highlight(bool)`;
  - `KnobResponse { ring_changed: Option<(usize, f32)>, ring_menu: Option<usize>, .. }`.
- Add an "FX" page that calls `crate::fx_view::show(ui, panel.session, panel.timeline, track, panel.color)`.
- Put `crate::modulation::source_handle(ui, session, owner, ModSource::Instrument("lfo1".into()))` beside each LFO, envelope and macro.

**Effects agent**
- Implement `modulate()` with ramps of at most one block for every effect, as part of the "safe under fast modulation" work.
- Their tests should include "every float parameter driven by a 20 Hz sine through `modulate` each 64-frame block: finite output, no discontinuity above a threshold".

---

## 5. Engine design

### 5.1 Node layout after Phase 1

```rust
pub(crate) struct Node {
    /* unchanged: id, output, source, fader, params, send, meters, latency, compensation, automation, processors */
    pub effects: Box<[EffectSlot]>,   // EVERY device of the track, in Track::devices order (slot == AutomationKind slot)
    pub program: ChainProgram,
    pub mods: ModProgram,             // empty when the track has no modulation
}
pub(crate) enum ChainProgram { Linear, Graph(Box<GraphProgram>) }
```

- `Linear` is today's `chain::run` and stays the fast path.
- The master gets the same treatment: `master_effects` + `master_program`.

### 5.2 Compiling a graph to a flat schedule (`engine/src/graph.rs`, UI thread)

```rust
pub(crate) struct GraphProgram {
    pub steps: Box<[Step]>,
    pub buffers: Box<[Bus]>,                               // scratch, sized here (MAX_BLOCK stereo each)
    pub splits: Box<[StateSlot<NodeId, SplitState>]>,      // crossover filter state
    pub delays: Box<[StateSlot<CableKey, Compensation>]>,  // per-cable latency compensation
    pub gains:  Box<[StateSlot<CableKey, Smoothed>]>,      // cable gain ramps (no clicks on gain edits)
    pub latency: usize,                                    // arrival at Output → node.latency
}
pub(crate) enum Step {
    LoadInput { to: Buf },
    Clear { buf: Buf },
    Mix { from: Buf, to: Buf, gain: u16, delay: Option<u16> },   // add a cable into a port buffer
    Effect { slot: u16, buf: Buf, sidechain: Sidechain },        // None | Send(usize) | Buffer(Buf)
    Split { state: u16, input: Buf, outputs: [Buf; 4], n: u8 },
    Join { kind: Join, a: Buf, b: Buf, out: Buf },
    StoreOutput { from: Buf },                                   // into the track buffer
}
/// Generic adopt-by-key state, like EffectSlot: `value: None` = take it from the old snapshot.
pub(crate) struct StateSlot<K, T> { pub key: K, pub value: Option<Box<T>> }
```

**Algorithm**
1. Validate the graph (fall back to `Linear` in device order if it's invalid).
2. Topological sort (Kahn), deterministic in node order. Nodes that can't reach Output, or aren't reachable from Input, get no steps. Their device slots still exist, so instances and tails persist.
3. **Latency.**
   - `arrive(out) = max over the node's input cables of (arrive(from) + 0) + node_latency`. `node_latency` is the device's descriptor latency, `AudioLookup::plugin_latency` for plugins, 0 when bypassed (today's rule), and 0 for splits and joins.
   - At each input port, every cable that arrives earlier than the latest one gets a `Compensation` delay of `latest − its arrival`.
   - Sidechain ports are aligned too: a node's main and sidechain inputs arrive together.
   - Delay lines are keyed by cable, so a changed delay crossfades instead of jumping.
   - Size each line to the delay it needs (rounded up, UI thread) instead of the fixed 8192. Linear-phase plugins can exceed 170 ms. Make the track-level `Compensation` sizable the same way.
4. **Buffer allocation** (liveness-based register allocation).
   - A device processes in place on its input buffer when that buffer has one consumer, one cable at unity gain and no delay. Otherwise the port gets a fresh buffer: `Clear` + one `Mix` per cable.
   - Fan-out of a buffer that's processed in place gets copied first (copy-on-write).
   - A buffer is freed after its last consumer.
   - A linear graph compiles to one buffer with zero copies.
5. **Steps index everything.** `sidechain: Buffer(b)` borrows disjointly with `<[Bus]>::get_disjoint_mut` (Rust 1.98 is installed).

The graph's `latency` replaces `chain_latency` for that node, so the existing `compensate()` keeps aligning tracks at their buses.

**Swap**
- Device instances are adopted exactly as now. `chain::take` has to search the effect slots, and the old snapshot's graph and master programs.
- `StateSlot`s are adopted by key, using a generic helper in `chain.rs`.
- New cables start at gain 0 and ramp to target over about 5 ms. Structural edits then fade in instead of clicking.

**Run (audio thread)**
- `graph::run(program, &mut effects, track_buf, &Context, sends)` walks `steps`.
- `Effect` steps call the same per-slot body `chain::run` uses: bypass/reset, sidechain context, meters and taps. Factor it out as `chain::run_slot`.
- No allocation: everything was sized at compile time.

### 5.3 Sidechains

- **Inside a graph:** cables into port 1 of a listening device, aligned by the latency pass.
- **From another track:** `Device::sidechain` and the send buses stay as they are, uncompensated and possibly a block late, as documented today.

### 5.4 Modulation (`engine/src/modulation.rs`)

```rust
pub(crate) struct ModProgram {
    pub modulators: Box<[ModulatorSlot]>,   // StateSlot<ModulatorId, Box<dyn modulators::Modulator>> + param values
    pub values: Box<[f32]>,                 // one per source output: modulator outputs, then instrument mod_outputs
    pub instrument_outputs: Range<usize>,
    pub routes: Box<[Route]>,               // { id, source: u16, scale_by: Option<u16>, amount, scale, offset, curve_k, curve_norm, bypassed }
    pub targets: Box<[Target]>,             // { sink, param: Option<ParamDescriptor>, base: f32 /*normalized*/, routes: Range<u16>, stage, last }
    pub meters: Arc<ModMeters>,             // atomics: per-target final normalized value, per-source value & LFO phase
}
pub(crate) enum Sink { Volume, Pan, Instrument(u16), Effect { slot: u16, param: u16 }, Modulator { slot: u16, param: u16 } }
pub(crate) enum Stage { BeforeSource, AfterSource }
```

**Per block, per node.** The control rate is the existing 64-frame block, and loop-cut blocks just run shorter.

1. Automation, while playing. For a modulated parameter it writes `Target.base` instead of the processor.
2. Apply targets on modulator parameters (previous block's values), then `tick` each modulator. `ModContext` carries `frames`, `bpm`, `beat`, `playing`, this block's note events, and the follower inputs measured last block.
3. `apply(BeforeSource)`: instrument-parameter targets.
4. Render the source, then `instrument.mod_outputs(&mut values[instrument_outputs])`.
5. `apply(AfterSource)`: volume, pan and effect targets, which can use this block's instrument LFO.
6. Chain or graph, then followers sample their buffers (used next block), then fader, compensation and output.

**Combining knob, automation and modulation**
- `base` = the automation value if the parameter has a lane and the transport is playing, else the project value. That's today's rule: automation replaces the knob. MIDI-controller links move the knob, which is the base.
- `final = clamp(base + Σ route.amount · curve(src·scale + offset) · aux, 0, 1)`.
  - `scale`/`offset` come from `Polarity`, exactly as the Wavetable's `mod_slot`.
  - The curve is `matrix_curve`. Move it to `dsp`, or call the public Wavetable function.
- If `final != last`, send `sink.modulate(descriptor.from_normalized(final))`. Enums round.
- The fader gets `Fader::modulate_gain/pan` with a one-block ramp.
- The base is pushed with `set_param` only when it changes.
- Unmodulated parameters keep exactly today's path.

**Live commands** (all variants added in Phase 1 so `command.rs` isn't contended later)
- `SetModAmount { owner, route, amount }`.
- `SetModulatorParam { modulator, index, value }`.
- `SetCableGain { owner, cable: CableKey, gain }`.
- `SetDeviceParam`/`SetInstrumentParam`/`SetTrackVolume` update `Target.base` when the parameter is modulated. That's a per-slot `mod_target: Box<[u16]>` lookup built at compile time.

**Modulation runs while stopped too.** LFOs keep moving when you audition notes. Tempo sync is phase-locked to `beat` while playing and free-running while stopped.

**UI readout**
- `Compiler::modulated_value(owner, &AutomationTarget) -> Option<f32>` and `Compiler::modulator_output(id) -> Option<(value, phase)>` read `ModMeters`.
- `Panel` passes these to the knob's live dot and rings.

**Built-in modulators** live in a new crate, `crates/modulators`. Its `Modulator` trait has `prepare`, `reset`, `set_param`, `tick(&ModContext) -> f32`, `meters`, and `ModulatorDescriptor { id, name, params: &'static [ParamDescriptor], outputs, bipolar, needs_notes, needs_audio }`. The built-ins:
- `canopy.lfo`: shapes plus drawn points, free or synced, retrigger on note/transport, phase, smooth, rise;
- `canopy.envelope`: ADSR on the track's notes, mono last-note;
- `canopy.follower`: attack, release, gain, listen target;
- `canopy.macro`;
- `canopy.random`: S&H and smooth;
- `canopy.midi`: velocity, note, mod wheel, pitch bend, CC from live input.

### 5.5 Instruments exporting modulators

- The compiler reads `descriptor.mod_outputs()` and reserves `values[instrument_outputs]`.
- The engine calls `Instrument::mod_outputs` after each `process`.
- `ModSource::Instrument("lfo1")` resolves by id; unknown ids are dropped at compile time.
- Plugin instruments export none, since CLAP has no API for it.

### 5.6 Swap additions and retire

After adoption, `swap()` walks the old snapshot for processors that weren't adopted (effect slots, instruments, modulators) and calls `retire()` on them, still on the audio thread, before pushing the snapshot to the garbage queue. A newly arrived plugin processor calls `start_processing` lazily on its first `process`. The old instance's stop therefore always precedes the new one's start.

---

## 6. Plugin integration boundary

### Crates

- **`crates/plugins`** (plugin-host agent): format-level hosting.
  - CLAP via `clap-sys`, VST3 via `vst3`.
  - Scanning (recommend out-of-process probing plus a cache in `~/.cache/canopy`), loading, instances, process wrappers, parameters, state, X11 editor windows, run loop.
  - No canopy dependencies.
- **`crates/hosting`** (new, integration agent C): the glue.
  - `PluginManager` (UI thread, `!Send`): instances keyed by `DeviceId` or `TrackId`.
  - `PluginEffect: effects::Effect` and `PluginInstrument: instruments::Instrument` adapters.
  - Descriptor registration (param info → leaked `EffectDescriptor` + an index↔param-id table kept in hosting).
  - Event translation.
  - It isolates canopy from churn in the `plugins` API.

### What canopy needs from the `plugins` crate

This is the list to negotiate:
- **Scan results:** `PluginInfo { format, id, name, vendor, version, path, kind: Effect|Instrument, categories, aux_input }`.
- **Main thread:**
  - `instantiate(info, host_callbacks) -> Instance`;
  - `Instance::{params() -> Vec<ParamInfo{ id: u32, name, module, min, max, default, flags }>, value_text, activate(rate, max_block) -> AudioProcessor, deactivate, latency, tail, save_state, load_state, set_offline, editor_open/close, pump(), take_events() -> Vec<InstanceEvent>}`.
  - `InstanceEvent` = `ParamGesture{id, begin}`, `ParamValue{id, value}` (VST3 performEdit), `StateDirty`, `RestartRequested`, `ParamsRescanned`, `EditorClosed`.
- **Audio thread** (`AudioProcessor: Send`, no allocation):
  - `process(inputs, aux, outputs, frames, transport, events_in /* Param{id,value,offset} | ParamMod{id,amount} | Note… | Midi… */, on_output_param)`;
  - `start_processing`/`stop_processing`;
  - `reset`.
- **Thread roles:** `clap_host_thread_check` must know the main thread (recorded at startup) and the audio thread. Export threads count as audio threads: the adapter marks a thread-local on its first `process`.
- **Run loop:** the CLAP timer and posix-fd extensions and VST3 `IRunLoop`, serviced inside `Instance::pump()`. A helper thread polls the registered fds and calls `ctx.request_repaint()`; timers set `request_repaint_after`.

### Engine hooks (Phase 1, in `arrangement.rs`)

The compiler runs on the UI thread, which is the main thread. These are additive default methods on `AudioLookup`, and the session's lookups implement them with a `RefCell<PluginManager>`:

```rust
fn plugin_effect(&self, _d: &Device, _rate: u32, _max_block: usize) -> Option<Box<dyn Effect>> { None }
fn plugin_instrument(&self, _t: &Track, _rate: u32, _max_block: usize) -> Option<Box<dyn Instrument>> { None }
fn plugin_latency(&self, _d: DeviceId) -> Option<usize> { None }
fn plugin_generation(&self, _d: DeviceId) -> u64 { 0 }   // bumped on restart → compiler builds a fresh adapter
```

- `chain::build_chain` and `Build::node` call these for `format != BuiltIn`.
- `LiveDevice` also remembers the generation.
- The engine never names the hosting crate. The learn app and engine tests are unaffected.

### Lifecycle

- **Add or load a device:** the manager instantiates it on the main thread, loads the state blob, applies `params`, registers the descriptor, then sets `needs_rebuild`. Compile then asks for an adapter: the manager activates the instance if needed and hands out a `PluginEffect` holding the `AudioProcessor`.
- **Remove a device:** the adapter is retired on the audio thread at swap (`stop_processing`) and comes back in the garbage. Once the adapter's `Arc` count drops, the manager deactivates the instance.
  - Keep detached instances in a small LRU cache (about 16), so undoing a delete reattaches the same instance. Otherwise destroy it and recreate it from the blob.
- **Restart** (latency change, rescan of all params, port change):
  1. The manager marks the instance pending and returns `None` for one compile; the slot outputs silence with a short fade.
  2. Once the old adapter is back, it deactivates, reactivates and bumps the generation.
  3. The next compile builds a new adapter.
  - Expect a few frames of dropout on that one plugin. The editor stays open because it's the same instance.
- **Tails:** process every block in v1. Sleeping (CLAP `process` status / tail) is a later optimization.
- **Bypass:** today's semantics: skip `process` and call `reset` when it comes back, which is CLAP `reset` on the audio thread.

### Parameters, automation and modulation

**Registry entries**
- CLAP: hidden and read-only params are excluded.
- Stepped/enum params with 128 steps or fewer become `Enum`, with labels from `value_to_text` captured at registration. The rest are `Float` with the plugin's own min, max and default.
- VST3 params are `Float 0..1` (normalized), or `Enum` for `stepCount`.
- `unit` is leaked from the plugin's unit string.
- The CLAP `module` path feeds automation-menu grouping.

**Automation (`set_param`)**
- CLAP: a `PARAM_VALUE` event at offset 0.
- VST3: a queue with a point at offset `frames-1` (the plugin ramps from the previous value). Later, two points for in-block ramps.

**Modulation (`modulate`)**
- CLAP params with the MODULATABLE flag get `PARAM_MOD { amount = final − base }`, where the adapter tracks the base from `set_param`. Removing the last route sends amount 0.
- Other CLAP params, and all VST3 params, get base+mod as value changes. This is destructive: the plugin GUI moves. For VST3, controller GUI updates for modulated params are throttled.

**Edits from the plugin's own GUI**
- CLAP output `PARAM_VALUE` events and gestures are pushed by the adapter into an rtrb queue to the manager.
- VST3 `performEdit` arrives on the UI thread, and the host must forward it: `Command::SetDeviceParam`.
- Either way, `session` records `params[id] = value` as a live gesture (begin on gesture start, `end_live` on gesture end, so one undo step). For CLAP it doesn't echo the value back.
- The last touched parameter becomes a "Touched: <name>" chip on the device's title bar. The chip is a modulation drop target and carries Automate / Link Controller (Section 7.3).

**Sidechain:** a plugin with an aux input port gets `descriptor.sidechain = true`, and the adapter fills the aux port from `Context::sidechain`.

### State and undo

- `PluginData.state` is refreshed:
  - on save (the session asks every instance, non-undoably, before serializing);
  - before copy/duplicate/paste/preset save of plugin devices;
  - on `StateDirty`/`mark_dirty` at the end of a gesture, as an undo step "Change <plugin>".
- Undo or redo that changes a blob makes compile call `load_state` on the main thread before sending the snapshot. The manager tracks a blob identity per instance.
- `Arc` blobs keep the 200-step history cheap unless a blob actually changes.

### Missing plugins

- `descriptor()` → `None` → the slot is `missing` and passes sound through. Instruments are silent.
- The project keeps `params` and `state` untouched (round-trip safe).
- The UI card says "<name> (<vendor>, CLAP) isn't installed — sound passes through" and offers Locate…, Replace with…, and Remove.

### Export and freeze

`Bounce` and `render_midi_clip` use a fresh compiler. With plugins:
1. The export `Lookup` asks the manager for **separate** instances (main thread, same state blob, offline render mode).
2. They process on the export thread.
3. They're destroyed on the main thread afterwards.

The UI keeps pumping main-thread callbacks during the export.

### Editor windows on Linux

- **Recommendation: always floating top-level windows. Never embed in the egui canvas.**
- The plugins crate creates an X11 window (x11rb; through XWayland on Wayland sessions), titled "<track> — <plugin>". The plugin embeds into it: CLAP X11 embed, VST3 `kPlatformTypeX11EmbedWindowID`, yabridge. Resize requests and close go through the host. Optionally the CLAP floating API can be used.
- Embedding inside the egui window is impossible on native Wayland. On X11 it would need a child XID over the wgpu surface, which conflicts with egui popups, focus and DPI.
- On Wayland the X11 window can't be made transient for canopy's Wayland surface, so offer "keep editors on top" (`_NET_WM_STATE_ABOVE`).
- Wayland-native editors (VST 3.8 preview; CLAP floating Wayland) come later.
- Windows (for the Windows-build agent) can embed an HWND child later. Keep the same API.

---

## 7. UI design

### 7.1 Rack (default) and how it relates to the graph

- `routing: None` keeps the current rack, exactly.
- Non-linear routing, v1: the rack lists devices in topological order under a banner "Patched routing — Open Patcher". Reordering in the rack is disabled there. "+ Effect" appends before Output: cables into Output are redirected through the new device.
- v2: series-parallel graphs render as Bitwig-style layer boxes (a split header, stacked lanes, then the join) in the rack itself. Anything else stays on the banner.

### 7.2 Patcher canvas (`ui/src/patcher/`, hand-painted)

Hand-painted to match the codebase. Avoid egui node-graph crates that lag egui 0.36.

**Where it lives**
- A third `Detail::Patcher` bottom view, plus a "Rack / Patcher" toggle in the device view.
- A pop-out viewport, the way analyzer windows work, for big patches.

**Nodes and cables**
- Nodes are rounded cards with a title (device name, format badge, bypass switch, small output peak).
- Input ports are on the left, with the sidechain port in a distinct color. Output ports are on the right.
- Input and Output nodes are labeled with the instrument/clips name and "Fader".
- Cables are cubic Béziers in the track's color; sidechain cables are orange. Modulation routes show as dashed purple cables from modulator nodes to device nodes; they're read-only and edited on the knobs.

**Interactions**
- Pan and zoom.
- Drag from an output port to an input port to connect. Connections that would create a cycle or use a missing port are refused, with a red preview.
- Dragging a cable's input end away detaches it (FL-style).
- Click a cable to select it; Delete removes it.
- The wheel or the midpoint handle sets gain (a live gesture plus `SetCableGain`).
- Cable right-click: Mute, Gain…, Insert Effect ▸, Delete.
- Canvas right-click: Add Effect ▸ (built-ins and plugins), Add Split ▸ (2/3 bands, L/R, M/S), Add Join ▸, Add Modulator ▸, Paste, Tidy (layered layout by topological depth).
- Device-node selection is shared with `Selection::devices`, so Delete, Ctrl+D, C, X and V reuse the `effect_rack` ops.
- Double-clicking a node shows its panel (docked under the canvas) or opens the plugin editor.
- New routing created from a linear chain is auto-laid out left to right.

### 7.3 Modulation UX (`ui/src/modulation/`, `devices/panel.rs`)

**Sources**
- The modulator column ("Mod") sits in the device view between the mixer column and the instrument:
  - a macro row (every Macro modulator as a knob);
  - LFO / envelope / follower / random cards (shape preview with playhead, a few knobs, fold);
  - "+ Mod".
- Each source has a painted drag handle (a crosshair) carrying `egui::DragAndDrop` payload `ModDrag { owner, source }`.
- The instrument's exported sources get handles inside its panel (the Wavetable agent places them), plus a "From instrument" list in the column.

**Drop to create a route**
- While dragging, every knob, choice, slider and touched-param chip on the same owner highlights.
- `Panel::knob_at` checks `response.dnd_release_payload::<ModDrag>()`. A drop creates a route with amount +0.25, unipolar per source.
- The wheel before the drop sets the depth (as in Vital).
- Mixer volume/pan sliders in `device_view.rs` are drop targets too.

**Knob display and editing**
- Each route draws an arc from the base to base+amount (bipolar routes are symmetric around the base), in the source's color.
- The live dot shows `Compiler::modulated_value`.
- Dragging a ring changes its amount live: `SetModAmount`, then `end_live` "Change Modulation".
- Right-clicking a ring offers Remove / Bypass / Polarity ▸ / Curve… / Scale by ▸.
- Hovering the knob lists its routes.

**Other views**
- Selecting a modulator highlights its targets. A collapsible "Routes" table in the column (source, target, amount, polarity, curve, bypass, like Vital's matrix) gives precise editing and reaches targets with no visible knob.
- Automation menus (`timeline/track_header.rs`, `timeline/automation.rs`) gain modulator parameters ("LFO 1: Rate").

### 7.4 Wavetable "FX" tab (Serum workflow, real devices)

- `ui/src/fx_view.rs` (agent B) provides `show(ui, session, timeline, track, color)`: the track's devices along the main signal path, as compact collapsible rows (on/off, name, its `Panel`), with "+ FX" offering a Serum-like quick list. These are the effects agent's new Hyper/Dimension, distortion, filter, frequency shifter and so on.
- They're the same track devices the rack and Patcher show, so the Wavetable's exported LFOs, dragged from the Modulation page, modulate them.
- A drag hovering the "FX" tab title for about 400 ms switches pages; the helper is `modulation::is_dragging(ctx)`.
- **Natural next step:** export Serum 2-style buses as multiple instrument outputs, i.e. extra output ports on the graph's Input node.

### 7.5 Plugins in the UI (`ui/src/plugins/`)

- "+ Effect" and the instrument picker gain Plugins ▸ submenus by vendor and format, from the scan cache. There's also a Plugin Manager window (rescan, block list, paths, yabridge hint).
- Plugin device title bars get "Edit" (opens the editor window) and the "Touched: <param>" chip.
- The generic plugin panel is `Panel` over the registered descriptor, with a search box, `module` grouping, plugin-formatted value text (`value_text`, main thread) and "pin to compact view" (Bitwig-style remote controls). It's dispatched from `devices/mod.rs` when `format != BuiltIn`.
- The missing-plugin card (Section 6).

---

## 8. Phased plan and work split

**Order:** Phase 0 → Phase 1 run sequentially, by one agent. Phases 2 and 3 run in parallel (A, B, C). Phase 4 is integration.

### Phase 0: interfaces (small, coordinated; a lead or agent A)

**Changes**
- `/home/m.polito/Personal-projects/canopy-vibe/crates/effects/src/lib.rs`: `modulate`, `retire`, `EffectRef.format`, external registry. Have the effects agent apply this, or apply it in an agreed window.
- `/home/m.polito/Personal-projects/canopy-vibe/crates/instruments/src/lib.rs`: `modulate`, `retire`, `mod_outputs`, `ModOutput`, `Format::Vst3` (alias `InstrumentFormat`), external registry, `BuiltInInstrumentDescriptor::mod_outputs()`.
- `/home/m.polito/Personal-projects/canopy-vibe/crates/instruments/src/builtin/mod.rs`: `mod_outputs(id)`.
- Then the Wavetable and effects agents implement their halves (Section 4).

**Tests**
- Defaults compile.
- Registry find, replace and leak behaviour.
- `EffectRef` without `format` deserializes as BuiltIn, and BuiltIn isn't written.
- The Wavetable's `mod_outputs` equal its meter LFO values (Wavetable agent).
- Fast-modulation tests (effects agent).

### Phase 1: foundation (one agent, sequential; everything later depends on it)

**Project**
- `ids.rs`; `track.rs`, `lib.rs` (fields).
- New `routing.rs` (types, `validate`, `normalize` skeleton, `ensure_routing`).
- New `modulation.rs` (types).
- New `plugin.rs` (`PluginData`, `Blob` with base64 serde; a hand-written encoder avoids a dependency).
- `automation.rs` (`ModulatorParam`).
- `file.rs`: repair, id max/unique for the new ids, invalid graph → `None`, prune dangling routes and listens, the format 1/2 rule.
- `devices.rs`: routes and graph nodes follow devices on remove, move, copy and insert; `CopiedDevice.routes`.
- `edit.rs`: `set_instrument` prunes instrument routes; `delete_tracks` prunes listens.
- `preset.rs`: an optional state blob.

**Engine**
- `arrangement.rs`: `Node.program`/`mods`; the `AudioLookup` plugin hooks; `ProcessorKind`/`LiveDevice` generation.
- `chain.rs`: `run_slot`, generic `StateSlot` adopt, `take` searching programs.
- `audio.rs`: render-loop hooks `mods.pre`/`post` (no-ops); the `retire()` pass in `swap`; command handlers.
- `command.rs`: every new variant (`SetModAmount`, `SetModulatorParam`, `SetCableGain`).
- New `graph.rs` (stub: compiles any routing as `Linear` in topological order).
- New `modulation.rs` (empty program).

**UI**
- Stub modules with their final entry-point signatures: `patcher/mod.rs` (A), `modulation/mod.rs` and `fx_view.rs` (B), `plugins/mod.rs` (C).
- One-line hooks in:
  - `device_view.rs` (modulator column call; instrument picker moved into `plugins::instrument_picker` or a small `instrument_picker.rs`);
  - `effect_rack.rs` (`plugins::effect_menu`, `plugins::title_bar_extras`, banner when routing is non-linear);
  - `devices/mod.rs` (plugin arm);
  - `app.rs` (`Detail::Patcher`, `plugins::pump` and `plugins::windows` each frame);
  - `main.rs` (mods).
- `session.rs`: `pub(crate)` accessors (`compiler()`, `modulated_value`, `refresh_plugin_states` placeholder) and a `plugins` field of a no-op type, so B and C write `impl Session` blocks in their own modules instead of editing `session.rs`.

**Tests**
- Today's-format fixture loads unchanged.
- New-feature round trip.
- Repair cases (cyclic graph, duplicate `NodeId`, dangling device node, route to a missing device).
- Device move/remove/copy carry routes and nodes.
- Fuzz invariant additions in `project/tests/fuzz.rs`.
- All existing engine, UI and realtime tests pass unchanged.
- A fake external `Effect` through `AudioLookup::plugin_effect` gets `retire()` exactly once, on the audio thread, under `assert_no_alloc`.

### Phase 2: engine and model (parallel)

**A — Graph** (owns `routing.rs`, `devices.rs`, `engine/src/graph.rs`, `chain.rs`, `compensation.rs`, new `dsp/src/crossover.rs`, `engine/tests/realtime_safety.rs`)
- Routing edits: `connect`, `disconnect`, `set_cable`, `add_split`, `add_join`, `add_device_node`, `insert_on_cable`, `remove_nodes`, `move_nodes` (view), and `normalize` called from every edit.
- Graph-aware legacy device edits.
- Compiler: topological sort, register allocation, latency per cable, `StateSlot` adoption, cable gain ramps, variable-size compensation.
- Master routing.
- Tests:
  - linear graph = 1 buffer, 0 copies;
  - parallel dry plus lookahead-limiter branch sums to a single impulse (latency aligned);
  - M/S split→join and L/R are null-identical;
  - 3-band split→sum is magnitude-flat within ±0.1 dB;
  - an internal sidechain ducks;
  - routing edits keep a reverb tail;
  - normalization returns `None`;
  - a graph in the realtime-safety project, including routing swaps while playing.

**B — Modulation** (owns new `crates/modulators`, `project/src/modulation.rs`, `engine/src/modulation.rs`, `engine/src/fader.rs`, new `engine/tests/modulation.rs` and `modulation_realtime.rs`)
- Modulator crate and edits: add/remove modulator, `add_mod_route`, `set_route`, `remove_route`.
- Engine evaluation with stages, base/automation/live-command handling, and `ModMeters`.
- Tests:
  - LFO shapes, sync and retrigger (unit);
  - an LFO on Utility gain produces amplitude modulation at the LFO's rate;
  - amount 0 changes nothing;
  - polarity and curve math match the Wavetable's;
  - automation base plus modulation combine;
  - `SetModAmount` works live;
  - volume tremolo and pan;
  - a route to an instrument parameter;
  - Wavetable `lfo1` drives an effect parameter;
  - LFO phase survives swaps;
  - no allocation.

**C — Plugins** (owns new `crates/hosting`, `ui/src/renders.rs` and `ui/src/export.rs` lookups, and the plugin-manager edits in `ui/src/session.rs`; it's the only Phase 2 agent touching `session.rs`)
- Adapters, manager, registry, state, events, restart, export instances. CLAP effects first, then CLAP instruments, then VST3.
- Tests: a tiny test CLAP plugin (gain + a latency param + modulation logging; built as a cdylib fixture by the plugins agent, or with clap-sys) verifying:
  - call order and threads (activate on main → start on audio before the first `process` → stop on audio at removal → deactivate/destroy on main);
  - state round trip;
  - latency feeds compensation;
  - `PARAM_MOD` only for modulatable params;
  - GUI-originated `PARAM_VALUE` becomes one undo step;
  - a missing plugin passes through and round-trips.
- Real plugins gated by an env var (`CANOPY_TEST_PLUGINS`), like `CANOPY_FLP_SAMPLES`.

C can start against a mock `plugins` trait if the host crate isn't ready.

### Phase 3: UI (parallel)

- **A:** `ui/src/patcher/*` (canvas, cables, menus, layout), `ui/src/effect_rack.rs` (banner, then series-parallel layer view). Tests: `ui_tests/patcher.rs` — add split, connect, gain drag, delete, undo, linear→rack normalization.
- **B:**
  - `ui/src/modulation/*` (column, cards, handles, routes table);
  - `ui/src/devices/panel.rs` (drop targets, rings, live values);
  - `ui/src/device_view.rs` (column, slider drop targets);
  - `ui/src/fx_view.rs`;
  - `ui/src/timeline/track_header.rs` and `automation.rs` (modulator params).
  - Tests: `ui_tests/modulation.rs` — drag LFO onto a knob creates a route, ring drag changes the amount, remove, undo; FX tab lists track devices.
- **C:** `ui/src/plugins/*` (menus, Plugin Manager, generic panel with search and the touched chip, editor buttons, missing card). Tests: `ui_tests/plugins.rs` with a fake registered descriptor.

### Phase 4: integration (mostly sequential)

- Patcher draws modulation cables (A reads B's model).
- End-to-end modulation of plugin parameters (C; works through `Effect::modulate`).
- Master modulators, if wanted.
- Perf pass: per-block cost with 100 routes and 50-node graphs.
- README "Threading rules" and "Layout", and PLAN.md "Status" (one agent).

### With only two agents

Agent 1 takes Graph + Plugins (both engine-chain heavy). Agent 2 takes Modulation + FX tab. Alternatively the plugin-host agent takes C once its crate stabilizes.

---

## 9. Risks and open questions (with recommendations)

1. **Modulation scope.** *Recommend:* track-local, including the track's instrument, effects, mixer and modulators. Master modulators come later. Cross-track (`ModSource::Track`) is a later extension that would be up to one block late. Bitwig and Reaper are effectively track-local too.
2. **Plugin descriptors.**
   - *Recommend:* the leaked `&'static` registry now. It means no refactor of dozens of call sites. Memory is bounded by plugins loaded × rescans.
   - A proper dynamic `ParamInfo` refactor can come later.
3. **Splits as graph nodes vs devices.** *Recommend:* graph nodes with edited (not automatable) parameters in v1; `AutomationTarget::NodeParam` later if crossover modulation is wanted.
4. **Feedback cables.** *Recommend:* DAG only. A "Feedback" node with at least a one-block delay can come later.
5. **Control rate.** *Recommend:* the existing 64-frame block, with one-block ramps in `modulate()`. Going to 32 means lowering `MAX_BLOCK`, at the cost of per-block overhead everywhere.
6. **Plugin GUIs.** *Recommend:* floating X11 windows (XWayland) only on Linux, with a "keep on top" option. No embedding into egui.
7. **Crash isolation.** *Recommend:* in-process hosting for v1, with out-of-process scanning. A plugin crash takes canopy down; the risk is accepted. Sandboxing à la Bitwig is a later project.
8. **Undoing plugin GUI edits.** Parameter edits are covered. Non-parameter state is captured only at `StateDirty` and gesture ends, so plugins that don't signal dirtiness won't undo. *Recommend:* accept, document, and refresh blobs on save and copy.
9. **Node positions in undo.** *Recommend:* not undoable (view data, like folds).
10. **File compatibility.** *Recommend:* write format 2 only when new features are used.
11. **Graph structure changes while playing** can click. Mitigation: new cables ramp in. *Recommend:* accept small transients beyond that, as other DAWs do.
12. **Large linear-phase latencies** exceed `MAX_DELAY`. *Recommend:* compensation lines sized at compile time.
13. **Process risks.** Concurrent edits to shared files (`session.rs`, `arrangement.rs`, the trait files, `knob.rs`), with no git history.
    - *Recommend:* an initial commit now, one worktree per agent, Phase 0/1 merged before parallel work, and the stub-hook pattern so files have exclusive owners.
    - The knob ring API depends on the Wavetable agent. B adapts in `Panel` and can draw rings with the painter in the meantime.
14. **FL import bonus.** FL projects store third-party plugin state, so the importer could load the real installed plugin plus its state instead of a stand-in. *Recommend:* later; the user decides.
    *Done 2026-10-12* (`docs/progress.md`, workstream 11).
15. **VST2 is not supported.** Windows plugins come in through yabridge's VST3/CLAP outputs.
    *Changed 2026-10-12:* the user asked for VST2, so it's hosted after all, through a
    clean-room ABI (`crates/plugins/src/vst2/`).

---

## 10. Sources

- CLAP headers: [events.h](https://github.com/free-audio/clap/blob/main/include/clap/events.h), [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h), [ext/params.h](https://github.com/free-audio/clap/blob/main/include/clap/ext/params.h), [ext/latency.h](https://github.com/free-audio/clap/blob/main/include/clap/ext/latency.h), [ext/gui.h](https://github.com/free-audio/clap/blob/main/include/clap/ext/gui.h)
- VST3: [Parameters and Automation](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/Parameters+Automation/Index.html), [IParamValueQueue](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IParamValueQueue.html), [Provide a Runloop on Linux](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/Provide+A+Runloop+On+Linux/Index.html), [VST 3 license (MIT)](https://steinbergmedia.github.io/vst3_dev_portal/pages/VST+3+Licensing/VST3+License.html), [KVR: VST 3.8 under MIT, Wayland preview](https://www.kvraudio.com/news/steinberg-moves-vst-3-sdk-to-mit-open-source-license-asio-now-gplv3-65179), [vst3 crate](https://crates.io/crates/vst3), [Micah Johnston on the vst3-rs build changes](https://micahjohnston.com/posts/vst3/)
- yabridge: [CHANGELOG](https://github.com/robbert-vdh/yabridge/blob/master/CHANGELOG.md), [architecture](https://github.com/robbert-vdh/yabridge/blob/master/docs/architecture.md), [REAPER IRunLoop thread](https://forum.cockos.com/archive/index.php/t-244777.html), [future/Wayland issue](https://github.com/robbert-vdh/yabridge/issues/459)
- Bitwig: [Unified modulation system](https://www.bitwig.com/userguide/latest/the_unified_modulation_system/), [Modulators](https://www.bitwig.com/userguide/latest/modulator/), [Containers](https://www.bitwig.com/userguide/latest/container/), [KVR on Multiband FX filters](https://www.kvraudio.com/forum/viewtopic.php?t=488574)
- FL Studio: [Patcher manual](https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/plugins/Patcher.htm)
- Ableton: [Instrument, Drum and Effect Racks (Live 12)](https://www.ableton.com/en/manual/instrument-drum-and-effect-racks/)
- Reaper: [What's new in REAPER 7](https://reapertips.com/post/reaper-7), [SOS: Parameter Modulation](https://www.soundonsound.com/techniques/parameter-modulation), [reaper.fm](https://www.reaper.fm/)
- Serum 2: [Effects guide: buses, routing, splitters](https://monosounds.studio/serum-2-effects-guide/), [KVR official Serum 2 thread](https://www.kvraudio.com/forum/viewtopic.php?t=619315&start=375), [EDMProd Serum 2 guide](https://www.edmprod.com/serum-2-guide/)
- Vital: [Modulation guide](https://davidmvogel.com/docs/Vital/UserGuide/Modulation)

---

### Critical Files for Implementation

- /home/m.polito/Personal-projects/canopy-vibe/crates/engine/src/arrangement.rs
- /home/m.polito/Personal-projects/canopy-vibe/crates/engine/src/audio.rs
- /home/m.polito/Personal-projects/canopy-vibe/crates/engine/src/chain.rs
- /home/m.polito/Personal-projects/canopy-vibe/crates/project/src/track.rs (with /home/m.polito/Personal-projects/canopy-vibe/crates/project/src/devices.rs and /home/m.polito/Personal-projects/canopy-vibe/crates/project/src/file.rs)
- /home/m.polito/Personal-projects/canopy-vibe/crates/ui/src/devices/panel.rs (with /home/m.polito/Personal-projects/canopy-vibe/crates/ui/src/effect_rack.rs; the shared traits are in /home/m.polito/Personal-projects/canopy-vibe/crates/effects/src/lib.rs and /home/m.polito/Personal-projects/canopy-vibe/crates/instruments/src/lib.rs)
---

## 11. Status

### Phases 0 and 1: done (2026-10-09)

Built and tested; the Phase 2 and 3 agents start from here. What follows is what exists, where the plan changed, and what each agent needs to know.

**Phase 0: shared interfaces**
- `instruments/src/lib.rs`:
  - `Format { BuiltIn (default), Clap, Vst3 }`, with `is_builtin(&self)` (for `skip_serializing_if`) and `label()`. `InstrumentFormat` is an alias.
  - `ModOutput { id, name, bipolar }`.
  - `Instrument::modulate` (defaults to `set_param`), `retire` and `mod_outputs(&self, &mut [f32])` (both default to nothing).
  - `BuiltInInstrumentDescriptor::mod_outputs()`, which looks up `builtin::mod_outputs(id)`. The Wavetable's arm is in.
  - `register_external(format, id, BuiltInInstrumentDescriptor) -> &'static _` and `find_external(format, id)`. `InstrumentRef::descriptor()` consults them for plugins.
  - Both registries share `Registry<T>`: leaked descriptors in an `RwLock`, keyed by format then id, so a lookup allocates nothing.
- `effects/src/lib.rs`:
  - `Effect::modulate` and `retire` (added by the effects agent, as specified).
  - `EffectRef.format`: written only for plugins; a file without it reads as `BuiltIn`.
  - `register_external` and `find_external`; `EffectRef::descriptor()` consults them. `effects::Format` re-exports the instruments one.

**Phase 1: project crate**
- Ids: `NodeId`, `ModulatorId` and `ModRouteId` come from the shared counter (`Project::new_node_id`, `new_modulator_id`, `new_mod_route_id`).
- Fields, all skipped when empty or `None`:
  - `Track::routing`, `modulators` and `modulations`;
  - `Project::master_routing`;
  - `Device::plugin`, and `plugin` on `TrackKind::Midi`.
- `routing.rs`:
  - The types as in Section 3, plus `Slope { Db12, Db24 (default), Db48 }` and `CableKey { from, to }` (`Cable::key()`).
  - `NodeKind::inputs(devices)` and `outputs()`. A device has a second (sidechain) input when its effect listens, or when this build doesn't know the effect, so a missing plugin keeps its cables.
  - `Routing::linear`, `validate(devices) -> Result<(), RoutingError>`, `topological_order()` (Kahn, ties in node order), `order_breaking_loops()`, `linear_chain()`, `splice_out(node)` and `splice_before_output(id, kind)`.
  - `Project::routing(owner)`, `ensure_routing(owner) -> &mut Routing`, `normalize_routing(owner)`, `node_positions()` and `set_node_position(node, pos)`.
- `modulation.rs`:
  - The types as in Section 3.
  - `Track::modulator(id)`, `has_source(&ModSource)`, `has_target(&AutomationTarget)` and `prune_modulation()` (crate-private).
  - `Project::modulator_folds()` and `set_modulator_folded(id, folded)`.
- `plugin.rs`: `PluginData`, and `Blob` (an `Arc<[u8]>`, serialized as base64 by a hand-written codec). Blobs compare by pointer or by bytes. A damaged blob loads as empty instead of failing the file.
- `AutomationTarget::ModulatorParam { modulator, param }`.
- `EditError` gains `NoSuchNode`, `NoSuchModulator`, `NoSuchRoute`, `UnknownModulator` and `Routing(RoutingError)`, with a `From` conversion.
- Plugin data setters: `Project::set_device_plugin(id, Option<PluginData>)` and `set_instrument_plugin(track, ...)`. `set_instrument` clears the plugin data.
- `file.rs`:
  - A project is written as format 2 only when it uses a graph, modulators, routes, plugin data, a non-built-in format, or a `ModulatorParam` lane or link (`Project::file_format()`). Anything else is written as format 1, byte for byte as before. This build reads up to 2.
  - `repair()` covers the new ids in its max and uniqueness pass.
    - A graph that fails `validate` becomes `None`, with the pool sorted by `order_breaking_loops`.
    - A valid graph gets node ids renumbered where they collide, with its cables and `Listen::Node` following, and is normalized.
    - Cable gains, positions and crossovers are clamped.
  - Dangling routes, `scale_by`s, listens, `ModulatorParam` lanes and links are pruned.
- `devices.rs`:
  - Add, insert and paste splice the new devices in before Output. Removing a device splices its node out: what fed it now feeds what it fed, gains add, and a mute on either side mutes the join.
  - Moving devices to another chain does both.
  - Routes go with their device: `CopiedDevice.routes`. A copy gets new route ids. Routes land only where their source exists on the destination, and `scale_by` is cleared otherwise. The master drops them.
  - Moving within a graph's own pool changes nothing, since the graph sets the order.
  - `prune_sidechains` became `prune_track_references`, which also clears `Listen::Track`.
- `edit.rs`: `set_instrument` drops routes to `InstrumentParam` and from `Instrument(..)` sources. `delete_tracks` and now `ungroup` prune references to removed tracks.
- `preset.rs`: `PresetFile.state: Blob`, optional.

**Phase 1: engine**
- `command.rs`:
  - `SetModAmount { owner, route, amount }`;
  - `SetModulatorParam { modulator, index, value }`;
  - `SetCableGain { owner, cable: CableKey, gain_db }`.
- `arrangement.rs`:
  - `Node.program: ChainProgram` and `Node.mods: ModProgram`; `Arrangement.master_program`; `AutomationKind::Modulator(slot, param, descriptor)`; `Arrangement::retire()`.
  - Six `AudioLookup` defaults, all called on the UI thread:
    - `plugin_effect(&Device, rate, max_block) -> Option<Box<dyn Effect>>`;
    - `plugin_instrument(&Track, rate, max_block)`;
    - `plugin_latency(DeviceId)`;
    - `plugin_instrument_latency(TrackId)`;
    - `plugin_generation(DeviceId)`;
    - `plugin_instrument_generation(TrackId)`.
  - `LiveDevice` and `ProcessorKind::Midi` remember the generation, so a bump makes a fresh instance.
  - The compiler owns `GraphState` (graph.rs) and `ModState` (modulation.rs), and calls their `begin`, `compile`, `finish` and `invalidate`. `Compiler::modulated_value(Option<TrackId>, &AutomationTarget)` and `modulator_output(ModulatorId)` delegate to `ModState`.
  - Device latency comes from the descriptor, or from `plugin_latency` for plugins.
  - `render_midi_clip` takes plugin instruments from the lookup and retires them when done.
- `chain.rs`:
  - `run_slot(slot, l, r, &Context)` and `sidechain(slot, sends, frames)`; `run` uses both.
  - `EffectSlot.silent`: a registered plugin for which `plugin_effect` returned `None`. It outputs zeros, isn't adopted, and is asked for again on the next compile.
  - `retire(chain)`, and `StateSlot<K, T>` with `new`, `kept`, `take` and `adopt`.
- `audio.rs`:
  - The render loop collects note events first. Per block it calls `mods.pre(cx, processors)`, renders the source, then `mods.post(cx, (l, r), processors, effects)`, then `program.run(...)`.
  - Automation and the live commands for volume, pan, instrument parameters and device parameters offer their value to `mods.set_base(Sink, value)` first.
  - `swap` leaves values for which `mods.modulates(sink)` is true alone, calls `program.adopt(old)` and `mods.adopt(&mut previous.mods)`, then `old.retire()` before handing the old snapshot back.
  - `Bounce` retires its engine's snapshot when dropped.
- `graph.rs` (stub): any graph compiles to `ChainProgram::Graph`, its devices in series in topological order through `run_slot`. `adopt` and `set_cable_gain` do nothing.
- `modulation.rs` (stub): an empty `ModProgram`. Every hook does nothing, `set_base` and `modulates` return false, and `automation_kind` returns `None`.

**Phase 1: UI**
- New stub modules:
  - `patcher/mod.rs`: `READY = false`, `show`, `windows` and `rack_banner` (a "Patched" note).
  - `modulation/mod.rs`: `column`, `source_handle(ui, session, owner: TrackId, ModSource) -> Response` and `is_dragging`.
  - `fx_view.rs`: `show(ui, session, timeline, track: TrackId, color)`, which lists effect names.
  - `plugins/mod.rs`: `Plugins` (the no-op session field), `pump`, `windows`, `refresh_states`, `effect_menu`, `title_bar_extras`, `panel` (generic knobs), `missing`, and `instrument_picker` (moved here from device_view).
- One-line hooks:
  - `device_view.rs`: the column, and the picker.
  - `effect_rack.rs`: the plugin menu, title-bar extras, the missing-plugin card, the banner, and refreshing plugin state before copy, cut, duplicate and copy-to.
  - `devices/mod.rs`: non-built-ins go to `plugins::panel`; `generic_panel`.
  - `devices/panel.rs`: `Panel.format`.
  - `app.rs`: `Detail::Patcher`, offered once `patcher::READY`; `plugins::pump` before `session.update`; `patcher::windows` and `plugins::windows` each frame.
  - `main.rs`: `mod` lines only.
- `session.rs`:
  - The `plugins` field, and accessors `compiler()`, `live_changed()`, `request_rebuild()`, `modulated_value`, `modulator_output` and `refresh_plugin_states()`. The last is called on save and on saving a preset.
  - `ViewState` carries node positions and modulator folds over undo.

### Where the plan changed
- `SetCableGain` carries `gain_db` (the model's unit), not a bare `gain`.
- Plugin instruments get their own generation and latency hooks, keyed by track.
- The compiler doesn't `prepare` plugin instances: `plugin_effect` must return them ready to run. Parameter values are pushed with `set_param`, as for built-ins.
- A registered plugin whose instance isn't ready plays silence, which the plan wanted. It isn't faded out yet.
- The graph stub compiles to a `Graph` program, in series and in topological order, rather than to `Linear`, so the graph path runs in tests.
- Modulation interception goes through `set_base` and `modulates` on `ModProgram` rather than a `mod_target` table the audio thread reads directly. B decides the table inside.
- Compile-time memory for graphs and modulation (`GraphState`, `ModState`) lives in their own files, so A and B never edit arrangement.rs or audio.rs.
- Repair also normalizes: a file holding a linear graph loads as `None`.

### Tests (all green)
- `instruments`: trait defaults; the registry finds, replaces and keeps old descriptors alive; the `InstrumentFormat` alias still works.
- `project`:
  - `plugin.rs`: base64 against the standard vectors, sharing and equality, round trip, damaged state.
  - `routing.rs`: validation errors, normalization, splicing, node positions.
  - `tests/routing_and_modulation.rs`:
    - a format 1 fixture (`tests/fixtures/linear.canopy`, with devices, a sidechain, automation, a group, the master chain and a link) loads and re-saves byte for byte;
    - every feature round-trips as format 2, and any one of them alone forces it;
    - newer formats are refused;
    - repair: a loop, a duplicate node id, a dangling device node, a device without a node, non-finite values, dangling routes, listens, lanes and links, and colliding ids;
    - routes and nodes follow removal, copying, moving, changing the instrument and deleting a track;
    - missing plugins keep their name, settings and state.
  - `CANOPY_PROJECTS=<dir>` checks real saved projects. The two saved by the current build re-save byte-identically.
  - `tests/fuzz.rs`: eight new ops (adding, removing, moving and pasting effects across chains, cables, modulation, changing the instrument, deleting tracks). After every step, `check_chains` verifies unique ids, valid and non-linear graphs, pools in sound order, and that nothing dangles. About 35,000 graph states and 240,000 route states are checked per run.
- `engine`:
  - `StateSlot` adoption; the graph stub's order.
  - `tests/plugins.rs`: a fake CLAP effect through `plugin_effect` is retired exactly once, on the audio thread, under `assert_no_alloc`, and dropped on the UI thread. This holds across an unrelated edit, a generation bump, a "not ready" pass (silent), and removal. The plugin's latency feeds `Compiler::latency`, and a non-linear graph plays its device.
  - `realtime_safety.rs` adds graphs on a group and the master, a graph made during playback, and the three new commands.
- `ui`: all existing tests pass unchanged, including the headless `ui_tests`.

### For the Phase 2 and 3 agents
- **Everyone:**
  - Every edit to a chain or its graph must end with `normalize_routing(owner)`. The fuzz checker enforces this: graphs valid, never linear, pool in topological order.
  - Node ids, modulator ids and route ids are unique across the whole project.
- **A (graph):**
  - Keep the signatures audio.rs calls: `ChainProgram::run(effects, l, r, &Context, sends)`, `adopt(&mut Arrangement)` (search `old.nodes[*].program` and `old.master_program` with `StateSlot::take`), and `set_cable_gain(CableKey, f32 dB)`.
  - `GraphState::compile(ChainInput { owner, devices, routing, latencies, sample_rate }) -> (ChainProgram, latency)`. `latencies` is by pool index, plugins included. `Node.latency` is instrument latency plus this.
  - Flip `patcher::READY` when the canvas exists.
- **B (modulation):**
  - `set_base` values are dB for Volume, -1..=1 for Pan, and the parameter's own units otherwise. Return true to own the parameter; you then push its base with `set_param` yourself.
  - `pre` and `post` run every block, playing or not.
  - `automation_kind` compiles `ModulatorParam` lanes. The `Sink` indices are `u16`.
  - Removing a modulator must also drop its lanes, links and routes; `prune_modulation` helps.
  - `fx_view::show` and `modulation::source_handle` are the Wavetable agent's entry points.
- **C (plugins):**
  - The six `AudioLookup` hooks.
  - Register descriptors with `effects::register_external` and `instruments::register_external`. The engine never calls a plugin descriptor's `create`.
  - `retire()` happens:
    - at swap, on the audio thread, for whatever wasn't adopted;
    - in `Bounce`'s drop, on the export thread;
    - at the end of `render_midi_clip`, on the calling thread.
  - It does **not** happen when a live engine is dropped (the stream closing), so adapters must cope with being dropped without it.
  - Replace `Session::plugins` (needs interior mutability for `&self` lookups) and fill in the `plugins` module.
  - `Device::name()` falls back to `PluginData.name`.

### Agent C (plugins): done (2026-10-12)

The plugin integration is built as planned, in `crates/hosting` and `ui/src/plugins/`, with VST2
added to the `plugins` crate as a third format. `docs/progress.md` (workstream 10) has what was
built and what's left. Where it differs from this plan:
- The manager reconciles with the project every frame (`PluginManager::pump`) instead of being
  told about each edit, so undo, paste, presets, imports and opening a project all go through
  one path. The session applies what plugins change by themselves as `hosting::Update`s.
- The `AudioLookup` hooks are implemented once, by `hosting::WithPlugins`, around the UI's own
  lookups; the session doesn't need a `RefCell` of its own (the manager has one inside).
- Exports of projects with plugins compile each job's snapshot on the UI thread, on request
  from the export thread; projects without plugins export as before.
- Enumerated parameters store their step index, everything else the plugin's own value.
- No silence fade when a plugin restarts yet (a snapshot or two of silence on that device).
