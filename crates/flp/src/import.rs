//! From an FL Studio [`fl::Project`] to a canopy `project::Project`.
//!
//! - **Channels** become tracks: MIDI tracks playing the built-in chosen for
//!   their generator, or audio tracks for sampler channels the playlist uses
//!   as audio clips. A channel whose clips overlap in time gets extra tracks
//!   ("Bass 2"), since canopy keeps one clip at a time on a track.
//! - **Patterns** placed in the playlist become a MIDI clip on each channel
//!   track they have notes for, with the item's position, length, trim and
//!   mute. Layer channels pass their notes on to their children.
//! - **Mixer inserts** become group tracks holding the tracks routed to them,
//!   with the insert's fader and effects; inserts routed into other inserts
//!   nest. An insert with a single track routed to it is folded into that
//!   track instead: the faders add up and the insert's effects go on it. The
//!   master insert is canopy's master.
//! - **Effects** carry their settings over where FL's are understood
//!   (Parametric EQ 2 fully, the compressor, Reeverb 2's main ones; see
//!   `convert.rs`), with each slot's dry/wet and bypass.
//! - **Automation clips** become automation lanes where they drive a
//!   channel's or insert's volume or pan, an effect's dry/wet, or an
//!   effect parameter whose meaning carries over.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use effects::EffectRef;
use instruments::InstrumentRef;
use midi::{PPQ, Pitch, Tick, Velocity};
use project::{
    AutomationPoint, AutomationTarget, Blob, Color, DeviceId, Mixer, NewTrack, Note, PluginData,
    TimeSignature, TrackId, TrackPosition, normalized_to_pan, pan_to_normalized,
    volume_to_normalized,
};

use crate::convert::{self, Conversion, Fidelity};
use crate::files::Files;
use crate::ids::mixer as mixer_param;
use crate::model as fl;
use crate::plugins::{self, Choice};

/// Whether a plan entry is a generator or an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Instrument,
    Effect,
}

/// One FL plugin the project uses, and what it would become.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    pub role: Role,
    /// The plugin's name in FL ("Sytrus", "Fruity Limiter", "Sampler").
    pub name: String,
    /// Where it's used: channels, or inserts.
    pub used_by: Vec<String>,
    pub suggestion: Choice,
    /// False when the suggestion is a guess.
    pub recognized: bool,
    /// For third-party plugins: which plugins they are, as canopy's plugin
    /// host names them (a name may stand for more than one, a VST2 and a
    /// VST3 of it, say), so the caller can offer [`Choice::Plugin`] for
    /// the installed ones.
    pub plugins: Vec<fl::PluginId>,
}

/// What the user picked for each plugin, by role and name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Choices(BTreeMap<(Role, String), Choice>);

impl Choices {
    pub fn get(&self, role: Role, name: &str) -> Option<Choice> {
        self.0.get(&(role, plugins::normalize(name))).copied()
    }

    pub fn set(&mut self, role: Role, name: &str, choice: Choice) {
        self.0.insert((role, plugins::normalize(name)), choice);
    }

    fn instrument(&self, name: &str) -> Choice {
        self.get(Role::Instrument, name)
            .unwrap_or_else(|| plugins::suggest_instrument(name).choice)
    }

    fn effect(&self, name: &str) -> Choice {
        self.get(Role::Effect, name)
            .unwrap_or_else(|| plugins::suggest_effect(name).choice)
    }
}

/// Every FL instrument and effect, with its suggested replacement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

impl Plan {
    pub fn new(project: &fl::Project) -> Plan {
        let layout = Layout::new(project, &Choices::default(), true);
        let mut items: Vec<PlanItem> = Vec::new();
        let mut add = |role: Role, name: String, user: String, plugin: Option<&fl::Plugin>| {
            let key = plugins::normalize(&name);
            let identity = plugin
                .and_then(|p| p.vst.as_ref())
                .and_then(|v| v.plugin.clone());
            match items
                .iter_mut()
                .find(|i| i.role == role && plugins::normalize(&i.name) == key)
            {
                Some(item) => {
                    if !item.used_by.contains(&user) {
                        item.used_by.push(user);
                    }
                    if let Some(identity) = identity
                        && !item.plugins.contains(&identity)
                    {
                        item.plugins.push(identity);
                    }
                }
                None => {
                    let suggestion = match role {
                        Role::Instrument => plugins::suggest_instrument(&name),
                        Role::Effect => plugins::suggest_effect(&name),
                    };
                    items.push(PlanItem {
                        role,
                        name,
                        used_by: vec![user],
                        suggestion: suggestion.choice,
                        recognized: suggestion.recognized,
                        plugins: identity.into_iter().collect(),
                    });
                }
            }
        };
        for channel in &project.channels {
            if let Some(name) = layout.instrument_name(channel) {
                add(
                    Role::Instrument,
                    name,
                    channel.display_name(),
                    channel.plugin.as_ref(),
                );
            }
        }
        for insert in &project.inserts {
            if !layout.insert_used(insert.index) {
                continue;
            }
            for slot in &insert.slots {
                let name = slot.plugin.display_name();
                if !name.is_empty() {
                    add(
                        Role::Effect,
                        name,
                        insert.display_name(),
                        Some(&slot.plugin),
                    );
                }
            }
        }
        Plan { items }
    }

    /// The suggestions, as choices to start from.
    pub fn choices(&self) -> Choices {
        let mut choices = Choices::default();
        for item in &self.items {
            choices.set(item.role, &item.name, item.suggestion);
        }
        choices
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Where to write the files the import makes: one-sample SFZs that let
    /// canopy's SFZ Sampler play FL Sampler channels, and Vital's
    /// wavetables for the Wavetable synth. `None` leaves those samplers
    /// without a file and those oscillators on built-in tables.
    pub sfz_dir: Option<PathBuf>,
    /// The third-party plugins installed here, which [`Choice::Plugin`]
    /// brings in as themselves.
    pub installed: HashSet<fl::PluginId>,
}

/// A file the imported project needs written before it plays: an SFZ for
/// a sampler channel, a wavetable for a Vital oscillator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: PathBuf,
    pub contents: Vec<u8>,
}

/// What an import made, in numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Summary {
    pub tempo: f64,
    pub tracks: usize,
    pub midi_clips: usize,
    pub notes: usize,
    pub audio_clips: usize,
    pub effects: usize,
    pub automation_lanes: usize,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub project: project::Project,
    pub summary: Summary,
    /// What didn't come across, one short line each.
    pub report: Vec<String>,
    pub files: Vec<GeneratedFile>,
}

/// Builds the canopy project. Never fails: what can't be imported is left
/// out and listed in the report.
pub fn import(
    project: &fl::Project,
    choices: &Choices,
    files: &Files,
    options: &Options,
) -> Import {
    let layout = Layout::new(project, choices, false);
    let mut builder = Builder {
        fl: project,
        layout: &layout,
        choices,
        files,
        options,
        p: project::Project::new(),
        report: Report::default(),
        generated: Vec::new(),
        channel_tracks: HashMap::new(),
        insert_tracks: HashMap::new(),
        merged_into: HashMap::new(),
        slot_devices: HashMap::new(),
    };
    builder.build();
    let Builder {
        p,
        report,
        generated,
        ..
    } = builder;
    let summary = summarize(&p);
    Import {
        project: p,
        summary,
        report: report.lines(),
        files: generated,
    }
}

fn summarize(p: &project::Project) -> Summary {
    let mut summary = Summary {
        tempo: p.tempo.tempo.to_bpm(),
        effects: p.master_devices.len(),
        ..Summary::default()
    };
    p.visit(|track, _| {
        summary.tracks += 1;
        summary.effects += track.devices.len();
        summary.automation_lanes += track.automation.len();
        for clip in track.clips() {
            match clip {
                project::ClipRef::Midi(midi) => {
                    summary.midi_clips += 1;
                    summary.notes += midi.notes.len();
                }
                project::ClipRef::Audio(_) => summary.audio_clips += 1,
            }
        }
    });
    summary
}

// Units.

/// FL's channel volume (`0..=12800`, 10000 by default) in dB. FL doesn't
/// document its curve; this is a square law with FL's default at 0 dB.
pub fn channel_volume_db(raw: f64) -> f32 {
    if raw <= 0.0 {
        return project::SILENCE_DB;
    }
    (40.0 * (raw / 10_000.0).log10()) as f32
}

/// FL's insert fader (`0..=16000`, 12800 at 0 dB) in dB. FL's curve, as
/// worked out on Image-Line's forum from typed-in values: a fader at `x`
/// (`0..=1`) passes a gain of `(11^(1.25 x) - 1) / 10`, which is 0 dB at
/// 0.8, -6 dB at 0.598 and +5.6 dB at the top. Fruity Send's and Fruity
/// Balance's knobs follow it too.
pub fn insert_volume_db(raw: f64) -> f32 {
    if raw <= 0.0 {
        return project::SILENCE_DB;
    }
    let x = (raw / 16_000.0).min(1.0);
    let gain = (11f64.powf(1.25 * x) - 1.0) / 10.0;
    (20.0 * gain.log10()) as f32
}

