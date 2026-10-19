//! [`PluginManager`]: the plugin instances behind a project's plugin
//! devices and instruments, kept in step with the project.
//!
//! # The project leads
//!
//! The project says which plugins there are (each device's or MIDI track's
//! format and id), their parameter values and their saved state.
//! [`PluginManager::pump`], every frame before the project is compiled,
//! loads an instance for each plugin the project names, puts away the ones
//! it no longer names (for a while, so undoing a removal brings the same
//! instance back), and loads a state into an instance when the project's
//! changes (an undo, a preset, a paste). The engine's compiler then asks
//! for running adapters ([`PluginManager::effect`],
//! [`PluginManager::instrument`]) through the session's lookup.
//!
//! What the plugin does by itself flows the other way, as [`Update`]s for
//! the session to apply: a control moved in the plugin's editor (one undo
//! step per gesture), a preset loaded there, a state that changed.
//!
//! # Restarts
//!
//! A plugin that asks for a restart (its latency or ports changed) gets a
//! new generation: the next compile asks for a fresh adapter while the
//! engine still runs the old one, gets none, and leaves the device silent
//! for that snapshot. Once the old adapter is back on this thread and
//! dropped (which deactivates the plugin), the manager asks for another
//! compile, which activates it again.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use effects::Effect;
use instruments::Instrument;
use plugins::{
    AudioConfig, EditorOptions, Host, ParamId, PluginEvent, PluginFormat, PluginInfo,
    PluginInstance, PluginKind,
};
use project::{Blob, Device, DeviceId, PluginData, Project, Track, TrackId, TrackKind};

use crate::adapter::{PluginEffect, PluginInstrument};
use crate::catalog::Catalog;
use crate::describe::{self, Described};

/// How long a plugin's own changes without a gesture (and changes to its
/// state) wait to be still before they become one undo step.
const QUIET: Duration = Duration::from_millis(400);

/// How many removed plugins are kept loaded, so that undoing their removal
/// brings back the same instance, editor and all.
const KEPT: usize = 16;

/// What a plugin belongs to: an effect device, or a MIDI track as its
/// instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Owner {
    Effect(DeviceId),
    Instrument(TrackId),
}

/// Which plugin: its format and id.
type Key = (PluginFormat, String);

/// A change a plugin made by itself, for the session to put in the
/// project.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// A parameter moved in the plugin (its editor): set it in the project
    /// as part of a live gesture. Its sound has already changed.
    Param {
        owner: Owner,
        /// Canopy's parameter id.
        param: &'static str,
        value: f32,
    },
    /// The plugin's gestures are over: end the live gesture as one undo
    /// step with this name.
    Commit { label: String },
    /// One undo step: new values for all of a plugin's parameters (a preset
    /// it loaded), a new saved state, or both.
    Edit {
        owner: Owner,
        label: String,
        values: Option<BTreeMap<String, f32>>,
        state: Option<Blob>,
    },
    /// Values the project doesn't have for a plugin it just loaded (one
    /// imported with only its state, or a newer version with more
    /// parameters): the plugin's own, to put in the project as they are,
    /// not as an undo step. Without them the engine would set the
    /// parameters to their defaults.
    Fill {
        owner: Owner,
        values: BTreeMap<String, f32>,
    },
}

/// Why a plugin the project names isn't playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Loaded; it plays.
    Loaded,
    /// The first scan hasn't finished: it may yet be found.
    Scanning,
    /// No scan found it.
    NotInstalled,
    /// It was found but wouldn't load or start.
    Failed(String),
}

/// A plugin loaded for a device that doesn't exist yet: the session adds
/// the device with these, then hands the instance over with
/// [`PluginManager::attach`].
pub struct Loaded {
    entry: Entry,
    /// What the project keeps about it, its first state included.
    pub data: PluginData,
    /// Every parameter's value, by canopy's id.
    pub values: BTreeMap<String, f32>,
    pub info: PluginInfo,
}

