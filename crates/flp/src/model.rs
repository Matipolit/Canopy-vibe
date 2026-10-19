//! What an FL Studio project holds, as plainly as the file says it: FL's
//! own units (ticks at the file's PPQ, FL's volume and pan ranges), before
//! anything is converted for canopy.

/// An RGB color, as FL stores it (the alpha byte is ignored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// From FL's little-endian `0xAABBGGRR` dword.
    pub fn from_dword(value: u32) -> Rgb {
        let [r, g, b, _] = value.to_le_bytes();
        Rgb { r, g, b }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Project {
    /// `"20.8.4.2576"`, if the file says.
    pub version: Option<String>,
    /// 0 for a project; presets and scores use other values.
    pub format: i16,
    /// Ticks per quarter note.
    pub ppq: u16,
    /// The channel count the header claims.
    pub channel_count: u16,
    pub title: Option<String>,
    /// Beats per minute.
    pub tempo: Option<f64>,
    /// Numerator and denominator.
    pub time_signature: Option<(u8, u8)>,
    /// In the order they appear, which is the channel rack's.
    pub channels: Vec<Channel>,
    pub patterns: Vec<Pattern>,
    pub arrangements: Vec<Arrangement>,
    /// The arrangement open in FL when it was saved.
    pub current_arrangement: Option<u16>,
    /// Mixer tracks by number: 0 is the master.
    pub inserts: Vec<Insert>,
    /// What each automation clip controls.
    pub automation_links: Vec<AutomationLink>,
    /// Where mixer locations start in this file (see `ids::mixer`): the
    /// insert of a location is `(location - mixer_base) >> 6`.
    pub mixer_base: u16,
    /// The file stopped short; everything after the cut is missing.
    pub truncated: bool,
}

impl Project {
    pub fn channel(&self, iid: u16) -> Option<&Channel> {
        self.channels.iter().find(|c| c.iid == iid)
    }

    pub fn pattern(&self, iid: u16) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.iid == iid)
    }

    pub fn insert(&self, index: u16) -> Option<&Insert> {
        self.inserts.iter().find(|i| i.index == index)
    }

    /// The major version, if known.
    pub fn major_version(&self) -> Option<u32> {
        self.version.as_deref()?.split('.').next()?.parse().ok()
    }

    /// The arrangement to import: the one open in FL, or failing that the
    /// first with anything in it.
    pub fn main_arrangement(&self) -> Option<&Arrangement> {
        self.current_arrangement
            .and_then(|iid| self.arrangements.iter().find(|a| a.iid == iid))
            .filter(|a| !a.items.is_empty())
            .or_else(|| self.arrangements.iter().find(|a| !a.items.is_empty()))
            .or_else(|| self.arrangements.first())
    }
}

/// What a channel is, by FL's type byte (sampler-likes reclassified: FL
/// stores audio clips and samplers with a sample as "instrument" too).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelKind {
    /// FL's own Sampler, or an audio clip's channel.
    #[default]
    Sampler,
    /// A generator plugin: native or VST.
    Instrument,
    /// Plays its children together.
    Layer,
    /// An automation clip.
    Automation,
    Unknown(u8),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Channel {
    /// The id patterns and the playlist refer to it by.
    pub iid: u16,
    pub kind: ChannelKind,
    /// The name shown in the channel rack, if one was set.
    pub name: Option<String>,
    /// The generator, for instruments.
    pub plugin: Option<Plugin>,
    pub sample_path: Option<String>,
    pub color: Option<Rgb>,
    /// `0..=12800`; FL's default is 10000.
    pub volume: Option<u32>,
    /// `0..=12800`, 6400 in the middle.
    pub pan: Option<i32>,
    /// The channel's pitch knob, in cents.
    pub pitch_cents: Option<i32>,
    /// The key that plays a sample at its own pitch (FL's C5 is 60).
    pub root_note: Option<u8>,
    /// The mixer insert it plays into; `None` for "current" or unset.
    pub insert: Option<u16>,
    pub enabled: bool,
    /// An automation clip's shape.
    pub automation: Option<AutomationClip>,
    /// A layer's children, by iid.
    pub children: Vec<u16>,
}