/// A clip's name without the " #2" FL adds to a copy's.
fn without_copy_number(name: &str) -> &str {
    match name.rsplit_once(" #") {
        Some((stem, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => stem,
        _ => name,
    }
}

fn clamp_db(db: f32) -> f32 {
    db.clamp(project::SILENCE_DB, project::MAX_VOLUME_DB)
}

/// FL's 0..=128 note velocities: 64 is the middle, 128 the top.
pub fn velocity(fl: u8) -> Velocity {
    let v = u32::from(fl.min(128));
    Velocity::new(((v * 65_535 + 64) / 128) as u16)
}

const FL_DEFAULT_CHANNEL_PAN: i32 = 6400;

// Layout: what becomes which track, decided before building anything.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum TrackRole {
    Midi,
    Audio,
}

/// A MIDI clip to make, in canopy ticks.
#[derive(Debug, Clone)]
struct MidiClipPlan {
    start: i64,
    length: i64,
    offset: i64,
    pattern: u16,
    muted: bool,
}

#[derive(Debug, Clone)]
struct AudioClipPlan {
    start: i64,
    length: i64,
    /// Into the sample, in quarter notes.
    offset_quarters: f64,
    muted: bool,
}

/// Everything decided without touching a canopy project.
struct Layout<'a> {
    fl: &'a fl::Project,
    arrangement: Option<&'a fl::Arrangement>,
    /// FL ticks to canopy ticks.
    scale: f64,
    /// Non-layer channels each channel's notes play on.
    layer_targets: HashMap<u16, Vec<u16>>,
    /// Per channel: its MIDI clips, sorted, each with its lane.
    midi_clips: BTreeMap<u16, Vec<(usize, MidiClipPlan)>>,
    audio_clips: BTreeMap<u16, Vec<(usize, AudioClipPlan)>>,
    /// Lanes (tracks) per channel and role.
    lanes: BTreeMap<(u16, TrackRole), usize>,
    /// Inserts' main outputs.
    parents: HashMap<u16, u16>,
    /// Inserts folded into the one track routed to them. The other used
    /// inserts, but the master, become groups.
    merged: HashMap<u16, (u16, TrackRole)>,
    used_inserts: BTreeSet<u16>,
    /// Channels whose generator is set to Skip.
    skipped: BTreeSet<u16>,
    /// Sends left out: (from, to).
    dropped_sends: Vec<(u16, u16)>,
}

impl<'a> Layout<'a> {
    /// `all_channels`: plan for every channel, as if nothing were skipped.
    fn new(fl: &'a fl::Project, choices: &Choices, all_channels: bool) -> Layout<'a> {
        let mut layout = Layout {
            fl,
            arrangement: fl.main_arrangement(),
            scale: PPQ as f64 / f64::from(fl.ppq.max(1)),
            layer_targets: HashMap::new(),
            midi_clips: BTreeMap::new(),
            audio_clips: BTreeMap::new(),
            lanes: BTreeMap::new(),
            parents: HashMap::new(),
            merged: HashMap::new(),
            used_inserts: BTreeSet::new(),
            skipped: BTreeSet::new(),
            dropped_sends: Vec::new(),
        };
        for channel in &fl.channels {
            let targets = layout.expand_layer(channel.iid);
            layout.layer_targets.insert(channel.iid, targets);
        }
        layout.plan_clips();
        if !all_channels {
            for channel in &fl.channels {
                if let Some(name) = layout.instrument_name(channel)
                    && layout.lanes.contains_key(&(channel.iid, TrackRole::Midi))
                    && choices.instrument(&name) == Choice::Skip
                {
                    layout.skipped.insert(channel.iid);
                    layout.lanes.remove(&(channel.iid, TrackRole::Midi));
                }
            }
        }
        layout.plan_routing();
        layout
    }

    fn ticks(&self, fl_ticks: f64) -> i64 {
        (fl_ticks * self.scale).round() as i64
    }

    /// The channels a channel's notes end up on: itself, or for a layer,
    /// its children's (recursively).
    fn expand_layer(&self, iid: u16) -> Vec<u16> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![iid];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            match self.fl.channel(current) {
                Some(c) if c.kind == fl::ChannelKind::Layer => {
                    stack.extend(c.children.iter().rev().copied());
                }
                Some(c) if c.kind == fl::ChannelKind::Automation => {}
                Some(_) => out.push(current),
                None => {}
            }
        }
        out
    }

    /// Patterns' notes by the channel they play on.
    fn pattern_channels(&self, pattern: &fl::Pattern) -> BTreeSet<u16> {
        pattern
            .notes
            .iter()
            .flat_map(|n| self.layer_targets.get(&n.channel).into_iter().flatten())
            .copied()
            .collect()
    }

    fn plan_clips(&mut self) {
        let Some(arrangement) = self.arrangement else {
            return self.plan_lanes();
        };
        let mut items: Vec<&fl::PlaylistItem> = arrangement.items.iter().collect();
        items.sort_by_key(|i| (i.position, i.track));
        let mut midi: BTreeMap<u16, Vec<MidiClipPlan>> = BTreeMap::new();
        let mut audio: BTreeMap<u16, Vec<AudioClipPlan>> = BTreeMap::new();
        for item in items {
            match item.target {
                Some(fl::ItemTarget::Pattern(iid)) => {
                    let Some(pattern) = self.fl.pattern(iid) else {
                        continue;
                    };
                    let length = match item.length {
                        0 => self.pattern_length(pattern),
                        n => self.ticks(f64::from(n)),
                    };
                    for channel in self.pattern_channels(pattern) {
                        midi.entry(channel).or_default().push(MidiClipPlan {
                            start: self.ticks(f64::from(item.position)),
                            length: length.max(1),
                            offset: self.ticks(item.start_offset.unwrap_or(0.0)),
                            pattern: iid,
                            muted: item.muted,
                        });
                    }
                }
                Some(fl::ItemTarget::Channel(iid)) => {
                    let Some(channel) = self.fl.channel(iid) else {
                        continue;
                    };
                    if channel.kind != fl::ChannelKind::Sampler {
                        continue;
                    }
                    audio.entry(iid).or_default().push(AudioClipPlan {
                        start: self.ticks(f64::from(item.position)),
                        length: self.ticks(f64::from(item.length)).max(1),
                        offset_quarters: item.start_offset.unwrap_or(0.0),
                        muted: item.muted,
                    });
                }
                None => {}
            }
        }
        for (channel, clips) in midi {
            let spans: Vec<(i64, i64)> = clips.iter().map(|c| (c.start, c.length)).collect();
            let lanes = assign_lanes(&spans);
            self.midi_clips
                .insert(channel, lanes.iter().copied().zip(clips).collect());
        }
        for (channel, clips) in audio {
            let spans: Vec<(i64, i64)> = clips.iter().map(|c| (c.start, c.length)).collect();
            let lanes = assign_lanes(&spans);
            self.audio_clips
                .insert(channel, lanes.iter().copied().zip(clips).collect());
        }
        self.plan_lanes();
    }

    /// A pattern's length when the playlist doesn't say: its notes, up to
    /// the next bar.
    fn pattern_length(&self, pattern: &fl::Pattern) -> i64 {
        let bar = f64::from(self.fl.ppq.max(1)) * 4.0;
        let end = pattern
            .length
            .map(f64::from)
            .unwrap_or_else(|| {
                pattern
                    .notes
                    .iter()
                    .map(|n| f64::from(n.position) + f64::from(n.length.max(1)))
                    .fold(0.0, f64::max)
            })
            .max(1.0);
        self.ticks((end / bar).ceil() * bar)
    }

    /// How many tracks each channel needs.
    fn plan_lanes(&mut self) {
        fn count<T>(clips: Option<&Vec<(usize, T)>>) -> usize {
            clips.map_or(0, |c| c.iter().map(|(lane, _)| lane + 1).max().unwrap_or(0))
        }
        for channel in &self.fl.channels {
            let midi = count(self.midi_clips.get(&channel.iid));
            let audio = count(self.audio_clips.get(&channel.iid));
            match channel.kind {
                fl::ChannelKind::Automation | fl::ChannelKind::Layer => {}
                fl::ChannelKind::Sampler => {
                    if audio > 0 {
                        self.lanes.insert((channel.iid, TrackRole::Audio), audio);
                    }
                    // A sampler used only for audio clips is just an
                    // audio track.
                    if midi > 0 || audio == 0 {
                        self.lanes
                            .insert((channel.iid, TrackRole::Midi), midi.max(1));
                    }
                }
                _ => {
                    self.lanes
                        .insert((channel.iid, TrackRole::Midi), midi.max(1));
                }
            }
        }
    }

    /// The generator's name, for channels that become MIDI tracks.
    fn instrument_name(&self, channel: &fl::Channel) -> Option<String> {
        if !self.lanes.contains_key(&(channel.iid, TrackRole::Midi))
            && !self.skipped.contains(&channel.iid)
        {
            return None;
        }
        match channel.kind {
            fl::ChannelKind::Sampler => Some(plugins::FL_SAMPLER.to_owned()),
            fl::ChannelKind::Automation | fl::ChannelKind::Layer => None,
            _ => {
                let name = channel
                    .plugin
                    .as_ref()
                    .map(fl::Plugin::plugin_name)
                    .unwrap_or_default()
                    .to_owned();
                Some(if name.is_empty() {
                    "Unknown generator".to_owned()
                } else {
                    name
                })
            }
        }
    }

    /// The insert a channel plays into (0, the master, when unset).
    fn channel_insert(&self, channel: &fl::Channel) -> u16 {
        channel
            .insert
            .filter(|i| self.fl.insert(*i).is_some())
            .unwrap_or(0)
    }

    fn plan_routing(&mut self) {
        // Each insert's main output: the master if it goes there at all,
        // else the first insert it feeds; other routes are sends.
        for insert in &self.fl.inserts {
            if insert.index == 0 {
                continue;
            }
            let outputs: Vec<u16> = insert
                .outputs()
                .into_iter()
                .filter(|o| self.fl.insert(*o).is_some())
                .collect();
            let main = if outputs.is_empty() || outputs.contains(&0) {
                0
            } else {
                outputs[0]
            };
            for other in outputs.iter().filter(|o| **o != main) {
                self.dropped_sends.push((insert.index, *other));
            }
            self.parents.insert(insert.index, main);
        }
        // Break loops: an insert whose chain comes back to itself goes to
        // the master.
        let indices: Vec<u16> = self.parents.keys().copied().collect();
        for index in indices {
            let mut seen = HashSet::from([index]);
            let mut current = self.parents[&index];
            while current != 0 {
                if !seen.insert(current) {
                    self.parents.insert(index, 0);
                    break;
                }
                current = self.parents.get(&current).copied().unwrap_or(0);
            }
        }
        // Direct tracks per insert, then which inserts carry anything.
        let mut direct: BTreeMap<u16, Vec<(u16, TrackRole)>> = BTreeMap::new();
        for channel in &self.fl.channels {
            for role in [TrackRole::Midi, TrackRole::Audio] {
                for _ in 0..self.lanes.get(&(channel.iid, role)).copied().unwrap_or(0) {
                    direct
                        .entry(self.channel_insert(channel))
                        .or_default()
                        .push((channel.iid, role));
                }
            }
        }
        self.used_inserts.insert(0);
        for &insert in direct.keys() {
            let mut current = insert;
            while self.used_inserts.insert(current) {
                current = self.parents.get(&current).copied().unwrap_or(0);
            }
        }
        for &insert in &self.used_inserts {
            if insert == 0 {
                continue;
            }
            let children = self
                .used_inserts
                .iter()
                .filter(|i| **i != 0 && self.parents.get(i) == Some(&insert))
                .count();
            if let Some([only]) = direct.get(&insert).map(Vec::as_slice)
                && children == 0
            {
                self.merged.insert(insert, *only);
            }
        }
    }

    fn insert_used(&self, index: u16) -> bool {
        self.used_inserts.contains(&index)
    }

    /// The chain of groups a channel's tracks sit in, outermost first.
    fn group_path(&self, channel: &fl::Channel) -> Vec<u16> {
        let mut path = Vec::new();
        let mut current = self.channel_insert(channel);
        if self.merged.contains_key(&current) {
            current = self.parents.get(&current).copied().unwrap_or(0);
        }
        while current != 0 && path.len() < 128 {
            path.push(current);
            current = self.parents.get(&current).copied().unwrap_or(0);
        }
        path.reverse();
        path
    }
}