impl Loaded {
    /// The reference a device or track names it by.
    pub fn effect_ref(&self) -> effects::EffectRef {
        effects::EffectRef {
            format: describe::format(self.info.format),
            id: self.info.id.clone(),
            version: self.info.version.clone(),
        }
    }

    pub fn instrument_ref(&self) -> instruments::InstrumentRef {
        instruments::InstrumentRef {
            format: describe::format(self.info.format),
            id: self.info.id.clone(),
            version: self.info.version.clone(),
        }
    }
}

struct Entry {
    key: Key,
    instance: PluginInstance,
    described: Rc<Described>,
    /// Bumped whenever the engine must get a fresh adapter.
    generation: u64,
    /// The state the instance was last given or gave: the project holding
    /// another means it must be loaded.
    state: Blob,
    /// The compiler asked for an adapter while the last one was still out.
    waiting: Cell<bool>,
    /// A restart was asked for and is under way: it ends with the next
    /// activation.
    restarting: bool,
    /// Parameters whose gesture in the editor is under way.
    gestures: HashSet<ParamId>,
    /// When the plugin last moved a parameter itself.
    moved: Option<Instant>,
    /// It said its state changed, at this time.
    dirty: Option<Instant>,
    /// The parameter it last moved, by descriptor index.
    touched: Option<usize>,
    /// Its parameters were rescanned because of a state we loaded.
    loading: bool,
    /// Why it wouldn't start, if it wouldn't: it isn't asked again.
    broken: Option<String>,
}

/// Every plugin instance of the session, and the catalog they come from.
pub struct PluginManager {
    host: Host,
    catalog: Catalog,
    entries: RefCell<HashMap<Owner, Entry>>,
    /// Removed plugins, most recently removed last.
    kept: Vec<(Owner, Entry)>,
    described: HashMap<Key, Rc<Described>>,
    /// Plugins that couldn't be loaded, and why, as of a catalog version.
    failed: HashMap<Owner, (u64, Status)>,
    next_generation: Cell<u64>,
    /// The plugins' changes waiting to become one undo step: its name, and
    /// when the last arrived.
    live: Option<(String, Instant)>,
    keep_on_top: bool,
    /// A new snapshot is needed.
    rebuild: Cell<bool>,
    /// Values just-loaded plugins have that the project doesn't.
    fills: Vec<(Owner, BTreeMap<String, f32>)>,
}

impl Default for PluginManager {
    fn default() -> Self {
        PluginManager::new()
    }
}

impl PluginManager {
    /// Makes the host on this thread, which becomes the plugins' main
    /// thread: keep the manager on it.
    pub fn new() -> PluginManager {
        PluginManager {
            host: Host::new(),
            catalog: Catalog::default(),
            entries: RefCell::default(),
            kept: Vec::new(),
            described: HashMap::new(),
            failed: HashMap::new(),
            next_generation: Cell::new(1),
            live: None,
            keep_on_top: false,
            rebuild: Cell::new(false),
            fills: Vec::new(),
        }
    }

    /// See [`Host::set_waker`].
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.host.set_waker(wake);
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn catalog_mut(&mut self) -> &mut Catalog {
        &mut self.catalog
    }

    fn generation(&self) -> u64 {
        let next = self.next_generation.get();
        self.next_generation.set(next + 1);
        next
    }

    /// The main-thread work of a frame, before the project is compiled:
    /// runs the plugins' callbacks, picks up a finished scan, brings the
    /// instances in line with `project`, and returns what the plugins
    /// changed. True in the second place if the engine needs a new
    /// snapshot.
    pub fn pump(&mut self, project: &Project) -> (Vec<Update>, bool) {
        self.host.idle();
        self.catalog.poll();
        self.reconcile(project);
        let mut updates: Vec<Update> = self
            .fills
            .drain(..)
            .map(|(owner, values)| Update::Fill { owner, values })
            .collect();
        self.collect(&mut updates);
        let now = Instant::now();
        let gestures = self
            .entries
            .borrow()
            .values()
            .any(|e| !e.gestures.is_empty());
        if !gestures
            && let Some((label, at)) = &self.live
            && now.duration_since(*at) >= QUIET
        {
            updates.push(Update::Commit {
                label: label.clone(),
            });
            self.live = None;
        }
        if self.live.is_none() {
            self.save_dirty(now, &mut updates);
        }
        for entry in self.entries.borrow().values() {
            if entry.waiting.get() && !entry.instance.is_active() {
                entry.waiting.set(false);
                self.rebuild.set(true);
            }
        }
        (updates, self.rebuild.replace(false))
    }

