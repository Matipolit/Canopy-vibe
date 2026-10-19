//! The open project and everything that keeps the engine in step with it:
//! undo history, the audio device, the snapshot compiler and loaded audio.
//!
//! Every committed edit rebuilds the engine's snapshot. Live gestures (a
//! fader drag) change the project without rebuilding and send small commands
//! instead; the snapshot catches up when the gesture ends.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use engine::device::AudioOutput;
use engine::{Command, Compiler, EngineHandle, TrackMeters};
use hosting::WithPlugins;
use media::{Loaded, Loader, MediaError};
use midi::{NoteEventKind, Tick};
use project::{
    AutomationTarget, ClipId, ClipRef, ControllerLink, DeviceId, EditError, History, Mixer,
    NewTrack, Note, Project, SourceId, TrackId, TrackKind, TrackPosition,
};

use crate::banks::{BankState, Banks, DataKey, TableState};
use crate::export::{self, Exporting};
use crate::midi_input::{Control, MidiInputs};
use crate::renders::{ExactLookup, Lookup, Renders};
use crate::taps::Tap;

pub struct Audio {
    pub handle: EngineHandle,
    /// The sound card stream, or `None` when something else drives the
    /// engine (tests).
    pub output: Option<AudioOutput>,
}

pub enum SourceState {
    Loading,
    Ready(Arc<media::Source>),
    Failed(MediaError),
}

/// A line for the status bar: what just happened, or what went wrong.
pub struct Message {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

/// Files dropped together land one after another on the same track.
struct ImportBatch {
    /// `None` until the first file arrives, if a new track is needed.
    track: Option<TrackId>,
    cursor: Tick,
    /// Where to add a new track if one is needed.
    position: TrackPosition,
}

/// Load keys with this bit set are imports, not sources already in the project.
/// Other keys are a source id in the low 32 bits and the project generation
/// it was requested for above that, so a load from a project that has since
/// been closed can't land in the next one, whose ids start over.
const IMPORT_KEY: u64 = 1 << 63;

fn source_key(generation: u32, id: SourceId) -> u64 {
    (u64::from(generation & 0x7FFF_FFFF) << 32) | u64::from(id.0)
}

/// What a preset is saved from or loaded into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetTarget {
    Device(DeviceId),
    Instrument(TrackId),
}

impl PresetTarget {
    /// The same thing as the plugin host names it.
    pub fn plugin_owner(self) -> hosting::Owner {
        match self {
            PresetTarget::Device(id) => hosting::Owner::Effect(id),
            PresetTarget::Instrument(track) => hosting::Owner::Instrument(track),
        }
    }
}

/// How the project is being looked at and listened to, kept through undo.
struct ViewState {
    groups: Vec<(TrackId, bool)>,
    devices: Vec<(DeviceId, bool)>,
    modulators: Vec<(project::ModulatorId, bool)>,
    /// Where graph nodes sit on the Patcher's canvas.
    nodes: Vec<(project::NodeId, [f32; 2])>,
    loop_region: Option<project::LoopRegion>,
    /// Where the arrangement is scrolled and zoomed to.
    arrangement: Option<project::ArrangementView>,
}

/// How long a controller must be still before its changes become an undo step.
const CONTROLLER_STILL: std::time::Duration = std::time::Duration::from_millis(400);

/// Notes played on a MIDI keyboard while recording, with their times.
struct Recorder {
    track: TrackId,
    /// Keys down: when, and how hard.
    open: HashMap<midi::Pitch, (Tick, midi::Velocity)>,
    /// `(start, end, pitch, velocity)`, on the timeline.
    notes: Vec<(Tick, Tick, midi::Pitch, midi::Velocity)>,
}

impl Recorder {
    fn new(track: TrackId) -> Recorder {
        Recorder {
            track,
            open: HashMap::new(),
            notes: Vec::new(),
        }
    }

    fn play(&mut self, at: Tick, kind: NoteEventKind) {
        match kind {
            NoteEventKind::On { pitch, velocity } => {
                self.close(pitch, at);
                self.open.insert(pitch, (at, velocity));
            }
            NoteEventKind::Off { pitch, .. } => self.close(pitch, at),
            _ => {}
        }
    }

    fn close(&mut self, pitch: midi::Pitch, at: Tick) {
        if let Some((start, velocity)) = self.open.remove(&pitch) {
            let end = at.max(start + Tick::new(1));
            self.notes.push((start, end, pitch, velocity));
        }
    }

    /// Everything played, with keys still down ending at `now`.
    fn finish(&mut self, now: Tick) -> Vec<(Tick, Tick, midi::Pitch, midi::Velocity)> {
        let held: Vec<_> = self.open.keys().copied().collect();
        for pitch in held {
            self.close(pitch, now);
        }
        std::mem::take(&mut self.notes)
    }
}

pub struct Session {
    pub project: Project,
    history: History,
    /// `Err` if no device could be opened. Everything but playback still works.
    pub audio: Result<Audio, String>,
    /// The rate to ask the device for; it may run at another.
    pub requested_rate: u32,
    compiler: Compiler,
    meters: Arc<TrackMeters>,
    track_peaks: HashMap<TrackId, [f32; 2]>,
    sources: HashMap<SourceId, SourceState>,
    loader: Loader,
    renders: Renders,
    exporting: Option<Exporting>,
    /// Asks for a repaint, from other threads.
    wake: Arc<dyn Fn() + Send + Sync>,
    pub midi: MidiInputs,
    /// The track a MIDI keyboard plays.
    live_target: Option<TrackId>,
    recording: bool,
    metronome: bool,
    recorder: Option<Recorder>,
    /// Keys sounding from a MIDI keyboard, for the piano roll to show.
    live_keys: [bool; 128],
    /// A control waiting to be linked to the next controller that moves.
    learning: Option<(Option<TrackId>, AutomationTarget)>,
    /// When a controller last moved something. Its changes become one undo
    /// step once it's been left alone for a moment.
    controller_moved: Option<Instant>,
    imports: HashMap<u64, (u64, PathBuf)>,
    batches: HashMap<u64, ImportBatch>,
    next_import: u64,
    /// Bumped whenever the project is replaced.
    generation: u32,
    /// Imports that finished during a live gesture, applied once it ends so
    /// they don't split its undo step.
    deferred: Vec<Loaded>,
    pub path: Option<PathBuf>,
    revision: u64,
    saved_revision: u64,
    needs_rebuild: bool,
    playing: bool,
    /// The project as it was when the current live gesture began.
    live_before: Option<Project>,
    pub message: Option<Message>,
    /// The recent sound of every effect that taps it.
    taps: HashMap<DeviceId, Tap>,
    /// Where presets are kept; `None` if there's no data folder.
    pub presets_dir: Option<PathBuf>,
    /// Instruments' files (samplers' banks), loading or loaded.
    banks: Banks,
    /// The plugin host: plugin instances, their editors, what scanning
    /// found (see `plugins`).
    pub(crate) plugins: crate::plugins::Plugins,
}

impl Session {
    pub fn new(requested_rate: u32, wake: impl Fn() + Send + Sync + 'static) -> Session {
        let mut session = Session::with_audio(start_audio(requested_rate), requested_rate, wake);
        // Tests leave these off, so they never touch the user's devices or
        // files.
        session.midi = MidiInputs::new(session.wake.clone());
        session.presets_dir = crate::presets::default_dir();
        session.plugins.start(session.wake.clone());
        session.connect_live();
        session
    }

    /// Points MIDI keyboards at the current engine.
    fn connect_live(&mut self) {
        let live = self.audio.as_ref().ok().map(|a| a.handle.live_sender());
        self.midi.set_live(live);
    }

    /// Where a MIDI keyboard's notes come in, for tests to play.
    #[cfg(test)]
    pub fn live_sender(&self) -> Option<engine::LiveSender> {
        self.audio.as_ref().ok().map(|a| a.handle.live_sender())
    }