/// Puts each span on the first lane where it overlaps nothing; spans must
/// be sorted by start. Returns each span's lane.
fn assign_lanes(spans: &[(i64, i64)]) -> Vec<usize> {
    let mut ends: Vec<i64> = Vec::new();
    spans
        .iter()
        .map(|&(start, length)| {
            let end = start + length.max(1);
            match ends.iter().position(|e| *e <= start) {
                Some(lane) => {
                    ends[lane] = end;
                    lane
                }
                None => {
                    ends.push(end);
                    ends.len() - 1
                }
            }
        })
        .collect()
}

// Building.

#[derive(Debug, Default)]
struct Report {
    missing_samples: BTreeSet<String>,
    no_sfz: BTreeSet<String>,
    generated_sfz: usize,
    default_effects: usize,
    converted_effects: usize,
    partial_effects: usize,
    vital_presets: usize,
    vital_dropped: BTreeSet<String>,
    master_effect_automation: usize,
    skipped_instruments: BTreeSet<String>,
    unsupported_automation: BTreeSet<String>,
    tempo_automation: usize,
    master_automation: usize,
    unlinked_automation: usize,
    clashing_automation: BTreeSet<String>,
    fine_pitch: usize,
    slides: usize,
    out_of_range: usize,
    note_pan: usize,
    sends: Vec<String>,
    sidechains: Vec<String>,
    layers: BTreeSet<String>,
    peak_controllers: BTreeSet<String>,
    /// Third-party plugins brought in as themselves.
    hosted: usize,
    /// Plugins chosen as themselves that aren't installed here.
    not_installed: BTreeSet<String>,
    /// Plugins chosen as themselves whose saved state didn't read.
    unread_states: BTreeSet<String>,
    lines: Vec<String>,
}

impl Report {
    fn lines(mut self) -> Vec<String> {
        let mut out = std::mem::take(&mut self.lines);
        let list = |set: &BTreeSet<String>| {
            let mut names: Vec<&str> = set.iter().map(String::as_str).take(4).collect();
            if set.len() > 4 {
                names.push("…");
            }
            names.join(", ")
        };
        if !self.missing_samples.is_empty() {
            out.push(format!(
                "Missing samples ({}): {}",
                self.missing_samples.len(),
                list(&self.missing_samples)
            ));
        }
        if self.generated_sfz > 0 {
            out.push(format!(
                "Sampler channels play through generated SFZ files ({})",
                self.generated_sfz
            ));
        }
        if !self.no_sfz.is_empty() {
            out.push(format!(
                "SFZ Samplers without a file: {}",
                list(&self.no_sfz)
            ));
        }
        if self.hosted > 0 {
            out.push(format!(
                "Plugins brought in as themselves, with their settings: {}",
                self.hosted
            ));
        }
        if !self.not_installed.is_empty() {
            out.push(format!(
                "Not installed here, so built-ins stand in: {}",
                list(&self.not_installed)
            ));
        }
        if !self.unread_states.is_empty() {
            out.push(format!(
                "Saved settings that couldn't be read, so built-ins stand in: {}",
                list(&self.unread_states)
            ));
        }
        if self.vital_presets > 0 {
            out.push(format!(
                "Vital presets rebuilt on the Wavetable synth: {}",
                self.vital_presets
            ));
        }
        if !self.vital_dropped.is_empty() {
            out.push(format!(
                "Not carried over from Vital: {}",
                list(&self.vital_dropped)
            ));
        }
        if !self.skipped_instruments.is_empty() {
            out.push(format!(
                "Skipped channels: {}",
                list(&self.skipped_instruments)
            ));
        }
        if self.converted_effects + self.partial_effects > 0 {
            let mut line = format!(
                "Effect settings carried over for {} effects",
                self.converted_effects + self.partial_effects
            );
            if self.partial_effects > 0 {
                line.push_str(&format!(" ({} in part)", self.partial_effects));
            }
            out.push(line);
        }
        if self.default_effects > 0 {
            out.push(format!(
                "Effect settings not carried over: {} effects start at their defaults",
                self.default_effects
            ));
        }
        if self.master_effect_automation > 0 {
            out.push(format!(
                "Automation of the master's effects not imported ({} clips)",
                self.master_effect_automation
            ));
        }
        if !self.peak_controllers.is_empty() {
            out.push(format!(
                "Peak Controllers with no linked target, skipped: {}",
                list(&self.peak_controllers)
            ));
        }
        if self.tempo_automation > 0 {
            out.push("Tempo automation not imported: canopy has one tempo".to_owned());
        }
        if self.master_automation > 0 {
            out.push("Master fader automation not imported".to_owned());
        }
        if !self.unsupported_automation.is_empty() {
            out.push(format!(
                "Plugin parameter automation not imported ({} clips): {}",
                self.unsupported_automation.len(),
                list(&self.unsupported_automation)
            ));
        }
        if !self.clashing_automation.is_empty() {
            out.push(format!(
                "Automation of a fader another clip already drives, skipped: {}",
                list(&self.clashing_automation)
            ));
        }
        if self.unlinked_automation > 0 {
            out.push(format!(
                "{} automation clips aren't linked to anything",
                self.unlinked_automation
            ));
        }
        if !self.sends.is_empty() {
            let mut shown: Vec<&str> = self.sends.iter().map(String::as_str).take(3).collect();
            if self.sends.len() > 3 {
                shown.push("…");
            }
            out.push(format!(
                "Sends not imported ({}): {}",
                self.sends.len(),
                shown.join(", ")
            ));
        }
        if !self.sidechains.is_empty() {
            let mut shown: Vec<&str> = self.sidechains.iter().map(String::as_str).take(3).collect();
            if self.sidechains.len() > 3 {
                shown.push("…");
            }
            out.push(format!(
                "Sidechains carried over ({}): {}",
                self.sidechains.len(),
                shown.join(", ")
            ));
        }
        if !self.layers.is_empty() {
            out.push(format!(
                "Layers play through their channels: {}",
                list(&self.layers)
            ));
        }
        if self.slides > 0 {
            out.push(format!("{} slide notes play as plain notes", self.slides));
        }
        if self.fine_pitch > 0 {
            out.push(format!("Fine pitch ignored on {} notes", self.fine_pitch));
        }
        if self.note_pan > 0 {
            out.push(format!("Note panning ignored on {} notes", self.note_pan));
        }
        if self.out_of_range > 0 {
            out.push(format!(
                "{} notes above MIDI's range skipped",
                self.out_of_range
            ));
        }
        out
    }
}