    /// Loads, keeps and puts away instances to match the plugins `project`
    /// names, and loads states the project changed.
    fn reconcile(&mut self, project: &Project) {
        let wanted = wanted(project);
        let names: HashMap<Owner, &Key> = wanted.iter().map(|w| (w.owner, &w.key)).collect();
        // Put away what the project no longer names (or names another
        // plugin for: a track's instrument changed).
        let gone: Vec<Owner> = self
            .entries
            .get_mut()
            .iter()
            .filter(|(owner, entry)| names.get(owner) != Some(&&entry.key))
            .map(|(owner, _)| *owner)
            .collect();
        for owner in gone {
            if let Some(mut entry) = self.entries.get_mut().remove(&owner) {
                entry.instance.close_editor();
                entry.gestures.clear();
                self.kept.push((owner, entry));
            }
        }
        if self.kept.len() > KEPT {
            let extra = self.kept.len() - KEPT;
            self.kept.drain(..extra);
        }
        self.failed.retain(|owner, _| names.contains_key(owner));
        for want in &wanted {
            if !self.entries.get_mut().contains_key(&want.owner) {
                let kept = self
                    .kept
                    .iter()
                    .rposition(|(owner, entry)| *owner == want.owner && entry.key == want.key);
                if let Some(index) = kept {
                    let (_, mut entry) = self.kept.remove(index);
                    entry.generation = self.generation();
                    self.entries.get_mut().insert(want.owner, entry);
                    self.rebuild.set(true);
                } else if let Err(status) = self.load_for(want) {
                    self.failed
                        .insert(want.owner, (self.catalog.version(), status));
                    continue;
                }
            }
            // A state the project holds that the instance doesn't: an
            // undo, a preset, a paste.
            let entry = self.entries.get_mut().get_mut(&want.owner).expect("loaded");
            if let Some(state) = want.state
                && !state.is_empty()
                && *state != entry.state
            {
                if entry.instance.load_state(state.as_bytes()).is_ok() {
                    // The rescan this causes is ours, not the user's.
                    entry.loading = true;
                    push_values(entry, want.values);
                }
                entry.state = state.clone();
            } else if let Some(state) = want.state {
                entry.state = state.clone();
            }
        }
    }

    /// Loads an instance for a plugin the project names.
    fn load_for(&mut self, want: &Want) -> Result<(), Status> {
        if let Some((version, status)) = self.failed.get(&want.owner)
            && *version == self.catalog.version()
        {
            return Err(status.clone());
        }
        let Some(info) = self.catalog.find(want.key.0, &want.key.1).cloned() else {
            return Err(if self.catalog.is_scanned() {
                Status::NotInstalled
            } else {
                Status::Scanning
            });
        };
        let mut entry = self.load_entry(&info).map_err(Status::Failed)?;
        if let Some(state) = want.state
            && !state.is_empty()
        {
            match entry.instance.load_state(state.as_bytes()) {
                Ok(()) => entry.loading = true,
                Err(err) => eprintln!("{}: couldn't restore its state: {err}", info.name),
            }
            entry.state = state.clone();
        }
        push_values(&mut entry, want.values);
        let missing: BTreeMap<String, f32> = values(&entry)
            .into_iter()
            .filter(|(key, _)| !want.values.contains_key(key))
            .collect();
        if !missing.is_empty() {
            self.fills.push((want.owner, missing));
        }
        self.entries.get_mut().insert(want.owner, entry);
        self.rebuild.set(true);
        Ok(())
    }