impl Channel {
    /// What to call it: its own name, else its plugin's.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| self.plugin.as_ref().map(Plugin::display_name))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Channel {}", self.iid + 1))
    }
}

/// A generator or effect plugin.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plugin {
    /// FL's name for the plugin class: `"3x Osc"`, `"Fruity Limiter"`, or
    /// `"Fruity Wrapper"` for VSTs. Empty for the Sampler.
    pub internal_name: String,
    /// The name it shows under (effects: the slot's name).
    pub name: Option<String>,
    /// For VSTs: what the wrapper says about the plugin.
    pub vst: Option<VstInfo>,
    /// The plugin's saved state, as FL wrote it.
    pub state: Vec<u8>,
}

impl Plugin {
    /// The plugin's own name: the VST's for wrapped plugins (or, if the
    /// wrapper's state doesn't say, the name it shows under), else FL's.
    pub fn plugin_name(&self) -> &str {
        match (&self.vst, &self.name) {
            (Some(vst), _) if !vst.name.is_empty() => &vst.name,
            (_, Some(name)) if self.is_vst() && !name.trim().is_empty() => name,
            _ => &self.internal_name,
        }
    }

    pub fn display_name(&self) -> String {
        let own = self.plugin_name();
        if own.is_empty() {
            self.name.clone().unwrap_or_default()
        } else {
            own.to_owned()
        }
    }

    pub fn is_vst(&self) -> bool {
        self.internal_name.eq_ignore_ascii_case("Fruity Wrapper")
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VstInfo {
    pub name: String,
    pub vendor: String,
    pub path: String,
    /// Which plugin it is, as canopy's host finds it again, when the
    /// wrapper says (FL 20 and later do).
    pub plugin: Option<PluginId>,
}

/// A third-party plugin as canopy's plugin host names it: its format and
/// its id in that format (`plugins::PluginInfo::id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginId {
    pub format: PluginFormat,
    pub id: String,
}

/// The plugin formats FL's wrapper hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PluginFormat {
    Clap,
    Vst3,
    Vst2,
}