struct Builder<'a> {
    fl: &'a fl::Project,
    layout: &'a Layout<'a>,
    choices: &'a Choices,
    files: &'a Files,
    options: &'a Options,
    p: project::Project,
    report: Report,
    generated: Vec<GeneratedFile>,
    /// Each channel's tracks, by role, one per lane.
    channel_tracks: HashMap<(u16, TrackRole), Vec<TrackId>>,
    /// The track an insert's fader and effects are on: its group, or the
    /// track it was folded into. The master's isn't here.
    insert_tracks: HashMap<u16, TrackId>,
    /// Inserts folded into a channel's track, by that track.
    merged_into: HashMap<TrackId, u16>,
    /// What each mixer slot became: by (insert, slot), the device, the
    /// canopy effect, the FL plugin, and the dry/wet it got.
    slot_devices: HashMap<(u16, u8), SlotDevice<'a>>,
}

/// An FL effect, made a canopy device.
#[derive(Debug, Clone, Copy)]
struct SlotDevice<'a> {
    device: DeviceId,
    effect: &'static str,
    plugin: &'a fl::Plugin,
    /// Its `mix` as set, for effects that have one.
    mix: Option<f32>,
}

impl<'a> Builder<'a> {
    fn build(&mut self) {
        let fl = self.fl;
        if fl.truncated {
            self.report
                .lines
                .push("The file is cut short; imported what was there".to_owned());
        }
        if fl.format != 0 {
            self.report
                .lines
                .push("Not a full project (a preset or score)".to_owned());
        }
        self.p.set_tempo_bpm(fl.tempo.unwrap_or(120.0));
        if let Some((numerator, denominator)) = fl.time_signature {
            self.p.tempo.time_signature = TimeSignature {
                numerator,
                denominator,
            };
        }
        self.note_arrangements();
        self.make_tracks();
        self.master();
        self.effects();
        self.sidechains();
        self.midi_clips();
        self.audio_clips();
        self.automation();
        let used = self
            .layout
            .dropped_sends
            .iter()
            .filter(|(from, _)| self.layout.insert_used(*from));
        for (from, to) in used {
            let name = |i: u16| {
                fl.insert(i)
                    .map_or_else(String::new, fl::Insert::display_name)
            };
            self.report
                .sends
                .push(format!("{} to {}", name(*from), name(*to)));
        }
    }

    fn note_arrangements(&mut self) {
        let fl = self.fl;
        let main = self.layout.arrangement.map(|a| a.iid);
        let others: Vec<String> = fl
            .arrangements
            .iter()
            .filter(|a| Some(a.iid) != main && !a.items.is_empty())
            .map(|a| a.name.clone().unwrap_or_else(|| format!("{}", a.iid + 1)))
            .collect();
        if !others.is_empty() {
            self.report
                .lines
                .push(format!("Other arrangements skipped: {}", others.join(", ")));
        }
        if let Some(arrangement) = self.layout.arrangement {
            let signatures = arrangement
                .markers
                .iter()
                .filter(|m| m.action == fl::Marker::TIME_SIGNATURE)
                .count();
            let others = arrangement.markers.len() - signatures;
            if others > 0 {
                self.report.lines.push(format!("{others} markers skipped"));
            }
            if signatures > 0 {
                self.report
                    .lines
                    .push(format!("{signatures} time signature changes skipped"));
            }
        }
        // What notes lose, counted once per pattern however often it plays.
        let placed: BTreeSet<u16> = self
            .layout
            .arrangement
            .into_iter()
            .flat_map(|a| &a.items)
            .filter_map(|i| match i.target {
                Some(fl::ItemTarget::Pattern(iid)) => Some(iid),
                _ => None,
            })
            .collect();
        let mut steps = 0;
        for pattern in fl.patterns.iter().filter(|p| placed.contains(&p.iid)) {
            if pattern.controller_events > 0 {
                steps += 1;
            }
            for note in &pattern.notes {
                if note.key > 127 {
                    self.report.out_of_range += 1;
                    continue;
                }
                self.report.slides += usize::from(note.is_slide());
                self.report.fine_pitch += usize::from(note.fine_pitch != 120);
                self.report.note_pan += usize::from(note.pan != 64);
            }
        }
        if steps > 0 {
            self.report
                .lines
                .push(format!("Step automation in {steps} patterns skipped"));
        }
    }

    /// Groups and tracks, in channel rack order, nested as the mixer routes
    /// them.
    fn make_tracks(&mut self) {
        let fl = self.fl;
        let layout = self.layout;
        for channel in &fl.channels {
            if layout.skipped.contains(&channel.iid) {
                self.report
                    .skipped_instruments
                    .insert(channel.display_name());
            }
            if channel.kind == fl::ChannelKind::Layer {
                self.report.layers.insert(channel.display_name());
            }
            let roles: Vec<(TrackRole, usize)> = [TrackRole::Midi, TrackRole::Audio]
                .into_iter()
                .filter_map(|role| Some((role, *layout.lanes.get(&(channel.iid, role))?)))
                .collect();
            if roles.is_empty() {
                continue;
            }
            let parent = self.group_for(layout.group_path(channel));
            let both = roles.len() > 1;
            for (role, lanes) in roles {
                let mut ids = Vec::new();
                for lane in 0..lanes {
                    let kind = match role {
                        TrackRole::Midi => NewTrack::Midi,
                        TrackRole::Audio => NewTrack::Audio,
                    };
                    let position = TrackPosition {
                        parent,
                        before: None,
                    };
                    let Ok(id) = self.p.add_track(kind, position) else {
                        continue;
                    };
                    let mut name = channel.display_name();
                    if both && role == TrackRole::Audio {
                        name = format!("{name} Audio");
                    }
                    if lane > 0 {
                        name = format!("{name} {}", lane + 1);
                    }
                    self.setup_channel_track(id, channel, role, &name);
                    ids.push(id);
                }
                self.channel_tracks.insert((channel.iid, role), ids);
            }
        }
    }

    /// The group track for the innermost insert of `path`, made (with its
    /// outer groups) the first time it's needed.
    fn group_for(&mut self, path: Vec<u16>) -> Option<TrackId> {
        let mut parent = None;
        for insert in path {
            parent = Some(match self.insert_tracks.get(&insert) {
                Some(id) => *id,
                None => {
                    let position = TrackPosition {
                        parent,
                        before: None,
                    };
                    let Ok(id) = self.p.add_track(NewTrack::Group, position) else {
                        return parent;
                    };
                    if let Some(fl_insert) = self.fl.insert(insert) {
                        let _ = self.p.rename_track(id, &fl_insert.display_name());
                        if let Some(color) = fl_insert.color {
                            let _ = self.p.set_track_color(id, color_of(color));
                        }
                        let _ = self.p.set_mixer(id, insert_mixer(fl_insert));
                    }
                    self.insert_tracks.insert(insert, id);
                    id
                }
            });
        }
        parent
    }

    fn setup_channel_track(
        &mut self,
        id: TrackId,
        channel: &fl::Channel,
        role: TrackRole,
        name: &str,
    ) {
        let _ = self.p.rename_track(id, name);
        if let Some(color) = channel.color {
            let _ = self.p.set_track_color(id, color_of(color));
        }
        let mut mixer = channel_mixer(channel);
        let insert = self.layout.channel_insert(channel);
        if let Some((merged_channel, merged_role)) = self.layout.merged.get(&insert)
            && *merged_channel == channel.iid
            && *merged_role == role
            && let Some(fl_insert) = self.fl.insert(insert)
        {
            let own = insert_mixer(fl_insert);
            mixer.volume_db = clamp_db(mixer.volume_db + own.volume_db);
            mixer.pan = (mixer.pan + own.pan).clamp(-1.0, 1.0);
            mixer.mute |= own.mute;
            self.insert_tracks.insert(insert, id);
            self.merged_into.insert(id, insert);
        }
        let _ = self.p.set_mixer(id, mixer);
        if role == TrackRole::Midi {
            self.setup_instrument(id, channel);
        }
    }