    /// Loads `info` and describes it (registering its description the
    /// first time).
    fn load_entry(&mut self, info: &PluginInfo) -> Result<Entry, String> {
        let instance = self.host.load(info).map_err(|e| e.to_string())?;
        let key = (info.format, info.id.clone());
        let described = match self.described.get(&key) {
            Some(described) => described.clone(),
            None => {
                let described = Rc::new(describe::register(&instance));
                self.described.insert(key.clone(), described.clone());
                described
            }
        };
        let mut instance = instance;
        if self.keep_on_top {
            instance.set_editor_keep_on_top(true);
        }
        Ok(Entry {
            key,
            instance,
            described,
            generation: self.generation(),
            state: Blob::default(),
            waiting: Cell::new(false),
            restarting: false,
            gestures: HashSet::new(),
            moved: None,
            dirty: None,
            touched: None,
            loading: false,
            broken: None,
        })
    }

    /// Lets go of every instance, for a project replaced by another (opened
    /// or new): ids start over in each project, so nothing carries over.
    pub fn forget(&mut self) {
        for entry in self.entries.get_mut().values_mut() {
            entry.instance.close_editor();
        }
        self.entries.get_mut().clear();
        self.kept.clear();
        self.failed.clear();
        self.live = None;
    }

    /// Loads `info` for a device or instrument about to be added: the
    /// caller adds it to the project with what this returns, then calls
    /// [`Self::attach`]. Its description is registered by then, so the
    /// project accepts it.
    pub fn load(&mut self, info: &PluginInfo) -> Result<Loaded, String> {
        let mut entry = self.load_entry(info)?;
        let state = entry
            .instance
            .save_state()
            .map(Blob::from)
            .unwrap_or_default();
        entry.state = state.clone();
        let values = values(&entry);
        Ok(Loaded {
            entry,
            data: PluginData {
                name: info.name.clone(),
                vendor: info.vendor.clone(),
                path: Some(info.path.clone()),
                state,
            },
            values,
            info: info.clone(),
        })
    }

    /// Hands a [`Self::load`]ed instance to the device or track it was
    /// added as.
    pub fn attach(&mut self, owner: Owner, loaded: Loaded) {
        self.failed.remove(&owner);
        // A track's earlier plugin instrument is kept, for undo.
        if let Some(mut earlier) = self.entries.get_mut().insert(owner, loaded.entry) {
            earlier.instance.close_editor();
            earlier.gestures.clear();
            self.kept.push((owner, earlier));
        }
        self.rebuild.set(true);
    }