impl PluginFormat {
    /// The format a canopy project names it by.
    pub fn project(self) -> project::Format {
        match self {
            PluginFormat::Clap => project::Format::Clap,
            PluginFormat::Vst3 => project::Format::Vst3,
            PluginFormat::Vst2 => project::Format::Vst2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AutomationClip {
    pub points: Vec<AutomationPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AutomationPoint {
    /// From the clip's start, in quarter notes.
    pub position: f64,
    /// `0..=1`.
    pub value: f64,
    /// How the curve bends; FL's range is `-1..=1`.
    pub tension: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pattern {
    pub iid: u16,
    pub name: Option<String>,
    pub color: Option<Rgb>,
    /// Set when the user gave it a length; 0 or `None` means "fit the notes".
    pub length: Option<u32>,
    pub notes: Vec<Note>,
    /// Step automation inside the pattern (not imported).
    pub controller_events: usize,
}

impl Pattern {
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Pattern {}", self.iid))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Note {
    /// From the pattern's start, in ticks.
    pub position: u32,
    pub flags: u16,
    /// The channel that plays it, by iid.
    pub channel: u16,
    /// 0 for notes entered in the step sequencer (they play the whole sound).
    pub length: u32,
    /// MIDI note number: FL's C5 is 60.
    pub key: u16,
    pub group: u16,
    /// `0..=240`, 120 in tune.
    pub fine_pitch: u8,
    /// `0..=128`, 64 by default.
    pub release: u8,
    pub midi_channel: u8,
    /// `0..=128`, 64 in the middle.
    pub pan: u8,
    /// `0..=128`, 100 by default.
    pub velocity: u8,
    pub mod_x: u8,
    pub mod_y: u8,
}

impl Note {
    /// Slide notes glide from the previous note instead of playing their own.
    pub fn is_slide(&self) -> bool {
        self.flags & 0x08 != 0
    }
}

/// One playlist: FL 12.9 and later can have several.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Arrangement {
    pub iid: u16,
    pub name: Option<String>,
    pub items: Vec<PlaylistItem>,
    pub markers: Vec<Marker>,
    pub tracks: Vec<PlaylistTrack>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemTarget {
    Pattern(u16),
    /// An audio clip or automation clip: the channel that holds it.
    Channel(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlaylistItem {
    /// In ticks.
    pub position: u32,
    /// In ticks.
    pub length: u32,
    pub target: Option<ItemTarget>,
    /// Playlist track, counting from the top from 0.
    pub track: u16,
    pub muted: bool,
    /// Where in the pattern or clip the item starts and ends, when trimmed.
    /// Patterns count ticks; audio and automation clips count quarter notes.
    pub start_offset: Option<f64>,
    pub end_offset: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlaylistTrack {
    pub index: u16,
    pub name: Option<String>,
    pub color: Option<Rgb>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Marker {
    /// In ticks.
    pub position: u32,
    /// What it does: 0 is a plain marker, 8 a time signature change; the
    /// rest loop, skip, pause, start and punch.
    pub action: u8,
    pub name: Option<String>,
    pub numerator: Option<u8>,
    pub denominator: Option<u8>,
}

impl Marker {
    pub const PLAIN: u8 = 0;
    pub const TIME_SIGNATURE: u8 = 8;
}

/// A mixer track.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Insert {
    /// 0 is the master.
    pub index: u16,
    pub name: Option<String>,
    pub color: Option<Rgb>,
    pub flags: Option<u32>,
    pub slots: Vec<Slot>,
    /// Which inserts it sends to, by number.
    pub routes: Vec<u16>,
    /// Send levels to other inserts, where the file has them: `(insert,
    /// level)`, 0 silent. FL's templates route every insert to the send
    /// inserts at level 0, and a route at level 0 is how FL sidechains an
    /// insert into another's effects.
    pub route_levels: Vec<(u16, i32)>,
    /// `0..=16000`; 12800 is 0 dB.
    pub volume: Option<i32>,
    /// `-6400..=6400`.
    pub pan: Option<i32>,
}

impl Insert {
    const EFFECTS_ON: u32 = 1 << 2;
    const ENABLED: u32 = 1 << 3;

    /// The inserts it actually feeds: its routes, less those at level 0.
    pub fn outputs(&self) -> Vec<u16> {
        self.routes
            .iter()
            .copied()
            .filter(|target| *target != self.index)
            .filter(|target| {
                self.route_levels
                    .iter()
                    .find(|(t, _)| t == target)
                    .is_none_or(|(_, level)| *level > 0)
            })
            .collect()
    }

    /// The inserts it only sidechains into: routes at level 0.
    pub fn sidechains(&self) -> Vec<u16> {
        self.routes
            .iter()
            .copied()
            .filter(|target| *target != self.index)
            .filter(|target| {
                self.route_levels
                    .iter()
                    .any(|(t, level)| t == target && *level <= 0)
            })
            .collect()
    }

    /// An insert that's off passes no sound (FL's mute).
    pub fn enabled(&self) -> bool {
        self.flags.is_none_or(|f| f & Self::ENABLED != 0)
    }

    /// All its effects bypassed at once.
    pub fn effects_on(&self) -> bool {
        self.flags.is_none_or(|f| f & Self::EFFECTS_ON != 0)
    }

    pub fn display_name(&self) -> String {
        match &self.name {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ if self.index == 0 => "Master".to_owned(),
            _ => format!("Insert {}", self.index),
        }
    }
}

/// An effect slot on an insert.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Slot {
    pub index: u8,
    pub plugin: Plugin,
    pub enabled: Option<bool>,
    /// Dry/wet.
    pub mix: Option<i32>,
}

/// What an automation clip drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomationLink {
    /// The automation clip's channel; for other controllers (a Peak
    /// Controller) where they sit, packed like an insert target.
    pub channel: u16,
    pub target: AutomationTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationTarget {
    /// A channel's own control (`param` 0 is volume, 1 pan), or with
    /// `plugin` set, one of its generator's parameters.
    Channel {
        channel: u16,
        param: u16,
        plugin: bool,
    },
    /// An insert's own control, or an effect's parameter in one of its slots.
    Insert {
        insert: u16,
        slot: Option<u8>,
        param: u16,
    },
    /// Master controls: 0 volume, 2 pitch, 5 tempo.
    Global {
        param: u16,
    },
    Unknown {
        param: u16,
        destination: u16,
    },
}