    fn setup_instrument(&mut self, id: TrackId, channel: &fl::Channel) {
        let Some(name) = self.layout.instrument_name(channel) else {
            return;
        };
        let instrument = match self.choices.instrument(&name) {
            Choice::Builtin(instrument) => instrument,
            Choice::Plugin => {
                if let Some((plugin, data)) = channel.plugin.as_ref().and_then(|p| self.hosted(p)) {
                    let reference = InstrumentRef {
                        format: plugin.format.project(),
                        id: plugin.id,
                        version: String::new(),
                    };
                    let _ = self.p.set_instrument(id, reference);
                    let _ = self.p.set_instrument_plugin(id, Some(data));
                    return;
                }
                match plugins::suggest_instrument(&name).choice {
                    Choice::Builtin(instrument) => instrument,
                    _ => return,
                }
            }
            Choice::Skip => return,
        };
        let _ = self
            .p
            .set_instrument(id, InstrumentRef::builtin(instrument));
        if instrument == plugins::WAVETABLE
            && let Some(preset) = channel
                .plugin
                .as_ref()
                .and_then(|p| crate::vital::Preset::from_state(&p.state))
        {
            self.vital(id, channel, &preset);
            return;
        }
        if instrument != plugins::SAMPLER {
            return;
        }
        // An SFZ the plugin was playing, or one made for the FL Sampler's
        // sample.
        let sfz = channel
            .plugin
            .as_ref()
            .and_then(|p| crate::files::sfz_path_in(&p.state))
            .and_then(|raw| self.files.get(&raw))
            .map(|found| found.path.clone());
        let file = match sfz {
            Some(path) => Some(path),
            None => self.sampler_sfz(channel),
        };
        match file {
            Some(path) => {
                let _ = self.p.set_instrument_file(id, Some(path));
            }
            None => {
                // A missing sample is reported as such.
                let missing = channel
                    .sample_path
                    .as_ref()
                    .is_some_and(|raw| self.files.get(raw).is_none());
                if !missing {
                    self.report.no_sfz.insert(channel.display_name());
                }
            }
        }
    }

    /// The third-party plugin FL used, as the project keeps it, if it's
    /// installed here and its state reads; otherwise noted for the report
    /// (a built-in stands in).
    fn hosted(&mut self, plugin: &fl::Plugin) -> Option<(fl::PluginId, PluginData)> {
        let name = plugin.display_name();
        let vst = plugin.vst.as_ref();
        let Some(id) = vst
            .and_then(|v| v.plugin.as_ref())
            .filter(|id| self.options.installed.contains(id))
        else {
            self.report.not_installed.insert(name);
            return None;
        };
        let Some(state) = crate::vst::hosted_state(plugin) else {
            self.report.unread_states.insert(name);
            return None;
        };
        let vst = vst.expect("an identity comes with the wrapper's");
        self.report.hosted += 1;
        let data = PluginData {
            name: if vst.name.is_empty() {
                name
            } else {
                vst.name.clone()
            },
            vendor: vst.vendor.clone(),
            path: (!vst.path.is_empty()).then(|| PathBuf::from(&vst.path)),
            state: Blob::from(state),
        };
        Some((id.clone(), data))
    }

    /// A Vital channel on the Wavetable synth: its settings, its wavetables
    /// (written for the synth's user tables) and its own effects, ahead of
    /// the mixer's on the track.
    fn vital(&mut self, id: TrackId, channel: &fl::Channel, preset: &crate::vital::Preset) {
        let converted = crate::vital::convert(preset);
        let _ = self.p.set_instrument_params(
            id,
            converted
                .params
                .iter()
                .map(|(param, value)| (param.clone(), *value))
                .collect(),
        );
        let name = channel.display_name();
        let mut tables = Vec::new();
        for (osc, table) in converted.tables.iter().enumerate() {
            let Some(table) = table else {
                tables.push(None);
                continue;
            };
            let Some(dir) = self.options.sfz_dir.as_ref() else {
                tables.push(None);
                continue;
            };
            let path = dir.join(format!(
                "{} {} osc {}.wav",
                file_safe(&name),
                channel.iid,
                ["A", "B", "C"][osc]
            ));
            self.generated.push(GeneratedFile {
                path: path.clone(),
                contents: crate::vital::wav_bytes(table),
            });
            tables.push(Some(path));
        }
        self.set_wavetables(id, tables);
        for (effect, params) in &converted.effects {
            let Ok(device) = self
                .p
                .add_device(Some(id), EffectRef::builtin(effect), None)
            else {
                continue;
            };
            for (param, value) in params {
                let _ = self.p.set_device_param(device, param, *value);
            }
        }
        self.report.vital_presets += 1;
        if !converted.dropped.is_empty() {
            self.report
                .vital_dropped
                .insert(format!("{name}: {}", converted.dropped.join(", ")));
        }
    }

    /// Hands a Wavetable track its oscillators' user tables.
    fn set_wavetables(&mut self, id: TrackId, tables: Vec<Option<PathBuf>>) {
        for (osc, path) in tables.into_iter().enumerate() {
            if path.is_some() {
                let _ = self.p.set_wavetable_file(id, osc, path);
            }
        }
    }

    /// A one-region SFZ for an FL Sampler's sample, for the caller to write.
    fn sampler_sfz(&mut self, channel: &fl::Channel) -> Option<PathBuf> {
        if channel.kind != fl::ChannelKind::Sampler {
            return None;
        }
        let raw = channel.sample_path.as_ref()?;
        let Some(found) = self.files.get(raw) else {
            self.report.missing_samples.insert(file_name(raw));
            return None;
        };
        let dir = self.options.sfz_dir.as_ref()?;
        let path = dir.join(format!(
            "{} {}.sfz",
            file_safe(&channel.display_name()),
            channel.iid
        ));
        if !self.generated.iter().any(|f| f.path == path) {
            // Step-sequencer notes play the whole sample in FL.
            let steps = self.fl.patterns.iter().any(|p| {
                p.notes
                    .iter()
                    .any(|n| n.length == 0 && self.plays_on(n.channel, channel.iid))
            });
            let mode = if steps { " loop_mode=one_shot" } else { "" };
            let root = channel.root_note.unwrap_or(60);
            let tune = match channel.pitch_cents.filter(|c| *c != 0) {
                Some(cents) => format!(" tune={}", cents.clamp(-9600, 9600)),
                None => String::new(),
            };
            let contents = format!(
                "// {} (FL Studio sampler channel)\n<region> pitch_keycenter={root}{tune}{mode}\nsample={}\n",
                channel.display_name(),
                found.path.display()
            );
            self.generated.push(GeneratedFile {
                path: path.clone(),
                contents: contents.into_bytes(),
            });
            self.report.generated_sfz += 1;
        }
        Some(path)
    }

    fn plays_on(&self, note_channel: u16, channel: u16) -> bool {
        self.layout
            .layer_targets
            .get(&note_channel)
            .is_some_and(|t| t.contains(&channel))
    }

    fn master(&mut self) {
        if let Some(master) = self.fl.insert(0) {
            self.p.master = insert_mixer(master);
            self.p.master.mute = false;
        }
    }

    /// Each used insert's effects onto its track or group (the master's
    /// onto the master).
    fn effects(&mut self) {
        let fl = self.fl;
        // Plugin parameters automation moves, by (insert, slot).
        let automated: HashSet<(u16, u8, u16)> = fl
            .automation_links
            .iter()
            .filter_map(|link| match link.target {
                fl::AutomationTarget::Insert {
                    insert,
                    slot: Some(slot),
                    param,
                } if param & 0x8000 != 0 => Some((insert, slot, param & 0x7FFF)),
                _ => None,
            })
            .collect();
        for insert in &fl.inserts {
            if !self.layout.insert_used(insert.index) {
                continue;
            }
            let target = if insert.index == 0 {
                None
            } else {
                match self.insert_tracks.get(&insert.index) {
                    Some(id) => Some(*id),
                    None => continue,
                }
            };
            for slot in &insert.slots {
                let name = slot.plugin.display_name();
                if name.is_empty() {
                    continue;
                }
                let bypassed = slot.enabled == Some(false) || !insert.effects_on();
                let effect = match self.choices.effect(&name) {
                    Choice::Builtin(effect) => effect,
                    Choice::Plugin => {
                        if let Some((plugin, data)) = self.hosted(&slot.plugin) {
                            let reference = EffectRef {
                                format: plugin.format.project(),
                                id: plugin.id,
                                version: String::new(),
                            };
                            if let Ok(device) = self.p.add_device(target, reference, None) {
                                let _ = self.p.set_device_plugin(device, Some(data));
                                let _ = self.p.set_device_bypassed(device, bypassed);
                            }
                            continue;
                        }
                        match plugins::suggest_effect(&name).choice {
                            Choice::Builtin(effect) => effect,
                            _ => continue,
                        }
                    }
                    Choice::Skip => continue,
                };
                if effect == plugins::DUCKER && plugins::is_peak_controller(&name) {
                    self.peak_controller(insert, slot, bypassed);
                    continue;
                }
                let Ok(device) = self.p.add_device(target, EffectRef::builtin(effect), None) else {
                    continue;
                };
                if bypassed {
                    let _ = self.p.set_device_bypassed(device, true);
                }
                if effect == plugins::DUCKER && plugins::is_tempo_ducker(&name) {
                    // Pump on every beat, like Kickstart.
                    let _ = self.p.set_device_param(device, "mode", 2.0);
                }
                let index = insert.index;
                let is_automated = |param: u16| automated.contains(&(index, slot.index, param));
                let settings =
                    convert::effect_settings(&slot.plugin, effect, slot.mix, &is_automated);
                for (id, value) in &settings.params {
                    let _ = self.p.set_device_param(device, id, *value);
                }
                match settings.fidelity {
                    Fidelity::Full => self.report.converted_effects += 1,
                    Fidelity::Partial => self.report.partial_effects += 1,
                    Fidelity::None => self.report.default_effects += 1,
                }
                let mix = self
                    .p
                    .device(device)
                    .and_then(|d| d.params.get("mix").copied());
                self.slot_devices.insert(
                    (insert.index, slot.index),
                    SlotDevice {
                        device,
                        effect,
                        plugin: &slot.plugin,
                        mix,
                    },
                );
            }
        }
    }