    /// Turns what each plugin did since the last frame into updates.
    fn collect(&mut self, updates: &mut Vec<Update>) {
        let now = Instant::now();
        let mut rescanned = Vec::new();
        let mut ended = false;
        for (owner, entry) in self.entries.get_mut().iter_mut() {
            if entry.instance.needs_restart() && !entry.restarting {
                entry.restarting = true;
                entry.generation = self.next_generation.get();
                self.next_generation.set(entry.generation + 1);
                self.rebuild.set(true);
            }
            let label = format!("Change {}", entry.described.info.name);
            for event in entry.instance.take_events() {
                match event {
                    PluginEvent::ParamGesture { id, begin } => {
                        if begin {
                            entry.gestures.insert(id);
                        } else {
                            entry.gestures.remove(&id);
                            ended = true;
                        }
                        if entry.described.index_of(id).is_some() {
                            self.live = Some((label.clone(), now));
                        }
                    }
                    PluginEvent::ParamValue { id, value } => {
                        let Some(index) = entry.described.index_of(id) else {
                            continue;
                        };
                        let map = entry.described.params[index];
                        updates.push(Update::Param {
                            owner: *owner,
                            param: entry.described.descriptors[index].id,
                            value: map.from_plugin(value),
                        });
                        entry.touched = Some(index);
                        entry.moved = Some(now);
                        self.live = Some((label.clone(), now));
                    }
                    PluginEvent::ParamsRescanned { infos } => {
                        if infos {
                            rescanned.push(entry.key.clone());
                        }
                        if std::mem::take(&mut entry.loading) && !infos {
                            continue;
                        }
                        // A preset loaded in its editor: its values and the
                        // rest of its state, as one step.
                        entry.moved = Some(now);
                        let state = entry
                            .instance
                            .save_state()
                            .ok()
                            .map(Blob::from)
                            .filter(|s| *s != entry.state);
                        if let Some(state) = &state {
                            entry.state = state.clone();
                        }
                        updates.push(Update::Edit {
                            owner: *owner,
                            label: label.clone(),
                            values: Some(values(entry)),
                            state,
                        });
                    }
                    PluginEvent::StateDirty => {
                        // Moving a parameter dirties the state too, and
                        // the parameter's undo step covers that.
                        let recent = entry
                            .moved
                            .is_some_and(|t| now.duration_since(t) < QUIET * 2);
                        if !recent && entry.gestures.is_empty() {
                            entry.dirty = Some(now);
                        }
                    }
                    PluginEvent::LatencyChanged(_) => self.rebuild.set(true),
                    PluginEvent::EditorClosed => {
                        ended |= !entry.gestures.is_empty();
                        entry.gestures.clear();
                    }
                    PluginEvent::RestartRequested | PluginEvent::TailChanged => {}
                }
            }
        }
        // Gestures that ended this frame commit at once; changes without
        // gestures wait to be still for a moment (see `pump`).
        let open = self
            .entries
            .get_mut()
            .values()
            .any(|e| !e.gestures.is_empty());
        if ended
            && !open
            && let Some((label, _)) = self.live.take()
        {
            updates.push(Update::Commit { label });
        }
        for key in rescanned {
            self.redescribe(&key);
        }
    }

    /// The plugin's parameters themselves changed: describes it again and
    /// gives every instance of it a fresh adapter.
    fn redescribe(&mut self, key: &Key) {
        let mut entries = self.entries.borrow_mut();
        let Some(first) = entries.values().find(|e| &e.key == key) else {
            return;
        };
        let described = Rc::new(describe::register(&first.instance));
        self.described.insert(key.clone(), described.clone());
        for entry in entries.values_mut().filter(|e| &e.key == key) {
            entry.described = described.clone();
            entry.generation = self.next_generation.get();
            self.next_generation.set(entry.generation + 1);
        }
        self.rebuild.set(true);
    }

    /// Saves the states plugins said changed and have been left alone
    /// since, as undo steps.
    fn save_dirty(&mut self, now: Instant, updates: &mut Vec<Update>) {
        for (owner, entry) in self.entries.get_mut().iter_mut() {
            let Some(since) = entry.dirty else {
                continue;
            };
            if now.duration_since(since) < QUIET || !entry.gestures.is_empty() {
                continue;
            }
            entry.dirty = None;
            let Ok(bytes) = entry.instance.save_state() else {
                continue;
            };
            if bytes.as_slice() == entry.state.as_bytes() {
                continue;
            }
            entry.state = Blob::from(bytes);
            updates.push(Update::Edit {
                owner: *owner,
                label: format!("Change {}", entry.described.info.name),
                values: None,
                state: Some(entry.state.clone()),
            });
        }
    }

    /// A running adapter for plugin effect `device`, for the engine at
    /// `sample_rate` with blocks of up to `max_block`; `None` if it isn't
    /// loaded, or its last adapter is still out (see "Restarts").
    pub fn effect(
        &self,
        device: &Device,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Effect>> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get_mut(&Owner::Effect(device.id))?;
        let (processor, values) = activate(entry, sample_rate, max_block, false)?;
        let described = &entry.described;
        Some(Box::new(PluginEffect::new(
            processor,
            described.params.clone(),
            described.descriptors,
            &values,
        )))
    }

    /// Like [`Self::effect`], for `track`'s plugin instrument.
    pub fn instrument(
        &self,
        track: &Track,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Instrument>> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get_mut(&Owner::Instrument(track.id))?;
        let (processor, values) = activate(entry, sample_rate, max_block, false)?;
        let described = &entry.described;
        Some(Box::new(PluginInstrument::new(
            processor,
            described.params.clone(),
            described.descriptors,
            &values,
        )))
    }