    /// A session whose engine is driven by the caller instead of a device.
    #[cfg(test)]
    pub fn with_engine(handle: EngineHandle, wake: impl Fn() + Send + Sync + 'static) -> Session {
        let rate = handle.sample_rate() as u32;
        Session::with_audio(
            Ok(Audio {
                handle,
                output: None,
            }),
            rate,
            wake,
        )
    }

    fn with_audio(
        audio: Result<Audio, String>,
        requested_rate: u32,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Session {
        let rate = engine_rate(&audio, requested_rate);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let (wake_loader, wake_renders, wake_banks) = (wake.clone(), wake.clone(), wake.clone());
        let mut session = Session {
            project: Project::with_default_tracks(),
            history: History::default(),
            audio,
            requested_rate,
            compiler: Compiler::new(rate),
            meters: engine::Arrangement::empty().meters().clone(),
            track_peaks: HashMap::new(),
            sources: HashMap::new(),
            loader: Loader::new(move || wake_loader()),
            renders: Renders::new(move || wake_renders()),
            exporting: None,
            midi: MidiInputs::off(wake.clone()),
            wake,
            live_target: None,
            recording: false,
            metronome: false,
            recorder: None,
            live_keys: [false; 128],
            learning: None,
            controller_moved: None,
            imports: HashMap::new(),
            batches: HashMap::new(),
            next_import: 0,
            generation: 0,
            deferred: Vec::new(),
            path: None,
            revision: 0,
            saved_revision: 0,
            needs_rebuild: true,
            playing: false,
            live_before: None,
            message: None,
            taps: HashMap::new(),
            presets_dir: None,
            banks: Banks::new(wake_banks),
            plugins: Default::default(),
        };
        session.sync_master();
        session
    }

    /// The rate the engine actually runs at.
    pub fn sample_rate(&self) -> u32 {
        self.compiler.sample_rate()
    }

    /// Reopens the device at `requested_rate`. The new engine starts empty,
    /// so it gets a full snapshot, and audio is converted again if the rate
    /// changed.
    pub fn restart_audio(&mut self) {
        // Close the old stream first: a raw ALSA device can only be open once.
        self.audio = Err("restarting audio".to_owned());
        self.audio = start_audio(self.requested_rate);
        self.playing = false;
        let rate = engine_rate(&self.audio, self.requested_rate);
        let rate_changed = rate != self.compiler.sample_rate();
        self.compiler = Compiler::new(rate);
        if rate_changed {
            self.sources.clear();
            self.renders.clear();
            self.request_sources();
        }
        self.needs_rebuild = true;
        self.sync_master();
        // The new engine knows nothing of the keyboard or metronome yet.
        self.connect_live();
        let target = self.live_target.take();
        self.set_live_target(target);
        self.send(Command::SetMetronome(self.metronome));
    }

    /// Call once per frame: drops old snapshots, picks up loaded audio, sends
    /// a new snapshot if anything changed, and reads the meters.
    pub fn update(&mut self) {
        if let Ok(audio) = &mut self.audio {
            audio.handle.collect_garbage();
        }
        while let Some(loaded) = self.loader.try_recv() {
            self.deferred.push(loaded);
        }
        if self.renders.receive() {
            self.needs_rebuild = true;
        }
        self.request_banks();
        if self.banks.receive() {
            self.needs_rebuild = true;
        }
        self.midi.update();
        while let Some(control) = self.midi.try_recv() {
            self.control(control);
        }
        if self
            .controller_moved
            .is_some_and(|t| t.elapsed() > CONTROLLER_STILL)
        {
            self.controller_moved = None;
            self.end_live("Controller");
        }
        self.receive_played();
        if let Some(exporting) = &mut self.exporting {
            exporting.serve(&mut self.plugins.manager);
        }
        if let Some(outcome) = self.exporting.as_ref().and_then(Exporting::poll) {
            self.exporting = None;
            match outcome {
                Ok(files) => match files.as_slice() {
                    [file] => self.info(format!("Exported {}", file.display())),
                    _ => {
                        let folder = files.first().and_then(|f| f.parent());
                        let folder = folder.map_or_else(String::new, |f| f.display().to_string());
                        self.info(format!("Exported {} files to {folder}", files.len()));
                    }
                },
                Err(err) => self.error(format!("Export failed: {err}")),
            }
        }
        if self.live_before.is_none() {
            for loaded in std::mem::take(&mut self.deferred) {
                self.finish_load(loaded);
            }
        }
        if self.needs_rebuild {
            self.rebuild();
        }
        for (id, peaks) in self.meters.take() {
            self.track_peaks.insert(id, peaks);
        }
        self.drain_taps();
    }

    /// Moves what tapping effects sent since last frame into their
    /// histories, and forgets effects that are gone.
    fn drain_taps(&mut self) {
        let mut tapped = Vec::new();
        let mut collect = |devices: &[project::Device]| {
            for device in devices {
                if device.effect.descriptor().is_some_and(|d| d.tap) {
                    tapped.push(device.id);
                }
            }
        };
        collect(&self.project.master_devices);
        self.project.visit(|track, _| collect(&track.devices));
        self.taps.retain(|id, _| tapped.contains(id));
        for id in tapped {
            let tap = self.taps.entry(id).or_default();
            self.compiler.drain_tap(id, |frame| tap.push(frame));
        }
    }

    /// The recent sound of an effect that taps it.
    pub fn tap(&self, device: DeviceId) -> Option<&Tap> {
        self.taps.get(&device)
    }

    /// What a preset target is (its effect's or instrument's id) and every
    /// setting it has now, defaults included, so a preset loads the same
    /// whatever it's loaded into.
    fn preset_values(&self, target: PresetTarget) -> Option<(&'static str, BTreeMap<String, f32>)> {
        match target {
            PresetTarget::Device(id) => {
                let device = self.project.device(id)?;
                let descriptor = device.effect.descriptor()?;
                let values = descriptor
                    .params
                    .iter()
                    .filter_map(|p| Some((p.id.to_owned(), device.param(p.id)?)))
                    .chain(
                        descriptor
                            .settings
                            .iter()
                            .filter_map(|s| Some((s.id.to_owned(), device.setting(s.id)?))),
                    )
                    .collect();
                Some((descriptor.id, values))
            }
            PresetTarget::Instrument(track) => {
                let track = self.project.track(track)?;
                let TrackKind::Midi { instrument, .. } = &track.kind else {
                    return None;
                };
                let descriptor = instrument.descriptor()?;
                let values = descriptor
                    .params
                    .iter()
                    .zip(track.instrument_values())
                    .map(|(p, value)| (p.id.to_owned(), value))
                    .collect();
                Some((descriptor.id, values))
            }
        }
    }

    /// The presets saved for what `target` is.
    pub fn presets(&self, target: PresetTarget) -> Vec<crate::presets::Preset> {
        match (&self.presets_dir, self.preset_values(target)) {
            (Some(dir), Some((device, _))) => crate::presets::list(dir, device),
            _ => Vec::new(),
        }
    }

    /// Saves `target`'s settings as preset `name`.
    pub fn save_preset(&mut self, target: PresetTarget, name: &str) {
        self.refresh_plugin_states();
        let Some(dir) = self.presets_dir.clone() else {
            self.error("Couldn't save the preset: no folder for presets");
            return;
        };
        let Some((device, values)) = self.preset_values(target) else {
            return;
        };
        let state = crate::plugins::plugin_data(&self.project, target.plugin_owner())
            .map(|p| p.state.clone())
            .unwrap_or_default();
        match crate::presets::save(&dir, device, name, &values, &state) {
            Ok(_) => self.info(format!("Saved preset {}", name.trim())),
            Err(err) => self.error(format!("Couldn't save the preset: {err}")),
        }
    }

    /// Loads a preset into `target`, as one undo step.
    pub fn load_preset(&mut self, target: PresetTarget, preset: &crate::presets::Preset) {
        let Some((device, _)) = self.preset_values(target) else {
            return;
        };
        let (values, state) = match crate::presets::load(&preset.path, device) {
            Ok(loaded) => loaded,
            Err(err) => {
                self.error(format!("Couldn't load {}: {err}", preset.name));
                return;
            }
        };
        if state.is_empty() {
            self.reset_to(target, values, "Load Preset");
            return;
        }
        // A plugin's preset: its own state too, which the plugin host
        // loads into it before the values arrive.
        let owner = target.plugin_owner();
        let Some(data) = crate::plugins::plugin_data(&self.project, owner).cloned() else {
            self.reset_to(target, values, "Load Preset");
            return;
        };
        let data = project::PluginData { state, ..data };
        self.edit("Load Preset", |p| match target {
            PresetTarget::Device(id) => {
                p.set_device_plugin(id, Some(data))?;
                p.set_device_params(id, values)
            }
            PresetTarget::Instrument(track) => {
                p.set_instrument_plugin(track, Some(data))?;
                p.set_instrument_params(track, values)
            }
        });
    }

    /// The presets that come with canopy for what `target` is.
    pub fn factory_presets(
        &self,
        target: PresetTarget,
    ) -> Vec<(&'static str, BTreeMap<String, f32>)> {
        self.preset_values(target)
            .map_or_else(Vec::new, |(device, _)| crate::presets::factory(device))
    }

    /// Loads a factory preset's settings into `target`, as one undo step.
    pub fn load_factory_preset(&mut self, target: PresetTarget, values: BTreeMap<String, f32>) {
        self.reset_to(target, values, "Load Preset");
    }

    /// Puts every setting of `target` back to its default, as one undo step.
    pub fn reset_device(&mut self, target: PresetTarget) {
        self.reset_to(target, BTreeMap::new(), "Default Settings");
    }

    fn reset_to(&mut self, target: PresetTarget, values: BTreeMap<String, f32>, label: &str) {
        match target {
            PresetTarget::Device(id) => {
                self.edit(label, |p| p.set_device_params(id, values));
            }
            PresetTarget::Instrument(track) => {
                self.edit(label, |p| p.set_instrument_params(track, values));
            }
        }
    }

    /// Loads every file an instrument plays from that isn't loaded yet,
    /// and lets go of the ones no track uses.
    fn request_banks(&mut self) {
        let mut used = Vec::new();
        self.project.visit(|track, _| {
            if let Some(key) = DataKey::of(track)
                && !used.contains(&key)
            {
                used.push(key);
            }
        });
        for key in &used {
            self.banks.request(key);
        }
        self.banks.retain(&used);
    }

    /// How a track's instrument file is doing: loading, loaded, or failed.
    pub fn bank(&self, track: TrackId) -> Option<&BankState> {
        let path = self.project.track(track)?.instrument_file()?;
        self.banks.get(path)
    }

    /// Points a track's instrument at a file (an .sfz) and loads it.
    pub fn set_instrument_file(&mut self, track: TrackId, path: PathBuf) {
        self.edit("Load Instrument", |p| {
            p.set_instrument_file(track, Some(path))
        });
    }

    /// How the user wavetable of oscillator `osc` (0 = A, 1 = B, 2 = C) of
    /// a Wavetable track is doing: loading, loaded, or failed; `None` if
    /// it has none (or the track doesn't play the Wavetable).
    pub fn wavetable(&self, track: TrackId, osc: usize) -> Option<&TableState> {
        let path = self.project.track(track)?.wavetable_file(osc)?;
        self.banks.table(path)
    }

    /// Loads a WAV file as oscillator `osc`'s user wavetable on a
    /// Wavetable track, and switches the oscillator's table to "User": one
    /// undo step. Picking the file it already has reads it again.
    pub fn load_wavetable(&mut self, track: TrackId, osc: usize, path: PathBuf) {
        use instruments::builtin::wavetable as wt;
        if osc >= wt::OSC_COUNT {
            return;
        }
        let current = self
            .project
            .track(track)
            .and_then(|t| t.wavetable_file(osc));
        if current == Some(path.as_path()) {
            self.banks.reload_table(&path);
        }
        let table = wt::PARAMS[wt::osc_param(osc, wt::OSC_TABLE)].id;
        self.edit("Load Wavetable", |p| {
            p.set_wavetable_file(track, osc, Some(path))?;
            p.set_instrument_param(track, table, wt::USER_TABLE as f32)
        });
    }

    /// Lets go of oscillator `osc`'s user wavetable, as one undo step. Set
    /// to "User", the oscillator then plays the first built-in table.
    pub fn remove_wavetable(&mut self, track: TrackId, osc: usize) {
        self.edit("Remove Wavetable", |p| {
            p.set_wavetable_file(track, osc, None)
        });
    }

    /// Plays an .sfz instrument: on `onto` if it's a MIDI track (switching
    /// it to the sampler), or on a new track named after the file at
    /// `position`. One undo step. Returns the track.
    pub fn load_sfz(
        &mut self,
        path: PathBuf,
        onto: Option<TrackId>,
        position: TrackPosition,
    ) -> Option<TrackId> {
        let sampler = instruments::InstrumentRef::builtin("canopy.sampler");
        let onto = onto.filter(|t| {
            matches!(
                self.project.track(*t).map(|t| &t.kind),
                Some(TrackKind::Midi { .. })
            )
        });
        let name = path.file_stem().map_or_else(
            || "Sampler".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        self.edit("Load SFZ", |p| {
            let track = match onto {
                Some(track) => track,
                None => {
                    let track = p.add_track(NewTrack::Midi, position)?;
                    p.rename_track(track, &name)?;
                    track
                }
            };
            let already = matches!(
                p.track(track).map(|t| &t.kind),
                Some(TrackKind::Midi { instrument, .. }) if *instrument == sampler
            );
            if !already {
                p.set_instrument(track, sampler)?;
            }
            p.set_instrument_file(track, Some(path))?;
            Ok(track)
        })
    }

    /// Loads a track's instrument file again, picking up changes to it.
    pub fn reload_bank(&mut self, track: TrackId) {
        if let Some(path) = self
            .project
            .track(track)
            .and_then(|t| t.instrument_file())
            .map(Path::to_owned)
        {
            self.banks.reload(&path);
        }
    }

    /// Sends the snapshot now if an edit is waiting, instead of next frame.
    pub fn flush(&mut self) {
        if self.needs_rebuild {
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        let rate = self.sample_rate();
        self.renders.update(&self.project, &self.sources, rate);
        let lookup = WithPlugins {
            audio: Lookup {
                sources: &self.sources,
                renders: &self.renders,
                banks: &self.banks,
            },
            plugins: &self.plugins.manager,
        };
        let arrangement = self.compiler.compile(&self.project, lookup);
        let meters = arrangement.meters().clone();
        let Ok(audio) = &mut self.audio else {
            self.needs_rebuild = false;
            return;
        };
        match audio.handle.send(Command::Arrangement(arrangement)) {
            Ok(()) => {
                self.meters = meters;
                self.needs_rebuild = false;
            }
            Err(_) => {
                // Audio has stalled. Try again next frame, with every track's
                // processors, since the engine never got the ones just built.
                self.compiler.invalidate();
            }
        }
    }

    /// Sends a live command. Dropped if there's no audio.
    pub fn send(&mut self, command: Command) {
        if let Ok(audio) = &mut self.audio
            && let Err(command) = audio.handle.send(command)
        {
            eprintln!("engine command queue full, dropped {command:?}");
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_playing(&mut self, playing: bool) {
        if !playing {
            // Take what was played before the engine stops reporting it.
            self.receive_played();
            self.finish_recording();
        }
        self.playing = playing;
        self.send(Command::SetPlaying(playing));
    }

    pub fn toggle_playing(&mut self) {
        self.set_playing(!self.playing);
    }

    pub fn seek(&mut self, tick: Tick) {
        let sample = self
            .project
            .tempo
            .tick_to_sample(tick.max(Tick::ZERO), self.sample_rate());
        self.send(Command::Seek(sample as u64));
    }

    /// The playhead in ticks, fractional for smooth drawing.
    pub fn playhead(&self) -> f64 {
        let samples = self
            .audio
            .as_ref()
            .map_or(0, |a| a.handle.playhead_samples());
        self.project
            .tempo
            .frames_to_ticks(self.heard(samples) as f64, self.sample_rate())
    }

    pub fn playhead_seconds(&self) -> f64 {
        let samples = self
            .audio
            .as_ref()
            .map_or(0, |a| a.handle.playhead_samples());
        self.heard(samples) as f64 / f64::from(self.sample_rate())
    }

    /// Where the song is as it's heard, `samples` being where the engine
    /// is: while playing, effects that delay the mix (lookahead) make what's
    /// heard lag behind, and the playhead and recordings follow the ear.
    fn heard(&self, samples: u64) -> u64 {
        if self.playing {
            samples.saturating_sub(self.compiler.latency() as u64)
        } else {
            samples
        }
    }

    pub fn take_master_peaks(&self) -> [f32; 2] {
        self.audio
            .as_ref()
            .map_or([0.0; 2], |a| a.handle.take_peaks())
    }

    /// How busy the audio thread was at its busiest since the last call.
    pub fn take_load(&self) -> f32 {
        self.audio.as_ref().map_or(0.0, |a| a.handle.take_load())
    }

    pub fn track_peaks(&self, id: TrackId) -> [f32; 2] {
        self.track_peaks.get(&id).copied().unwrap_or([0.0; 2])
    }

    fn changed(&mut self) {
        self.revision += 1;
        self.needs_rebuild = true;
    }

    /// Sets or clears the loop region. Like folding, it's how you're
    /// listening rather than an edit, so it's not an undo step.
    pub fn set_loop(&mut self, region: Option<project::LoopRegion>) {
        if self.project.set_loop(region).is_ok() {
            self.changed();
        }
    }

    /// Loops `start..end` and starts the playhead inside it (see
    /// [`Session::enter_loop`]).
    pub fn loop_span(&mut self, start: Tick, end: Tick) {
        self.set_loop(Some(project::LoopRegion {
            start,
            end,
            on: true,
        }));
        self.enter_loop();
    }

    /// While looping, moves a playhead that's outside the loop to its
    /// start, so what plays next is the loop. For when the loop is set to
    /// be heard (the piano roll's Loop), not for every change to it: a
    /// playhead before the loop otherwise runs up to it.
    pub fn enter_loop(&mut self) {
        let Some(region) = self.project.loop_region.filter(|r| r.on) else {
            return;
        };
        let at = self.playhead();
        if at < region.start.get() as f64 || at >= region.end.get() as f64 {
            self.seek(region.start);
        }
    }

    /// Turns looping on or off; turning it on with no region loops the four
    /// bars from the one the playhead is in.
    pub fn toggle_loop(&mut self) {
        let region = match self.project.loop_region {
            Some(region) => project::LoopRegion {
                on: !region.on,
                ..region
            },
            None => {
                let bar = self.project.tempo.bar_len();
                let at = (self.playhead() as i64).max(0) / bar.get().max(1);
                let start = bar * at;
                project::LoopRegion {
                    start,
                    end: start + bar * 4,
                    on: true,
                }
            }
        };
        self.set_loop(Some(region));
    }

    /// Applies one undoable edit. Failures are reported in the status bar.
    pub fn edit<T>(
        &mut self,
        label: &str,
        edit: impl FnOnce(&mut Project) -> Result<T, EditError>,
    ) -> Option<T> {
        self.end_live(label);
        let generation = self.history.generation();
        match self.history.edit(&mut self.project, label, edit) {
            Ok(value) => {
                if self.history.generation() != generation {
                    self.changed();
                }
                Some(value)
            }
            Err(err) => {
                self.error(format!("{label}: {err}"));
                None
            }
        }
    }

    /// Starts a gesture that changes the project continuously (a drag). Until
    /// [`Self::end_live`], changes go straight into the project with no undo
    /// steps; the gesture as a whole becomes one.
    pub fn begin_live(&mut self) {
        if self.live_before.is_none() {
            self.live_before = Some(self.project.clone());
        }
    }

    /// Ends the current gesture, if any, as one undo step named `label`.
    pub fn end_live(&mut self, label: &str) {
        if let Some(mut before) = self.live_before.take() {
            // Scrolling meanwhile (the view following the playhead, or a
            // drag scrolling at the edge) isn't part of the gesture.
            before.arrangement_view = self.project.arrangement_view;
            if before != self.project {
                self.history.record(label, before);
                self.changed();
            }
        }
    }

    /// Changes the project during a live gesture and rebuilds the snapshot,
    /// for changes that move things in time (tempo).
    pub fn live_edit(&mut self, edit: impl FnOnce(&mut Project) -> Result<(), EditError>) {
        self.begin_live();
        if edit(&mut self.project).is_ok() {
            self.changed();
        }
    }

    /// Abandons the current live gesture, putting the project back as it was
    /// when it began (Escape during a drag).
    pub fn cancel_live(&mut self) {
        if let Some(before) = self.live_before.take() {
            self.project = before;
            self.changed();
        }
    }

    /// Renders a MIDI clip to a WAV file and puts it on an audio track right
    /// below its own (reusing one an earlier freeze made), with the same
    /// color, fader and fader automation, then mutes the MIDI clip. One undo
    /// step. Returns the new audio clip.
    pub fn freeze_clip(&mut self, clip: ClipId) -> Option<ClipId> {
        self.end_live("Edit");
        self.refresh_plugin_states();
        // A plugin instrument renders with an instance of its own, so the
        // one playing carries on.
        let instrument = self
            .project
            .clip(clip)
            .map(|(track, _)| hosting::Owner::Instrument(track));
        let offline = self
            .plugins
            .manager
            .offline_for(&self.project, |owner| Some(owner) == instrument);
        let lookup = WithPlugins {
            audio: Lookup {
                sources: &self.sources,
                renders: &self.renders,
                banks: &self.banks,
            },
            plugins: &offline,
        };
        let rendered = match self.compiler.render_midi_clip(&self.project, clip, lookup) {
            Ok(rendered) => rendered,
            Err(err) => {
                self.error(format!("Couldn't freeze: {err}"));
                return None;
            }
        };
        let (track_id, _) = self.project.clip(clip)?;
        let track = self.project.track(track_id)?.clone();
        let Some(path) = self.bounce_path(&track.name, "Frozen") else {
            self.error("Couldn't freeze: no folder to write the audio to");
            return None;
        };
        if let Err(err) = media::write_wav(&path, &rendered.audio) {
            self.error(format!("Couldn't freeze: {}: {err}", path.display()));
            return None;
        }
        let audio = Arc::new(rendered.audio);
        let (rate, frames) = (audio.sample_rate(), audio.frames() as u64);
        let loaded = media::Source {
            native_rate: rate,
            native_frames: frames,
            channels: 2,
            peaks: media::Peaks::new(&audio),
            audio,
        };
        let frozen_name = format!("{} (frozen)", track.name);
        let placed = self.edit("Freeze to Audio", |p| {
            let below = TrackPosition::after(p, track_id);
            let reuse = below.before.filter(|id| {
                p.track(*id).is_some_and(|t| {
                    t.name == frozen_name && matches!(t.kind, TrackKind::Audio { .. })
                })
            });
            let target = match reuse {
                Some(id) => id,
                None => {
                    let id = p.add_track(NewTrack::Audio, below)?;
                    p.rename_track(id, &frozen_name)?;
                    p.set_track_color(id, track.color)?;
                    let mixer = Mixer {
                        mute: false,
                        solo: false,
                        ..track.mixer
                    };
                    p.set_mixer(id, mixer)?;
                    // The render is before the effects, so the frozen
                    // track gets copies of them.
                    let mut copies = Vec::new();
                    for device in &track.devices {
                        let copy = p.add_device(Some(id), device.effect.clone(), None)?;
                        for (param, value) in &device.params {
                            p.set_device_param(copy, param, *value)?;
                        }
                        p.set_device_plugin(copy, device.plugin.clone())?;
                        p.set_device_bypassed(copy, device.bypassed)?;
                        copies.push((device.id, copy));
                    }
                    for lane in &track.automation {
                        let target = match &lane.target {
                            AutomationTarget::Volume | AutomationTarget::Pan => lane.target.clone(),
                            AutomationTarget::DeviceParam { device, param } => {
                                let Some((_, copy)) = copies.iter().find(|(old, _)| old == device)
                                else {
                                    continue;
                                };
                                AutomationTarget::DeviceParam {
                                    device: *copy,
                                    param: param.clone(),
                                }
                            }
                            // The frozen track has no instrument or
                            // modulators.
                            AutomationTarget::InstrumentParam(_)
                            | AutomationTarget::ModulatorParam { .. } => continue,
                        };
                        p.set_automation(id, target, lane.points.clone())?;
                    }
                    id
                }
            };
            let source = p.add_source(path.clone(), rate, frames, 2);
            let new_clip = p.add_audio_clip(target, rendered.start, source)?;
            p.set_clips_muted(&[clip], true)?;
            Ok((source, new_clip))
        });
        let (source, new_clip) = placed?;
        self.sources
            .insert(source, SourceState::Ready(Arc::new(loaded)));
        self.needs_rebuild = true;
        self.info(format!("Froze to {}", path.display()));
        Some(new_clip)
    }

    /// A new file for audio canopy makes (frozen, consolidated) in `folder`:
    /// next to the project if it's saved, in canopy's data folder if not.
    fn bounce_path(&self, track_name: &str, folder: &str) -> Option<PathBuf> {
        let dir = match &self.path {
            Some(project) => {
                let stem = project.file_stem()?.to_string_lossy();
                project
                    .parent()?
                    .join(format!("{stem} Samples"))
                    .join(folder)
            }
            None => eframe::storage_dir("canopy")?.join(folder),
        };
        let name: String = track_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || " -_()".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        (1..)
            .map(|n| dir.join(format!("{name} {n}.wav")))
            .find(|p| !p.exists())
    }

    /// Glues audio clips from one track, and the gaps between them, into a
    /// single new clip on a new file, as they sound (gain, fades, stretch),
    /// replacing them. One undo step. Returns the new clip.
    pub fn consolidate_clips(&mut self, ids: &[ClipId]) -> Option<ClipId> {
        self.end_live("Edit");
        let rate = self.sample_rate();
        let lookup = ExactLookup {
            project: &self.project,
            sources: &self.sources,
            renders: &self.renders,
            rate,
        };
        let rendered = match self.compiler.render_audio_clips(&self.project, ids, lookup) {
            Ok(rendered) => rendered,
            Err(err) => {
                self.error(format!("Couldn't consolidate: {err}"));
                return None;
            }
        };
        let (track_id, _) = self.project.clip(*ids.first()?)?;
        let track_name = self.project.track(track_id)?.name.clone();
        let Some(path) = self.bounce_path(&track_name, "Consolidated") else {
            self.error("Couldn't consolidate: no folder to write the audio to");
            return None;
        };
        if let Err(err) = media::write_wav(&path, &rendered.audio) {
            self.error(format!("Couldn't consolidate: {}: {err}", path.display()));
            return None;
        }
        let audio = Arc::new(rendered.audio);
        let (frames, loaded) = (
            audio.frames() as u64,
            Arc::new(media::Source {
                native_rate: rate,
                native_frames: audio.frames() as u64,
                channels: 2,
                peaks: media::Peaks::new(&audio),
                audio,
            }),
        );
        let (source, clip) = self.edit("Consolidate", |p| {
            p.delete_clips(ids)?;
            let source = p.add_source(path.clone(), rate, frames, 2);
            Ok((source, p.add_audio_clip(track_id, rendered.start, source)?))
        })?;
        self.sources.insert(source, SourceState::Ready(loaded));
        self.needs_rebuild = true;
        Some(clip)
    }

    /// Sets each audio clip's gain so its loudest sample hits 0 dB. One undo
    /// step. Clips whose audio isn't loaded, or is silent, are left alone.
    pub fn normalize_clips(&mut self, ids: &[ClipId]) {
        let rate = self.sample_rate();
        let mut gains = Vec::new();
        for id in ids {
            let Some((_, ClipRef::Audio(clip))) = self.project.clip(*id) else {
                continue;
            };
            let (Some(file), Some(SourceState::Ready(source))) = (
                self.project.source(clip.source),
                self.sources.get(&clip.source),
            ) else {
                continue;
            };
            let ratio = rate as f64 / file.sample_rate.max(1) as f64;
            let frames = |n: u64| (n as f64 * ratio).round() as u64;
            let peak = media::peak_level(
                &source.audio,
                frames(clip.source_offset),
                frames(clip.source_len),
            );
            if peak > 1e-6 {
                gains.push((*id, -20.0 * peak.log10()));
            }
        }
        if gains.is_empty() {
            self.error("Nothing to normalize: the audio is silent or still loading");
            return;
        }
        self.edit("Normalize", |p| {
            for (id, gain) in gains {
                p.set_clip_gain(id, gain)?;
            }
            Ok(())
        });
    }

    /// Splits an audio clip at every transient (drum hit, note onset) in what
    /// it plays. `sensitivity` is 0..=1; higher finds more. One undo step.
    pub fn chop_at_transients(&mut self, id: ClipId, sensitivity: f32) -> Option<Vec<ClipId>> {
        let rate = self.sample_rate();
        let Some((_, ClipRef::Audio(clip))) = self.project.clip(id) else {
            return None;
        };
        let (Some(file), Some(SourceState::Ready(source))) = (
            self.project.source(clip.source),
            self.sources.get(&clip.source),
        ) else {
            self.error("Can't chop: the audio is still loading");
            return None;
        };
        let ratio = rate as f64 / file.sample_rate.max(1) as f64;
        let start = (clip.source_offset as f64 * ratio).round() as u64;
        let end = ((clip.source_offset + clip.source_len) as f64 * ratio).round() as u64;
        let len = clip.source_len.max(1) as f64;
        let mut cuts = Vec::new();
        for hit in media::detect_transients(&source.audio, start, end, sensitivity) {
            // Into the region at the file's own rate, then the way it plays.
            let mut x = hit as f64 / ratio - clip.source_offset as f64;
            if clip.reversed {
                x = len - x;
            }
            if clip.looped {
                let mut material = (x - clip.loop_phase as f64).rem_euclid(len);
                while material < clip.played() as f64 {
                    cuts.push(material);
                    material += len;
                }
            } else {
                cuts.push(x);
            }
        }
        let at: Vec<Tick> = cuts
            .into_iter()
            .map(|m| clip.start + Tick::new(self.project.audio_ticks(clip, m).round() as i64))
            .collect();
        if at.is_empty() {
            self.info("No transients found");
            return None;
        }
        self.edit("Chop", |p| p.chop_clip(id, &at))
    }

    /// Keeps how the arrangement is scrolled and zoomed with the project,
    /// to be saved with it and restored on opening. Like a fold it's not an
    /// undo step (undo and redo carry it over); unlike one, it doesn't mark
    /// the project as changed, as looking around isn't worth asking to save
    /// for.
    pub fn keep_arrangement_view(&mut self, view: project::ArrangementView) {
        if self.project.arrangement_view != Some(view) {
            let _ = self.project.set_arrangement_view(Some(view));
        }
    }

    /// A change that matters for the file but not for undo or sound, like
    /// folding a group.
    pub fn view_edit(&mut self, edit: impl FnOnce(&mut Project) -> Result<(), EditError>) {
        if edit(&mut self.project).is_ok() {
            self.revision += 1;
        }
    }

    /// A fader drag: updates the project and tells the engine directly.
    pub fn live_mixer(&mut self, track: Option<TrackId>, mixer: Mixer) {
        self.begin_live();
        match track {
            Some(track) => {
                let _ = self.project.set_mixer(track, mixer);
                self.send(Command::SetTrackVolume {
                    track,
                    db: mixer.volume_db,
                });
                self.send(Command::SetTrackPan {
                    track,
                    pan: mixer.pan,
                });
                self.send(Command::SetTrackMute {
                    track,
                    mute: mixer.mute,
                });
            }
            None => {
                self.project.master = mixer;
                self.send(Command::SetMasterVolume(mixer.volume_db));
            }
        }
        self.revision += 1;
    }

    /// A knob drag on an instrument parameter.
    pub fn live_param(&mut self, track: TrackId, index: usize, id: &str, value: f32) {
        self.begin_live();
        if self.project.set_instrument_param(track, id, value).is_ok() {
            self.send(Command::SetInstrumentParam {
                track,
                index,
                value,
            });
            self.revision += 1;
        }
    }

    /// A knob drag on an effect parameter.
    pub fn live_device_param(&mut self, device: DeviceId, index: usize, id: &str, value: f32) {
        self.begin_live();
        if self.project.set_device_param(device, id, value).is_ok() {
            self.send(Command::SetDeviceParam {
                device,
                index,
                value,
            });
            self.revision += 1;
        }
    }

    /// What compiles the project for the engine, and reads back what the
    /// engine publishes (meters, modulation).
    #[allow(dead_code)] // for the modulation and plugin modules
    pub(crate) fn compiler(&self) -> &Compiler {
        &self.compiler
    }

    /// Marks the project changed by a live gesture, which needs saving but
    /// no new snapshot (the gesture's commands moved the engine).
    pub(crate) fn live_changed(&mut self) {
        self.revision += 1;
    }

    /// Sends a new snapshot next frame, for changes the project doesn't
    /// show (a plugin restarted, or loaded its state).
    pub(crate) fn request_rebuild(&mut self) {
        self.needs_rebuild = true;
    }

    /// What wakes the UI from other threads (a scan finishing, say).
    pub(crate) fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.wake.clone()
    }

    /// Where `target` on `track` is right now, modulation and all, on the
    /// `0..=1` scale: for a knob's live dot. `None` if it isn't modulated.
    #[allow(dead_code)] // for the modulation module and panels
    pub(crate) fn modulated_value(
        &self,
        track: Option<TrackId>,
        target: &AutomationTarget,
    ) -> Option<f32> {
        self.compiler.modulated_value(track, target)
    }

    /// A modulator's latest output and, if it has one, its phase.
    #[allow(dead_code)] // for the modulation module
    pub(crate) fn modulator_output(&self, id: project::ModulatorId) -> Option<(f32, f32)> {
        self.compiler.modulator_output(id)
    }

    /// Puts every plugin's current state into the project, before it's
    /// saved or copied. Not an undo step.
    pub(crate) fn refresh_plugin_states(&mut self) {
        crate::plugins::refresh_states(self);
    }

    /// An effect's meters (a compressor's gain reduction and levels), as its
    /// descriptor lists them.
    pub fn device_meters(&self, device: DeviceId) -> [f32; effects::MAX_METERS] {
        self.compiler.device_meters(device)
    }

    /// A track's instrument meters, as the instrument documents them.
    pub fn instrument_meters(&self, track: TrackId) -> [f32; effects::MAX_METERS] {
        self.compiler.instrument_meters(track)
    }

    fn sync_master(&mut self) {
        let db = self.project.master.volume_db;
        self.send(Command::SetMasterVolume(db));
    }

    pub fn preview_note(&mut self, track: TrackId, event: NoteEventKind) {
        self.send(Command::PreviewNote { track, event });
    }

    /// What's folded (groups, effects), the loop region and where the
    /// arrangement is scrolled to. They aren't undo steps, so undo and redo
    /// carry them over instead of restoring old ones.
    fn fold_state(&self) -> ViewState {
        let mut groups = Vec::new();
        self.project.visit(|t, _| {
            if t.is_group() {
                groups.push((t.id, t.is_folded()));
            }
        });
        ViewState {
            groups,
            devices: self.project.device_folds(),
            modulators: self.project.modulator_folds(),
            nodes: self.project.node_positions(),
            loop_region: self.project.loop_region,
            arrangement: self.project.arrangement_view,
        }
    }

    fn restore_folds(&mut self, state: ViewState) {
        for (id, folded) in state.groups {
            let _ = self.project.set_folded(id, folded);
        }
        for (id, folded) in state.devices {
            let _ = self.project.set_device_folded(id, folded);
        }
        for (id, folded) in state.modulators {
            let _ = self.project.set_modulator_folded(id, folded);
        }
        for (id, pos) in state.nodes {
            let _ = self.project.set_node_position(id, pos);
        }
        let _ = self.project.set_loop(state.loop_region);
        let _ = self.project.set_arrangement_view(state.arrangement);
    }

    pub fn undo(&mut self) {
        self.end_live("Edit");
        let folds = self.fold_state();
        match self.history.undo(&mut self.project) {
            Some(label) => {
                self.restore_folds(folds);
                self.changed();
                self.sync_master();
                self.info(format!("Undo {label}"));
            }
            None => self.info("Nothing to undo"),
        }
    }

    pub fn redo(&mut self) {
        self.end_live("Edit");
        let folds = self.fold_state();
        match self.history.redo(&mut self.project) {
            Some(label) => {
                self.restore_folds(folds);
                self.changed();
                self.sync_master();
                self.info(format!("Redo {label}"));
            }
            None => self.info("Nothing to redo"),
        }
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.history.undo_label()
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.history.redo_label()
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn info(&mut self, text: impl Into<String>) {
        let text = text.into();
        if crate::app::debug_drops() {
            eprintln!("canopy: {text}");
        }
        self.message = Some(Message {
            text,
            error: false,
            at: Instant::now(),
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        eprintln!("{text}");
        self.message = Some(Message {
            text,
            error: true,
            at: Instant::now(),
        });
    }

    // Audio files.

    // MIDI keyboards and controllers.

    /// Sends what a MIDI keyboard plays to `track` (a MIDI track), or nowhere.
    pub fn set_live_target(&mut self, track: Option<TrackId>) {
        if track != self.live_target {
            self.live_target = track;
            self.send(Command::SetLiveTarget(track));
        }
    }

    pub fn live_target(&self) -> Option<TrackId> {
        self.live_target
    }

    /// Keys a MIDI keyboard is holding (or the pedal is), by pitch.
    pub fn live_keys(&self) -> &[bool; 128] {
        &self.live_keys
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// While on and playing, what a MIDI keyboard plays is recorded into the
    /// track it plays.
    pub fn set_recording(&mut self, on: bool) {
        if !on {
            self.receive_played();
            self.finish_recording();
        }
        self.recording = on;
    }

    pub fn metronome(&self) -> bool {
        self.metronome
    }

    pub fn set_metronome(&mut self, on: bool) {
        self.metronome = on;
        self.send(Command::SetMetronome(on));
    }

    /// Notes the engine played from the keyboard: for the piano roll's keys,
    /// and for recording.
    fn receive_played(&mut self) {
        let Ok(audio) = &mut self.audio else {
            return;
        };
        let mut played = Vec::new();
        while let Some(event) = audio.handle.pop_played() {
            played.push(event);
        }
        let rate = self.compiler.sample_rate();
        for event in played {
            match event.kind {
                NoteEventKind::On { pitch, .. } => self.live_keys[usize::from(pitch.get())] = true,
                NoteEventKind::Off { pitch, .. } => {
                    self.live_keys[usize::from(pitch.get())] = false;
                }
                _ => {}
            }
            let (Some(sample), true, Some(track)) =
                (event.sample, self.recording, self.live_target)
            else {
                continue;
            };
            // Played in time with what was heard, which lags the engine.
            let sample = sample.saturating_sub(self.compiler.latency() as u64);
            let tick = self.project.tempo.sample_to_tick(sample as i64, rate);
            if self.recorder.as_ref().is_some_and(|r| r.track != track) {
                self.finish_recording();
            }
            let recorder = self.recorder.get_or_insert_with(|| Recorder::new(track));
            recorder.play(tick, event.kind);
        }
    }

    /// Puts what was recorded on its track: into the clip it was played
    /// over (overdubbing), or a new clip covering whole bars. One undo step.
    fn finish_recording(&mut self) {
        let Some(mut recorder) = self.recorder.take() else {
            return;
        };
        let now = Tick::new(self.playhead().round() as i64);
        let notes = recorder.finish(now);
        let (Some(first), Some(last)) = (
            notes.iter().map(|n| n.0).min(),
            notes.iter().map(|n| n.1).max(),
        ) else {
            return;
        };
        let track = recorder.track;
        let count = notes.len();
        let recorded = self.edit("Record", |p| {
            let covering = p.track(track).and_then(|t| {
                t.clips().into_iter().find_map(|c| match c {
                    ClipRef::Midi(m) if m.start <= first && m.end() >= last => Some(m.id),
                    _ => None,
                })
            });
            let clip = match covering {
                Some(clip) => clip,
                None => {
                    let bar = p.tempo.bar_len().get().max(1);
                    let start = Tick::new(first.get().div_euclid(bar) * bar).max(Tick::ZERO);
                    let end = Tick::new((last.get() + bar - 1).div_euclid(bar) * bar)
                        .max(start + Tick::new(bar));
                    p.add_midi_clip(track, start, end - start)?
                }
            };
            let Some((_, ClipRef::Midi(midi))) = p.clip(clip) else {
                return Err(EditError::NoSuchClip(clip));
            };
            let origin = midi.start - midi.offset;
            let mut all = midi.notes.clone();
            for (start, end, pitch, velocity) in notes {
                all.push(Note {
                    id: p.new_note_id(),
                    start: start - origin,
                    len: end - start,
                    pitch,
                    velocity,
                    release_velocity: midi::Velocity::CENTER,
                    muted: false,
                });
            }
            p.set_notes(clip, all)?;
            Ok(clip)
        });
        if recorded.is_some() {
            self.info(format!("Recorded {count} notes"));
        }
    }

    /// Links the next controller that moves to `target` on `track` (`None`
    /// for the master). Asking again for the same control cancels.
    pub fn learn_controller(&mut self, track: Option<TrackId>, target: AutomationTarget) {
        if self.learning.as_ref() == Some(&(track, target.clone())) {
            self.learning = None;
            self.info("Controller linking cancelled");
        } else {
            self.learning = Some((track, target));
            self.info("Move a knob or fader on your MIDI controller…");
        }
    }

    pub fn learning(&self) -> Option<&(Option<TrackId>, AutomationTarget)> {
        self.learning.as_ref()
    }

    /// A controller moved: link it if a control is waiting, else move what
    /// it's linked to.
    pub fn control(&mut self, control: Control) {
        let Control {
            channel,
            controller,
            value,
        } = control;
        if let Some((track, target)) = self.learning.take() {
            let link = ControllerLink {
                channel,
                controller,
                track,
                target,
            };
            if self
                .edit("Link Controller", |p| p.link_controller(link))
                .is_some()
            {
                self.info(format!("Linked CC {controller}"));
            }
            return;
        }
        let Some(link) = self.project.controller_link(channel, controller).cloned() else {
            return;
        };
        let value = f32::from(value) / 127.0;
        let mixer = match link.track {
            Some(track) => self.project.track(track).map(|t| t.mixer),
            None => Some(self.project.master),
        };
        match (&link.target, mixer) {
            (AutomationTarget::Volume, Some(mixer)) => {
                let volume_db = project::normalized_to_volume(value);
                self.live_mixer(link.track, Mixer { volume_db, ..mixer });
            }
            (AutomationTarget::Pan, Some(mixer)) if link.track.is_some() => {
                let pan = project::normalized_to_pan(value);
                self.live_mixer(link.track, Mixer { pan, ..mixer });
            }
            (AutomationTarget::InstrumentParam(id), _) => {
                let Some(track) = link.track.and_then(|t| self.project.track(t)) else {
                    return;
                };
                let TrackKind::Midi { instrument, .. } = &track.kind else {
                    return;
                };
                let params = instrument.descriptor().map_or(&[][..], |d| d.params);
                let Some(index) = params.iter().position(|p| p.id == id.as_str()) else {
                    return;
                };
                let plain = params[index].from_normalized(value);
                let (track, id) = (track.id, id.clone());
                self.live_param(track, index, &id, plain);
            }
            (AutomationTarget::DeviceParam { device, param }, _) => {
                let Some(descriptor) = self
                    .project
                    .device(*device)
                    .and_then(|d| d.effect.descriptor())
                else {
                    return;
                };
                let Some(index) = descriptor
                    .params
                    .iter()
                    .position(|p| p.id == param.as_str())
                else {
                    return;
                };
                let plain = descriptor.params[index].from_normalized(value);
                let (device, param) = (*device, param.clone());
                self.live_device_param(device, index, &param, plain);
            }
            _ => return,
        }
        self.controller_moved = Some(Instant::now());
    }

    /// Renders the mix or tracks to files on a worker thread, as `jobs`
    /// say. The status bar shows progress, and the outcome.
    pub fn export(&mut self, jobs: Vec<export::Job>, settings: export::Settings) {
        self.end_live("Edit");
        if self.exporting.is_some() {
            self.error("Already exporting");
            return;
        }
        if jobs.is_empty() {
            return;
        }
        let loading = self
            .project
            .sources
            .iter()
            .any(|s| matches!(self.sources.get(&s.id), None | Some(SourceState::Loading)));
        if loading {
            self.error("Can't export yet: audio is still loading");
            return;
        }
        let rate = self.sample_rate();
        let mut audio = export::Audio::default();
        for (id, state) in &self.sources {
            if let SourceState::Ready(source) = state {
                audio.sources.insert(*id, source.audio.clone());
            }
        }
        self.project.visit(|track, _| {
            if let TrackKind::Audio { clips } = &track.kind {
                for clip in clips {
                    let current = self
                        .renders
                        .current(&self.project, &self.sources, rate, clip);
                    if let Some(rendered) = current {
                        audio.renders.insert(clip.id, rendered);
                    }
                }
            }
            if let Some(key) = DataKey::of(track)
                && let Some(data) = self.banks.data(&key)
            {
                audio.banks.insert(key, data);
            }
        });
        let project = self.project.clone();
        let wake = self.wake.clone();
        self.exporting = Some(export::start(project, audio, rate, jobs, settings, wake));
        self.info("Exporting…");
    }

    /// How far along the export is, `0..=1`, while one is running.
    pub fn export_progress(&self) -> Option<f32> {
        self.exporting.as_ref().map(Exporting::progress)
    }

    /// True while a clip's reversed, looped or stretched audio is being made.
    pub fn is_rendering(&self, clip: ClipId) -> bool {
        self.renders.is_rendering(clip)
    }

    pub fn source_state(&self, id: SourceId) -> Option<&SourceState> {
        self.sources.get(&id)
    }

    /// Loads every project source that isn't loaded or loading.
    fn request_sources(&mut self) {
        let rate = self.sample_rate();
        for source in &self.project.sources {
            if let std::collections::hash_map::Entry::Vacant(entry) = self.sources.entry(source.id)
            {
                entry.insert(SourceState::Loading);
                let key = source_key(self.generation, source.id);
                self.loader.request(key, source.path.clone(), rate);
            }
        }
    }

    /// Imports audio files onto `track` from `start`, one after another, or
    /// onto a new audio track at `position` if `track` is `None` or isn't an
    /// audio track. The clips appear once each file has loaded.
    pub fn import_files(
        &mut self,
        paths: Vec<PathBuf>,
        track: Option<TrackId>,
        start: Tick,
        position: TrackPosition,
    ) {
        let track = track.filter(|id| {
            matches!(
                self.project.track(*id).map(|t| &t.kind),
                Some(TrackKind::Audio { .. })
            )
        });
        let mut accepted = Vec::new();
        let mut rejected = None;
        for path in paths {
            if !media::is_supported_extension(&path) {
                rejected = Some(format!(
                    "{}: not an audio file canopy can read",
                    path.display()
                ));
            } else if path.to_str().is_none() {
                // Project files store paths as text.
                rejected = Some(format!(
                    "{}: rename the file; canopy can't save paths that aren't valid UTF-8",
                    path.display()
                ));
            } else {
                accepted.push(path);
            }
        }
        if !accepted.is_empty() {
            let batch = self.next_import;
            self.next_import += 1;
            self.batches.insert(
                batch,
                ImportBatch {
                    track,
                    cursor: start.max(Tick::ZERO),
                    position,
                },
            );
            let rate = self.sample_rate();
            for path in accepted {
                self.next_import += 1;
                let key = IMPORT_KEY | self.next_import;
                self.imports.insert(key, (batch, path.clone()));
                self.loader.request(key, path, rate);
            }
            self.info("Loading audio…");
        }
        // Shown last so it isn't hidden behind "Loading audio…".
        if let Some(rejected) = rejected {
            self.error(rejected);
        }
    }

    fn finish_load(&mut self, loaded: Loaded) {
        if loaded.target_rate != self.sample_rate() {
            // Converted for a rate we no longer run at. A fresh request for
            // project sources went out when the rate changed.
            if loaded.key & IMPORT_KEY != 0 {
                self.loader
                    .request(loaded.key, loaded.path, self.sample_rate());
            }
            return;
        }
        if loaded.key & IMPORT_KEY == 0 {
            let id = SourceId(loaded.key as u32);
            if loaded.key != source_key(self.generation, id) {
                return; // for a project that's been closed since
            }
            let state = match loaded.result {
                Ok(source) => SourceState::Ready(source),
                Err(err) => {
                    self.error(format!("{}: {err}", loaded.path.display()));
                    SourceState::Failed(err)
                }
            };
            self.sources.insert(id, state);
            self.needs_rebuild = true;
            return;
        }

        let Some((batch_id, path)) = self.imports.remove(&loaded.key) else {
            return;
        };
        let source = match loaded.result {
            Ok(source) => source,
            Err(err) => {
                self.error(format!("Couldn't import {}: {err}", path.display()));
                return;
            }
        };
        let Some(batch) = self.batches.get(&batch_id) else {
            return;
        };
        let (track, cursor, position) = (batch.track, batch.cursor, batch.position);
        let placed = self.edit("Import Audio", |p| {
            let track = match track.filter(|t| p.track(*t).is_some()) {
                Some(track) => track,
                // The spot it was meant for may be gone by now.
                None => p
                    .add_track(NewTrack::Audio, position)
                    .or_else(|_| p.add_track(NewTrack::Audio, TrackPosition::END))?,
            };
            let id = p.add_source(
                path.clone(),
                source.native_rate,
                source.native_frames,
                source.channels,
            );
            let clip = p.add_audio_clip(track, cursor, id)?;
            let (_, clip) = p.clip(clip).expect("just added");
            Ok((track, id, p.clip_end(clip)))
        });
        if let Some((track, id, end)) = placed {
            self.sources.insert(id, SourceState::Ready(source));
            if let Some(batch) = self.batches.get_mut(&batch_id) {
                batch.track = Some(track);
                batch.cursor = end;
            }
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            self.info(format!("Imported {name}"));
        }
    }

    // Project files.

    fn replace_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.end_live("Edit");
        // Loads still in flight belong to the old project.
        self.generation = self.generation.wrapping_add(1);
        self.imports.clear();
        self.batches.clear();
        self.deferred.clear();
        self.recorder = None;
        self.learning = None;
        self.set_playing(false);
        self.seek(Tick::ZERO);
        self.project = project;
        self.plugins.manager.forget();
        self.history.clear();
        self.path = path;
        self.revision += 1;
        self.saved_revision = self.revision;
        self.needs_rebuild = true;
        self.sources.clear();
        self.renders.clear();
        self.compiler.invalidate();
        self.request_sources();
        self.sync_master();
    }

    pub fn new_project(&mut self) {
        self.replace_project(Project::with_default_tracks(), None);
        self.info("New project");
    }

    pub fn open(&mut self, path: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => return self.error(format!("Couldn't open {}: {err}", path.display())),
        };
        match Project::from_file_string(&text) {
            Ok(mut project) => {
                relink_sources(&mut project, path);
                self.replace_project(project, Some(path.to_owned()));
                self.info(format!("Opened {}", path.display()));
            }
            Err(err) => self.error(format!("Couldn't open {}: {err}", path.display())),
        }
    }

    /// Saves to the current path. `None` if there isn't one yet; otherwise
    /// whether it worked.
    pub fn save(&mut self) -> Option<bool> {
        let path = self.path.clone()?;
        Some(self.save_as(&path))
    }

    /// Returns whether it worked; failures are reported in the status bar.
    pub fn save_as(&mut self, path: &Path) -> bool {
        self.end_live("Edit");
        self.refresh_plugin_states();
        let path = with_project_extension(path);
        let text = match self.project.to_file_string() {
            Ok(text) => text,
            Err(err) => {
                self.error(format!("Couldn't save {}: {err}", path.display()));
                return false;
            }
        };
        // Write next to the target and rename, so a crash mid-save can't
        // leave a half-written project behind.
        let mut temp = path.clone().into_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        let result = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, &path));
        match result {
            Ok(()) => {
                self.saved_revision = self.revision;
                self.info(format!("Saved {}", path.display()));
                self.path = Some(path);
                true
            }
            Err(err) => {
                let _ = std::fs::remove_file(&temp);
                self.error(format!("Couldn't save {}: {err}", path.display()));
                false
            }
        }
    }

    /// The user chose to quit without saving; stop guarding the changes.
    pub fn mark_saved_for_quit(&mut self) {
        self.saved_revision = self.revision;
    }

    pub fn display_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_stem()).map_or_else(
            || "Untitled".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        )
    }
}

/// Adds `.canopy` unless it's already there. Not `with_extension`, which
/// would turn "mix v1.2" into "mix v1.canopy".
fn with_project_extension(path: &Path) -> PathBuf {
    if path
        .extension()
        .is_some_and(|e| e == project::FILE_EXTENSION)
    {
        return path.to_owned();
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(project::FILE_EXTENSION);
    PathBuf::from(name)
}

/// If an audio file has moved along with the project, look for it next to
/// the project file under the same name.
fn relink_sources(project: &mut Project, project_path: &Path) {
    let Some(dir) = project_path.parent() else {
        return;
    };
    for source in &mut project.sources {
        if source.path.exists() {
            continue;
        }
        if let Some(name) = source.path.file_name() {
            let candidate = dir.join(name);
            if candidate.exists() {
                source.path = candidate;
            }
        }
    }
}

fn start_audio(sample_rate: u32) -> Result<Audio, String> {
    engine::device::start(sample_rate)
        .map(|(handle, output)| Audio {
            handle,
            output: Some(output),
        })
        .map_err(|err| err.to_string())
}

fn engine_rate(audio: &Result<Audio, String>, requested: u32) -> u32 {
    audio
        .as_ref()
        .map_or(requested, |a| a.handle.sample_rate() as u32)
}