    /// FL sidechains an insert into another by routing it there at level 0;
    /// the first compressor or ducker there listens to it, as FL's do when
    /// told to (which one isn't in the file).
    fn sidechains(&mut self) {
        let fl = self.fl;
        for insert in &fl.inserts {
            if !self.layout.insert_used(insert.index) {
                continue;
            }
            let Some(&source) = self.insert_tracks.get(&insert.index) else {
                continue;
            };
            for target in insert.sidechains() {
                if !self.layout.insert_used(target) {
                    continue;
                }
                let mut slots: Vec<(u8, SlotDevice)> = self
                    .slot_devices
                    .iter()
                    .filter(|((i, _), _)| *i == target)
                    .map(|((_, slot), device)| (*slot, *device))
                    .collect();
                slots.sort_by_key(|(slot, _)| *slot);
                let listener = slots.into_iter().map(|(_, d)| d).find(|d| {
                    matches!(d.effect, plugins::COMPRESSOR | plugins::DUCKER)
                        && self
                            .p
                            .device(d.device)
                            .is_some_and(|device| device.sidechain.is_none())
                });
                let Some(listener) = listener else {
                    continue;
                };
                if self
                    .p
                    .set_device_sidechain(listener.device, Some(source))
                    .is_ok()
                {
                    let name = |i: u16| {
                        fl.insert(i)
                            .map_or_else(String::new, fl::Insert::display_name)
                    };
                    self.report.sidechains.push(format!(
                        "{} into {}",
                        name(insert.index),
                        name(target)
                    ));
                }
            }
        }
    }

    /// FL's Peak Controller sits on the insert that triggers it and drives
    /// other things' levels; in canopy that's a Ducker on each of those,
    /// listening to this insert.
    fn peak_controller(&mut self, insert: &fl::Insert, slot: &fl::Slot, bypassed: bool) {
        let source = self.insert_tracks.get(&insert.index).copied();
        let location = self.fl.mixer_base + (insert.index << 6) + u16::from(slot.index);
        let mut targets: Vec<Option<TrackId>> = Vec::new();
        for link in &self.fl.automation_links {
            if link.channel != location {
                continue;
            }
            match link.target {
                fl::AutomationTarget::Channel { channel, .. } => {
                    for role in [TrackRole::Midi, TrackRole::Audio] {
                        if let Some(ids) = self.channel_tracks.get(&(channel, role)) {
                            targets.extend(ids.iter().map(|id| Some(*id)));
                        }
                    }
                }
                fl::AutomationTarget::Insert { insert, .. } => {
                    if insert == 0 {
                        targets.push(None);
                    } else if let Some(id) = self.insert_tracks.get(&insert) {
                        targets.push(Some(*id));
                    }
                }
                _ => {}
            }
        }
        let mut seen = HashSet::new();
        targets.retain(|t| seen.insert(*t) && *t != source);
        if targets.is_empty() {
            self.report.peak_controllers.insert(insert.display_name());
            return;
        }
        for target in targets {
            let Ok(device) = self
                .p
                .add_device(target, EffectRef::builtin(plugins::DUCKER), None)
            else {
                continue;
            };
            let _ = self.p.set_device_sidechain(device, source);
            if bypassed {
                let _ = self.p.set_device_bypassed(device, true);
            }
            self.report.default_effects += 1;
        }
    }

    fn midi_clips(&mut self) {
        let fl = self.fl;
        let layout = self.layout;
        for (&channel, clips) in &layout.midi_clips {
            let Some(tracks) = self
                .channel_tracks
                .get(&(channel, TrackRole::Midi))
                .cloned()
            else {
                continue;
            };
            for (lane, plan) in clips {
                let Some(&track) = tracks.get(*lane) else {
                    continue;
                };
                let Some(pattern) = fl.pattern(plan.pattern) else {
                    continue;
                };
                let notes = self.notes(pattern, channel);
                let Ok(clip) =
                    self.p
                        .add_midi_clip(track, Tick::new(plan.start), Tick::new(plan.length))
                else {
                    continue;
                };
                let _ = self.p.rename_clip(clip, &pattern.display_name());
                let _ = self.p.set_notes(clip, notes);
                if plan.muted {
                    let _ = self.p.set_clips_muted(&[clip], true);
                }
                if plan.offset != 0 {
                    set_midi_offset(&mut self.p, track, clip, Tick::new(plan.offset));
                }
            }
        }
    }

    /// A pattern's notes for one channel, in canopy's units.
    fn notes(&mut self, pattern: &fl::Pattern, channel: u16) -> Vec<Note> {
        let layout = self.layout;
        let mut notes = Vec::new();
        for note in &pattern.notes {
            if !self.plays_on(note.channel, channel) {
                continue;
            }
            let Some(pitch) = u8::try_from(note.key).ok().and_then(Pitch::new) else {
                continue;
            };
            let len = match note.length {
                // Step-sequencer notes: a sixteenth.
                0 => PPQ / 4,
                n => layout.ticks(f64::from(n)).max(1),
            };
            notes.push(Note {
                id: self.p.new_note_id(),
                start: Tick::new(layout.ticks(f64::from(note.position))),
                len: Tick::new(len),
                pitch,
                velocity: velocity(note.velocity),
                release_velocity: velocity(note.release),
                muted: false,
            });
        }
        notes
    }

    fn audio_clips(&mut self) {
        let fl = self.fl;
        let layout = self.layout;
        for (&iid, clips) in &layout.audio_clips {
            let Some(channel) = fl.channel(iid) else {
                continue;
            };
            let Some(tracks) = self.channel_tracks.get(&(iid, TrackRole::Audio)).cloned() else {
                continue;
            };
            let Some(raw) = &channel.sample_path else {
                continue;
            };
            let found = self
                .files
                .get(raw)
                .and_then(|f| Some((f.path.clone(), f.audio?)));
            let Some((path, audio)) = found else {
                self.report.missing_samples.insert(file_name(raw));
                continue;
            };
            if audio.frames == 0 || audio.sample_rate == 0 {
                continue;
            }
            let source = self
                .p
                .add_source(path, audio.sample_rate, audio.frames, audio.channels);
            for (lane, plan) in clips {
                let Some(&track) = tracks.get(*lane) else {
                    continue;
                };
                let rate = audio.sample_rate;
                let tempo = self.p.tempo;
                let offset_ticks = plan.offset_quarters * PPQ as f64;
                let offset = (tempo.ticks_to_frames(offset_ticks, rate).round() as u64)
                    .min(audio.frames - 1);
                let wanted = tempo.ticks_to_frames(plan.length as f64, rate).floor() as u64;
                let length = wanted.clamp(1, audio.frames - offset);
                let Ok(clip) = self.p.add_audio_clip(track, Tick::new(plan.start), source) else {
                    continue;
                };
                let muted = plan.muted;
                let _ = self.p.edit_audio_clip(clip, |c| {
                    c.source_offset = offset;
                    c.source_len = length;
                    c.play_len = length;
                    c.muted = muted;
                });
            }
        }
    }