    /// How late `owner`'s plugin makes its sound, as of its last
    /// activation.
    pub fn latency(&self, owner: Owner) -> Option<usize> {
        let entries = self.entries.borrow();
        let latency = entries.get(&owner)?.instance.latency();
        (latency > 0).then_some(latency as usize)
    }

    /// The generation of `owner`'s instance (0 for none): a new one means
    /// the engine must get a fresh adapter.
    pub fn generation_of(&self, owner: Owner) -> u64 {
        self.entries
            .borrow()
            .get(&owner)
            .map_or(0, |e| e.generation)
    }

    /// Why `owner`'s plugin isn't playing, or that it is.
    pub fn status(&self, owner: Owner) -> Status {
        if let Some(entry) = self.entries.borrow().get(&owner) {
            return match &entry.broken {
                Some(err) => Status::Failed(err.clone()),
                None => Status::Loaded,
            };
        }
        match self.failed.get(&owner) {
            Some((_, status)) => status.clone(),
            None if !self.catalog.is_scanned() => Status::Scanning,
            None => Status::NotInstalled,
        }
    }

    /// What scanning found about `owner`'s plugin.
    pub fn info(&self, owner: Owner) -> Option<PluginInfo> {
        let entries = self.entries.borrow();
        entries.get(&owner).map(|e| e.described.info.clone())
    }

    /// How `owner`'s plugin shows `value` (canopy's units) of the parameter
    /// at descriptor `index`, if it can say.
    pub fn param_text(&self, owner: Owner, index: usize, value: f32) -> Option<String> {
        let entries = self.entries.borrow();
        let entry = entries.get(&owner)?;
        let map = entry.described.params.get(index)?;
        entry.instance.param_text(map.id, map.to_plugin(value))
    }

    /// The parameter `owner`'s plugin last moved itself, by descriptor
    /// index.
    pub fn touched(&self, owner: Owner) -> Option<usize> {
        self.entries.borrow().get(&owner)?.touched
    }

    pub fn has_editor(&self, owner: Owner) -> bool {
        self.entries
            .borrow()
            .get(&owner)
            .is_some_and(|e| e.instance.has_editor())
    }

    pub fn editor_is_open(&self, owner: Owner) -> bool {
        self.entries
            .borrow()
            .get(&owner)
            .is_some_and(|e| e.instance.editor_is_open())
    }

    /// Opens `owner`'s editor in a window titled `title`, or closes it if
    /// it's open.
    pub fn toggle_editor(&mut self, owner: Owner, title: &str) -> Result<(), String> {
        let keep_on_top = self.keep_on_top;
        let entries = self.entries.get_mut();
        let entry = entries.get_mut(&owner).ok_or("the plugin isn't loaded")?;
        if entry.instance.editor_is_open() {
            entry.instance.close_editor();
            return Ok(());
        }
        let options = EditorOptions {
            title: title.to_owned(),
            keep_on_top,
        };
        entry
            .instance
            .open_editor(&options)
            .map_err(|e| e.to_string())
    }

    /// Whether editor windows stay above other windows.
    pub fn keep_on_top(&self) -> bool {
        self.keep_on_top
    }

    pub fn set_keep_on_top(&mut self, on_top: bool) {
        self.keep_on_top = on_top;
        for entry in self.entries.get_mut().values_mut() {
            entry.instance.set_editor_keep_on_top(on_top);
        }
    }

    /// Puts every plugin's current state into `project`, where it changed,
    /// before the project is saved or copied: not an undo step.
    pub fn refresh_states(&mut self, project: &mut Project) {
        for (owner, entry) in self.entries.get_mut().iter_mut() {
            let Ok(bytes) = entry.instance.save_state() else {
                continue;
            };
            if bytes.as_slice() != entry.state.as_bytes() {
                entry.state = Blob::from(bytes);
            }
            if let Some(data) = plugin_data(project, *owner)
                && !data.state.ptr_eq(&entry.state)
            {
                let data = PluginData {
                    state: entry.state.clone(),
                    ..data.clone()
                };
                set_plugin_data(project, *owner, data);
            }
        }
    }

    /// Separate instances of the plugins `project` names, for rendering it
    /// offline (an export or a freeze): each loaded with the state its live
    /// instance has now (or the project's, if it has none) and the project's
    /// values. Drop it on this thread once the render is done.
    pub fn offline(&mut self, project: &Project) -> Offline {
        self.offline_for(project, |_| true)
    }

    /// Like [`Self::offline`], for only the plugins `owner` picks (a
    /// freeze needs only its track's instrument).
    pub fn offline_for(
        &mut self,
        project: &Project,
        wanted_owner: impl Fn(Owner) -> bool,
    ) -> Offline {
        let mut slots = HashMap::new();
        for want in wanted(project)
            .into_iter()
            .filter(|w| wanted_owner(w.owner))
        {
            let Some(info) = self.catalog.find(want.key.0, &want.key.1).cloned() else {
                continue;
            };
            let state = match self.entries.get_mut().get_mut(&want.owner) {
                Some(live) => live.instance.save_state().ok().map(Blob::from),
                None => want.state.cloned(),
            };
            let Ok(mut entry) = self.load_entry(&info) else {
                continue;
            };
            if let Some(state) = state.filter(|s| !s.is_empty()) {
                let _ = entry.instance.load_state(state.as_bytes());
            }
            push_values(&mut entry, want.values);
            slots.insert(want.owner, entry);
        }
        Offline {
            entries: RefCell::new(slots),
        }
    }
}

/// Plugins loaded for an offline render (see [`PluginManager::offline`]).
pub struct Offline {
    entries: RefCell<HashMap<Owner, Entry>>,
}

impl Offline {
    /// An adapter for effect `device`, activated for offline rendering.
    pub fn effect(
        &self,
        device: &Device,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Effect>> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get_mut(&Owner::Effect(device.id))?;
        let (processor, values) = activate(entry, sample_rate, max_block, true)?;
        let described = &entry.described;
        Some(Box::new(PluginEffect::new(
            processor,
            described.params.clone(),
            described.descriptors,
            &values,
        )))
    }

    pub fn instrument(
        &self,
        track: &Track,
        sample_rate: u32,
        max_block: usize,
    ) -> Option<Box<dyn Instrument>> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get_mut(&Owner::Instrument(track.id))?;
        let (processor, values) = activate(entry, sample_rate, max_block, true)?;
        let described = &entry.described;
        Some(Box::new(PluginInstrument::new(
            processor,
            described.params.clone(),
            described.descriptors,
            &values,
        )))
    }

    pub fn latency(&self, owner: Owner) -> Option<usize> {
        let entries = self.entries.borrow();
        let latency = entries.get(&owner)?.instance.latency();
        (latency > 0).then_some(latency as usize)
    }

    pub fn generation_of(&self, owner: Owner) -> u64 {
        self.entries
            .borrow()
            .get(&owner)
            .map_or(0, |e| e.generation)
    }

    /// Whether it holds any plugin.
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

/// Activates `entry`'s plugin and returns its processor with its
/// parameters' current values; `None` (and a note to ask again) while its
/// last processor is still out, or if the plugin won't start.
fn activate(
    entry: &mut Entry,
    sample_rate: u32,
    max_block: usize,
    offline: bool,
) -> Option<(plugins::Processor, Vec<f32>)> {
    if entry.broken.is_some() {
        return None;
    }
    if entry.instance.is_active() {
        entry.waiting.set(true);
        return None;
    }
    let config = AudioConfig {
        offline,
        ..AudioConfig::new(f64::from(sample_rate), max_block as u32)
    };
    match entry.instance.activate(config) {
        Ok(processor) => {
            entry.restarting = false;
            let values: Vec<f32> = entry
                .described
                .params
                .iter()
                .map(|map| {
                    entry
                        .instance
                        .param_value(map.id)
                        .map_or(f32::NAN, |v| map.from_plugin(v))
                })
                .collect();
            Some((processor, values))
        }
        Err(err) => {
            eprintln!("{}: couldn't start: {err}", entry.described.info.name);
            entry.broken = Some(err.to_string());
            None
        }
    }
}