    fn automation(&mut self) {
        let fl = self.fl;
        let Some(arrangement) = self.layout.arrangement else {
            return;
        };
        // (track, lane) → what drives it, how its values convert, and the
        // points from every placement, in order.
        let mut lanes: BTreeMap<
            (TrackId, LaneKind),
            (fl::AutomationTarget, LaneValue, Vec<AutomationPoint>),
        > = BTreeMap::new();
        for channel in &fl.channels {
            if channel.kind != fl::ChannelKind::Automation {
                continue;
            }
            let Some(clip) = &channel.automation else {
                continue;
            };
            let mut placements: Vec<&fl::PlaylistItem> = arrangement
                .items
                .iter()
                .filter(|i| i.target == Some(fl::ItemTarget::Channel(channel.iid)))
                .collect();
            if placements.is_empty() || clip.points.is_empty() {
                continue;
            }
            placements.sort_by_key(|i| i.position);
            let links: Vec<fl::AutomationTarget> = fl
                .automation_links
                .iter()
                .filter(|l| l.channel == channel.iid)
                .map(|l| l.target)
                .collect();
            if links.is_empty() {
                self.report.unlinked_automation += 1;
                continue;
            }
            for target in links {
                let Some(destinations) = self.lane_destinations(&target, channel) else {
                    continue;
                };
                for (track, kind, value) in destinations {
                    let (source, _, points) = lanes
                        .entry((track, kind))
                        .or_insert_with(|| (target, value, Vec::new()));
                    if *source != target {
                        // A folded track's channel and insert faders, both
                        // automated: canopy has one fader.
                        self.report
                            .clashing_automation
                            .insert(channel.display_name());
                        continue;
                    }
                    for item in &placements {
                        self.place_points(points, clip, item, &|raw| value.apply(raw, &target));
                    }
                }
            }
        }
        for ((track, kind), (_, _, mut points)) in lanes {
            points.sort_by_key(|p| p.tick);
            let target = match kind {
                LaneKind::Volume => AutomationTarget::Volume,
                LaneKind::Pan => AutomationTarget::Pan,
                LaneKind::Device(device, param) => AutomationTarget::DeviceParam { device, param },
                LaneKind::Instrument(param) => AutomationTarget::InstrumentParam(param),
            };
            let _ = self.p.set_automation(track, target, points);
        }
    }

    /// Which tracks' lanes an automation target turns into, and how its
    /// values convert, or `None` (reported) if canopy can't automate it.
    fn lane_destinations(
        &mut self,
        target: &fl::AutomationTarget,
        clip: &fl::Channel,
    ) -> Option<Vec<(TrackId, LaneKind, LaneValue)>> {
        let unsupported = |builder: &mut Self| {
            builder
                .report
                .unsupported_automation
                .insert(clip.display_name());
            None
        };
        match *target {
            fl::AutomationTarget::Global { param: 5 } => {
                self.report.tempo_automation += 1;
                None
            }
            fl::AutomationTarget::Global { .. } => {
                self.report.master_automation += 1;
                None
            }
            fl::AutomationTarget::Channel {
                channel,
                param,
                plugin: false,
            } if param <= 1 => {
                let kind = if param == 0 {
                    LaneKind::Volume
                } else {
                    LaneKind::Pan
                };
                let mut tracks = Vec::new();
                for &c in self
                    .layout
                    .layer_targets
                    .get(&channel)
                    .into_iter()
                    .flatten()
                {
                    for role in [TrackRole::Midi, TrackRole::Audio] {
                        for id in self.channel_tracks.get(&(c, role)).into_iter().flatten() {
                            let offset = self.static_offset(*id, target, &kind);
                            tracks.push((*id, kind.clone(), LaneValue::Fader { offset }));
                        }
                    }
                }
                (!tracks.is_empty()).then_some(tracks)
            }
            fl::AutomationTarget::Insert { insert, param, .. }
                if param >> 8 == 0x1F
                    && matches!((param & 0xFF) as u8, mixer_param::VOLUME | mixer_param::PAN) =>
            {
                let kind = if (param & 0xFF) as u8 == mixer_param::VOLUME {
                    LaneKind::Volume
                } else {
                    LaneKind::Pan
                };
                if insert == 0 {
                    self.report.master_automation += 1;
                    return None;
                }
                let track = *self.insert_tracks.get(&insert)?;
                let offset = self.static_offset(track, target, &kind);
                Some(vec![(track, kind, LaneValue::Fader { offset })])
            }
            fl::AutomationTarget::Channel {
                channel,
                plugin: true,
                ..
            } => {
                // A Vital knob, by the name FL gave the clip: "<channel> -
                // <parameter>".
                let Some(fl_channel) = self.fl.channel(channel) else {
                    return unsupported(self);
                };
                let vital = fl_channel
                    .plugin
                    .as_ref()
                    .is_some_and(|p| plugins::normalize(p.plugin_name()) == "vital");
                let name = clip.display_name();
                let name = without_copy_number(&name);
                let param = name
                    .strip_prefix(&fl_channel.display_name())
                    .and_then(|rest| rest.trim_start().strip_prefix('-'))
                    .unwrap_or(name);
                let mapped = vital.then(|| crate::vital::automation(param)).flatten();
                let Some((id, automation)) = mapped else {
                    return unsupported(self);
                };
                let tracks: Vec<TrackId> = self
                    .channel_tracks
                    .get(&(channel, TrackRole::Midi))
                    .cloned()
                    .unwrap_or_default();
                let mut out = Vec::new();
                for track in tracks {
                    let wavetable = self.p.track(track).is_some_and(|t| {
                        matches!(&t.kind, project::TrackKind::Midi { instrument, .. }
                            if instrument.id == plugins::WAVETABLE)
                    });
                    let descriptor = instruments::find_builtin(plugins::WAVETABLE)
                        .and_then(|d| d.params.iter().find(|p| p.id == id));
                    if let (true, Some(descriptor)) = (wavetable, descriptor) {
                        let value = LaneValue::Vital {
                            automation,
                            descriptor,
                        };
                        out.push((track, LaneKind::Instrument(id.clone()), value));
                    }
                }
                if out.is_empty() {
                    return unsupported(self);
                }
                Some(out)
            }
            fl::AutomationTarget::Insert {
                insert,
                slot: Some(slot),
                param,
            } => {
                let Some(device) = self.slot_devices.get(&(insert, slot)).copied() else {
                    return unsupported(self);
                };
                let own_mix = param >> 8 == u16::from(mixer_param::OWN)
                    && (param & 0xFF) as u8 == mixer_param::SLOT_MIX;
                let mapped = if own_mix {
                    convert::slot_mix_automation(device.effect, device.mix)
                } else if param & 0x8000 != 0 {
                    convert::plugin_automation(device.plugin, device.effect, param & 0x7FFF)
                        .or_else(|| {
                            // Any plugin's dry/wet, by the name FL gave the
                            // clip: "<plugin> - <insert> - <parameter>".
                            let name = clip.display_name();
                            let param = without_copy_number(&name).rsplit(" - ").next();
                            let param = param.unwrap_or("");
                            convert::is_mix_name(param)
                                .then(|| convert::slot_mix_automation(device.effect, Some(100.0)))
                                .flatten()
                        })
                } else {
                    None
                };
                let Some((id, conversion)) = mapped else {
                    return unsupported(self);
                };
                if insert == 0 {
                    self.report.master_effect_automation += 1;
                    return None;
                }
                let track = *self.insert_tracks.get(&insert)?;
                let descriptor = EffectRef::builtin(device.effect)
                    .descriptor()?
                    .params
                    .iter()
                    .find(|p| p.id == id)?;
                let value = LaneValue::Param {
                    conversion,
                    descriptor,
                };
                Some(vec![(track, LaneKind::Device(device.device, id), value)])
            }
            _ => unsupported(self),
        }
    }

    /// The fader setting a lane must keep on top of the automated one, for
    /// tracks that carry both a channel's and an insert's fader.
    fn static_offset(&self, track: TrackId, target: &fl::AutomationTarget, kind: &LaneKind) -> f32 {
        let Some(insert) = self
            .merged_into
            .get(&track)
            .and_then(|i| self.fl.insert(*i))
        else {
            return 0.0;
        };
        let channel = self
            .layout
            .merged
            .get(&insert.index)
            .and_then(|(c, _)| self.fl.channel(*c));
        let from_insert = matches!(target, fl::AutomationTarget::Insert { .. });
        match (kind, from_insert) {
            // The channel's fader stays while the insert's moves…
            (LaneKind::Volume, true) => channel.map_or(0.0, |c| channel_mixer(c).volume_db),
            (LaneKind::Pan, true) => channel.map_or(0.0, |c| channel_mixer(c).pan),
            // …and the other way round.
            (LaneKind::Volume, false) => insert_mixer(insert).volume_db,
            (LaneKind::Pan, false) => insert_mixer(insert).pan,
            (LaneKind::Device(..) | LaneKind::Instrument(_), _) => 0.0,
        }
    }

    /// One placement of an automation clip: the points inside the item's
    /// window, plus its edges, holding the value in between placements.
    fn place_points(
        &self,
        points: &mut Vec<AutomationPoint>,
        clip: &fl::AutomationClip,
        item: &fl::PlaylistItem,
        value: &dyn Fn(f64) -> f32,
    ) {
        let ppq = f64::from(self.fl.ppq.max(1));
        let start = item.start_offset.unwrap_or(0.0);
        let end = start + f64::from(item.length) / ppq;
        let at_tick = |quarters: f64| {
            let fl_ticks = f64::from(item.position) + (quarters - start) * ppq;
            Tick::new(self.layout.ticks(fl_ticks))
        };
        let first_tick = at_tick(start);
        // Hold the last value up to here.
        if let Some(last) = points.last().copied()
            && last.tick < first_tick
        {
            points.push(AutomationPoint {
                tick: first_tick,
                value: last.value,
                curve: 0.0,
            });
        }
        points.push(AutomationPoint {
            tick: first_tick,
            value: value(value_at(clip, start)),
            curve: 0.0,
        });
        let inside: Vec<(usize, &fl::AutomationPoint)> = clip
            .points
            .iter()
            .enumerate()
            .filter(|(_, p)| p.position > start && p.position < end)
            .collect();
        for (index, point) in inside {
            // FL keeps a segment's bend on the point it leads to.
            if let Some(previous) = points.last_mut()
                && index > 0
                && previous.tick < at_tick(point.position)
            {
                previous.curve = point.tension.clamp(-1.0, 1.0);
            }
            points.push(AutomationPoint {
                tick: at_tick(point.position),
                value: value(point.value),
                curve: 0.0,
            });
        }
        points.push(AutomationPoint {
            tick: at_tick(end),
            value: value(value_at(clip, end)),
            curve: 0.0,
        });
    }
}