/// Every parameter's current value, by canopy's id.
fn values(entry: &Entry) -> BTreeMap<String, f32> {
    let described = &entry.described;
    described
        .params
        .iter()
        .zip(described.descriptors)
        .filter_map(|(map, descriptor)| {
            let value = entry.instance.param_value(map.id)?;
            Some((descriptor.id.to_owned(), map.from_plugin(value)))
        })
        .collect()
}

/// Gives the plugin the project's values for its parameters, where they
/// differ from its own: setting a value it already has can still set off
/// what some plugins do when a parameter is touched (a "reset" switch).
fn push_values(entry: &mut Entry, values: &BTreeMap<String, f32>) {
    let described = entry.described.clone();
    for (map, descriptor) in described.params.iter().zip(described.descriptors) {
        let Some(value) = values.get(descriptor.id) else {
            continue;
        };
        let value = map.to_plugin(*value);
        let same = entry
            .instance
            .param_value(map.id)
            .is_some_and(|now| (now - value).abs() <= 1e-6 * now.abs().max(1.0));
        if !same {
            entry.instance.set_param(map.id, value);
        }
    }
}

/// Whether `project` names any plugin.
pub fn uses_plugins(project: &Project) -> bool {
    !wanted(project).is_empty()
}

/// A plugin the project names.
struct Want<'a> {
    owner: Owner,
    key: Key,
    state: Option<&'a Blob>,
    values: &'a BTreeMap<String, f32>,
}

/// Every plugin `project` names: its plugin effects (the master's too)
/// and plugin instruments.
fn wanted(project: &Project) -> Vec<Want<'_>> {
    let mut wanted = Vec::new();
    fn add<'a>(devices: &'a [Device], wanted: &mut Vec<Want<'a>>) {
        for device in devices {
            let Some(format) = describe::plugin_format(device.effect.format) else {
                continue;
            };
            wanted.push(Want {
                owner: Owner::Effect(device.id),
                key: (format, device.effect.id.clone()),
                state: device.plugin.as_ref().map(|p| &p.state),
                values: &device.params,
            });
        }
    }
    add(&project.master_devices, &mut wanted);
    project.visit(|track, _| {
        add(&track.devices, &mut wanted);
        if let TrackKind::Midi {
            instrument,
            params,
            plugin,
            ..
        } = &track.kind
            && let Some(format) = describe::plugin_format(instrument.format)
        {
            wanted.push(Want {
                owner: Owner::Instrument(track.id),
                key: (format, instrument.id.clone()),
                state: plugin.as_ref().map(|p| &p.state),
                values: params,
            });
        }
    });
    wanted
}

/// What the project keeps about `owner`'s plugin.
fn plugin_data(project: &Project, owner: Owner) -> Option<&PluginData> {
    match owner {
        Owner::Effect(id) => project.device(id)?.plugin.as_ref(),
        Owner::Instrument(id) => match &project.track(id)?.kind {
            TrackKind::Midi { plugin, .. } => plugin.as_ref(),
            _ => None,
        },
    }
}

/// Replaces what the project keeps about `owner`'s plugin.
fn set_plugin_data(project: &mut Project, owner: Owner, data: PluginData) {
    let _ = match owner {
        Owner::Effect(id) => project.set_device_plugin(id, Some(data)),
        Owner::Instrument(id) => project.set_instrument_plugin(id, Some(data)),
    };
}

/// Whether `info` is something canopy can use as a device: an effect or
/// an instrument with stereo (or mono) sound, not a note effect.
pub fn usable(info: &PluginInfo) -> bool {
    match info.kind {
        PluginKind::Instrument => info.main_output().is_some(),
        PluginKind::Effect => info.main_output().is_some(),
        PluginKind::NoteEffect => false,
    }
}