/// What a lane drives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum LaneKind {
    Volume,
    Pan,
    /// An effect's parameter, by id.
    Device(DeviceId, String),
    /// The track's instrument's parameter, by id.
    Instrument(String),
}

/// How an FL automation value becomes a lane's value (`0..=1` on canopy's
/// scale for what it drives).
#[derive(Debug, Clone, Copy)]
enum LaneValue {
    /// A volume or pan, with `offset` (dB or pan) added for folded faders.
    Fader { offset: f32 },
    /// An effect parameter: converted to its units, then onto its scale.
    Param {
        conversion: Conversion,
        descriptor: &'static instruments::ParamDescriptor,
    },
    /// A Vital parameter onto the Wavetable synth's.
    Vital {
        automation: crate::vital::Automation,
        descriptor: &'static instruments::ParamDescriptor,
    },
}

impl LaneValue {
    fn apply(self, raw: f64, target: &fl::AutomationTarget) -> f32 {
        match self {
            LaneValue::Fader { offset } => {
                let kind = match target {
                    fl::AutomationTarget::Channel { param: 1, .. } => LaneKind::Pan,
                    fl::AutomationTarget::Insert { param, .. }
                        if (param & 0xFF) as u8 == mixer_param::PAN =>
                    {
                        LaneKind::Pan
                    }
                    _ => LaneKind::Volume,
                };
                lane_value(raw, &kind, target, offset)
            }
            LaneValue::Param {
                conversion,
                descriptor,
            } => descriptor.to_normalized(conversion.apply(raw)),
            LaneValue::Vital {
                automation,
                descriptor,
            } => descriptor.to_normalized(automation.apply(raw)),
        }
    }
}

/// An automation clip's value at `quarters`, straight between points and
/// held past the ends.
fn value_at(clip: &fl::AutomationClip, quarters: f64) -> f64 {
    let points = &clip.points;
    let after = points.partition_point(|p| p.position <= quarters);
    match (
        after.checked_sub(1).map(|i| points[i]),
        points.get(after).copied(),
    ) {
        (None, None) => 0.0,
        (Some(p), None) | (None, Some(p)) => p.value,
        (Some(a), Some(b)) => {
            let span = b.position - a.position;
            if span <= 0.0 {
                b.value
            } else {
                a.value + (b.value - a.value) * (quarters - a.position) / span
            }
        }
    }
}

/// An FL automation value (`0..=1` across the control's range) on canopy's
/// lane scale, with `offset` (dB or pan) added for folded faders.
fn lane_value(raw: f64, kind: &LaneKind, target: &fl::AutomationTarget, offset: f32) -> f32 {
    let raw = raw.clamp(0.0, 1.0);
    match kind {
        LaneKind::Volume => {
            let db = match target {
                fl::AutomationTarget::Insert { .. } => insert_volume_db(raw * 16_000.0),
                _ => channel_volume_db(raw * 12_800.0),
            };
            volume_to_normalized(clamp_db(db + offset))
        }
        LaneKind::Pan | LaneKind::Device(..) | LaneKind::Instrument(_) => {
            let pan = normalized_to_pan(raw as f32) + offset;
            pan_to_normalized(pan.clamp(-1.0, 1.0))
        }
    }
}

/// Moves a fresh MIDI clip's content window without moving the clip.
fn set_midi_offset(p: &mut project::Project, track: TrackId, clip: project::ClipId, offset: Tick) {
    if let Some(project::TrackKind::Midi { clips, .. }) = p.track_mut(track).map(|t| &mut t.kind)
        && let Some(clip) = clips.iter_mut().find(|c| c.id == clip)
    {
        clip.offset = offset.max(Tick::ZERO);
    }
}

fn color_of(rgb: fl::Rgb) -> Color {
    Color::rgb(rgb.r, rgb.g, rgb.b)
}

fn channel_mixer(channel: &fl::Channel) -> Mixer {
    let volume = channel
        .volume
        .map_or(0.0, |v| channel_volume_db(f64::from(v)));
    let pan = channel
        .pan
        .map_or(0.0, |p| (p - FL_DEFAULT_CHANNEL_PAN) as f32 / 6400.0);
    Mixer {
        volume_db: clamp_db(volume),
        pan: pan.clamp(-1.0, 1.0),
        mute: !channel.enabled,
        solo: false,
    }
}

fn insert_mixer(insert: &fl::Insert) -> Mixer {
    let volume = insert
        .volume
        .map_or(0.0, |v| insert_volume_db(f64::from(v)));
    let pan = insert.pan.map_or(0.0, |p| p as f32 / 6400.0);
    Mixer {
        volume_db: clamp_db(volume),
        pan: pan.clamp(-1.0, 1.0),
        mute: !insert.enabled(),
        solo: false,
    }
}

/// The last part of a path, whichever way its slashes go.
fn file_name(raw: &str) -> String {
    raw.rsplit(['/', '\\'])
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or(raw)
        .to_owned()
}

/// `name` without characters file systems dislike.
fn file_safe(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim();
    if safe.is_empty() {
        "channel".to_owned()
    } else {
        safe.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_curves() {
        assert_eq!(channel_volume_db(10_000.0), 0.0);
        assert!((channel_volume_db(12_800.0) - 4.29).abs() < 0.01);
        assert_eq!(channel_volume_db(0.0), project::SILENCE_DB);
        assert!(insert_volume_db(12_800.0).abs() < 1e-4);
        assert!((insert_volume_db(16_000.0) - 5.59).abs() < 0.01);
        // FL's own readout at 0.59843 of the way up.
        assert!((insert_volume_db(0.59843 * 16_000.0) + 6.0).abs() < 0.01);
        assert!(insert_volume_db(6_400.0) < -12.0);
        assert_eq!(velocity(0), Velocity::MIN);
        assert_eq!(velocity(64), Velocity::CENTER);
        assert_eq!(velocity(128), Velocity::MAX);
        assert_eq!(velocity(200), Velocity::MAX);
        assert!(velocity(100) > velocity(99));
    }

    #[test]
    fn lanes_keep_overlaps_apart() {
        assert_eq!(
            assign_lanes(&[(0, 10), (10, 10), (5, 10), (20, 5)]),
            [0, 0, 1, 0]
        );
        assert_eq!(assign_lanes(&[(0, 10), (0, 10), (0, 10)]), [0, 1, 2]);
        assert!(assign_lanes(&[]).is_empty());
    }

    #[test]
    fn automation_values() {
        let clip = fl::AutomationClip {
            points: vec![
                fl::AutomationPoint {
                    position: 0.0,
                    value: 0.0,
                    tension: 0.0,
                },
                fl::AutomationPoint {
                    position: 4.0,
                    value: 1.0,
                    tension: 0.0,
                },
            ],
        };
        assert_eq!(value_at(&clip, -1.0), 0.0);
        assert_eq!(value_at(&clip, 2.0), 0.5);
        assert_eq!(value_at(&clip, 9.0), 1.0);
        let pan = fl::AutomationTarget::Channel {
            channel: 0,
            param: 1,
            plugin: false,
        };
        assert_eq!(lane_value(0.25, &LaneKind::Pan, &pan, 0.0), 0.25);
        let insert = fl::AutomationTarget::Insert {
            insert: 1,
            slot: Some(0),
            param: 0x1FC0,
        };
        let unity = lane_value(0.8, &LaneKind::Volume, &insert, 0.0);
        assert!((unity - volume_to_normalized(0.0)).abs() < 1e-4);
    }

    #[test]
    fn clip_copies_lose_their_number() {
        assert_eq!(
            without_copy_number("MSaturator - Bus - Dry/Wet #2"),
            "MSaturator - Bus - Dry/Wet"
        );
        assert_eq!(without_copy_number("Lead - Cutoff #12"), "Lead - Cutoff");
        assert_eq!(without_copy_number("Kick #b"), "Kick #b");
        assert_eq!(without_copy_number("Snare #"), "Snare #");
    }

    #[test]
    fn names_for_files() {
        assert_eq!(file_name("C:\\Samples\\Kick 1.wav"), "Kick 1.wav");
        assert_eq!(file_name("/a/b.wav"), "b.wav");
        assert_eq!(file_safe("Kick/Snare: 1"), "Kick_Snare_ 1");
        assert_eq!(file_safe("???"), "___");
        assert_eq!(file_safe("  "), "channel");
    }
}
