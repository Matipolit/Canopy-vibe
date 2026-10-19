//! Events to [`Project`]: one pass over the stream, keeping track of which
//! channel, pattern, arrangement and mixer insert the events belong to.
//!
//! Each kind of event either names its owner (a new channel, pattern,
//! arrangement) or applies to the latest one. The plugin events are shared
//! by channels (their generator) and mixer slots (their effects), so the
//! parser also tracks whether it is in the channel rack or the mixer. In
//! the mixer, a slot's plugin events come first and the slot's index event
//! closes it; an insert's output event closes the insert.
//!
//! Anything unknown or malformed is skipped, so a damaged file gives
//! whatever could be read.

use crate::ids::{self, mixer};
use crate::model::{
    Arrangement, AutomationClip, AutomationLink, AutomationPoint, AutomationTarget, Channel,
    ChannelKind, Insert, ItemTarget, Marker, Note, Pattern, PlaylistItem, PlaylistTrack, Plugin,
    PluginFormat, PluginId, Project, Rgb, Slot, VstInfo,
};
use crate::reader::{self, Cursor, Event, ParseError, TextEncoding};

/// No real project comes near these; they keep garbage from allocating
/// without bound.
const MAX_INSERTS: usize = 1024;
const MAX_SLOTS: usize = 64;

/// Reads a whole file.
pub fn parse(bytes: &[u8]) -> Result<Project, ParseError> {
    let events = reader::read(bytes)?;
    let mut parser = Parser::new(events.header);
    parser.project.truncated = events.truncated;
    for event in &events.events {
        parser.event(event);
    }
    Ok(parser.finish())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    None,
    /// Index into `project.channels`.
    Channel(usize),
    Mixer,
}

/// Where time markers go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerOwner {
    None,
    Pattern(usize),
    Arrangement(usize),
}

struct Parser {
    project: Project,
    encoding: TextEncoding,
    scope: Scope,
    pattern: Option<usize>,
    arrangement: Option<usize>,
    markers: MarkerOwner,
    /// The insert being read, by number.
    insert: usize,
    /// A mixer slot's plugin, until the slot index arrives.
    pending: Option<Plugin>,
    tempo_coarse: Option<u16>,
    tempo_fine: Option<u16>,
    time_numerator: Option<u8>,
    time_steps: Option<u8>,
    /// Automation links as the file has them: (clip channel, parameter,
    /// destination), decoded once the mixer's base is known.
    links: Vec<(u16, u16, u16)>,
    /// Where mixer locations start (see [`mixer`]).
    mixer_base: Option<u16>,
}

impl Parser {
    fn new(header: reader::Header) -> Parser {
        Parser {
            project: Project {
                format: header.format,
                ppq: header.ppq,
                channel_count: header.channels,
                ..Project::default()
            },
            encoding: TextEncoding::Unknown,
            scope: Scope::None,
            pattern: None,
            arrangement: None,
            markers: MarkerOwner::None,
            insert: 0,
            pending: None,
            tempo_coarse: None,
            tempo_fine: None,
            time_numerator: None,
            time_steps: None,
            links: Vec::new(),
            mixer_base: None,
        }
    }

    fn text(&self, event: &Event) -> Option<String> {
        event.data().map(|d| self.encoding.decode(d))
    }

    fn channel(&mut self) -> Option<&mut Channel> {
        match self.scope {
            Scope::Channel(index) => self.project.channels.get_mut(index),
            _ => None,
        }
    }

    fn event(&mut self, event: &Event) {
        let number = event.number();
        match event.id {
            ids::VERSION => {
                if let Some(data) = event.data() {
                    let version = reader::decode_ansi(data).trim_end_matches('\0').to_owned();
                    self.encoding = TextEncoding::for_version(&version);
                    self.project.version = Some(version);
                }
            }
            ids::TEMPO => self.project.tempo = number.map(|n| f64::from(n) / 1000.0),
            ids::TEMPO_COARSE => self.tempo_coarse = number.map(|n| n as u16),
            ids::TEMPO_FINE => self.tempo_fine = number.map(|n| n as u16),
            ids::TIME_SIG_NUMERATOR => self.time_numerator = number.map(|n| n as u8),
            ids::TIME_SIG_DENOMINATOR => self.time_steps = number.map(|n| n as u8),
            ids::TITLE => self.project.title = self.text(event),

            ids::CHANNEL_NEW => {
                if let Some(iid) = number {
                    self.new_channel(iid as u16);
                }
            }
            ids::PATTERN_NEW => {
                if let Some(iid) = number {
                    let index = self.pattern_index(iid as u16);
                    self.pattern = Some(index);
                    self.markers = MarkerOwner::Pattern(index);
                }
            }
            ids::ARRANGEMENT_NEW => {
                if let Some(iid) = number {
                    let index = self.arrangement_index(iid as u16);
                    self.arrangement = Some(index);
                    self.markers = MarkerOwner::Arrangement(index);
                    self.scope = Scope::None;
                }
            }
            ids::CURRENT_ARRANGEMENT => self.project.current_arrangement = number.map(|n| n as u16),

            ids::PATTERN_NOTES
            | ids::PATTERN_NAME
            | ids::PATTERN_COLOR
            | ids::PATTERN_LENGTH
            | ids::PATTERN_CONTROLLERS => self.pattern_event(event),

            ids::PLAYLIST | ids::ARRANGEMENT_NAME | ids::TRACK_DATA | ids::TRACK_NAME => {
                self.arrangement_event(event)
            }

            ids::MARKER_POSITION
            | ids::MARKER_NAME
            | ids::MARKER_NUMERATOR
            | ids::MARKER_DENOMINATOR => self.marker_event(event),

            ids::INSERT_ICON
            | ids::SLOT_INDEX
            | ids::INSERT_OUTPUT
            | ids::INSERT_COLOR
            | ids::INSERT_INPUT
            | ids::INSERT_NAME
            | ids::INSERT_ROUTING
            | ids::INSERT_FLAGS => self.insert_event(event),
            ids::MIXER_PARAMS => {
                if let Some(data) = event.data() {
                    self.mixer_params(data);
                }
            }

            ids::PLUGIN_COLOR
            | ids::PLUGIN_INTERNAL_NAME
            | ids::PLUGIN_NAME
            | ids::PLUGIN_STATE => self.plugin_event(event),

            ids::REMOTE_CONTROLLER => {
                if let Some(link) = event.data().and_then(raw_link) {
                    self.links.push(link);
                }
            }

            _ => self.channel_event(event),
        }
    }

    fn new_channel(&mut self, iid: u16) {
        let index = match self.project.channels.iter().position(|c| c.iid == iid) {
            Some(index) => index,
            None => {
                self.project.channels.push(Channel {
                    iid,
                    enabled: true,
                    ..Channel::default()
                });
                self.project.channels.len() - 1
            }
        };
        self.scope = Scope::Channel(index);
        self.markers = MarkerOwner::None;
    }

    fn pattern_index(&mut self, iid: u16) -> usize {
        match self.project.patterns.iter().position(|p| p.iid == iid) {
            Some(index) => index,
            None => {
                self.project.patterns.push(Pattern {
                    iid,
                    ..Pattern::default()
                });
                self.project.patterns.len() - 1
            }
        }
    }

    fn arrangement_index(&mut self, iid: u16) -> usize {
        match self.project.arrangements.iter().position(|a| a.iid == iid) {
            Some(index) => index,
            None => {
                self.project.arrangements.push(Arrangement {
                    iid,
                    ..Arrangement::default()
                });
                self.project.arrangements.len() - 1
            }
        }
    }

    /// The arrangement being read; files from before FL 12.9 have just
    /// one, never announced.
    fn current_arrangement(&mut self) -> usize {
        match self.arrangement {
            Some(index) => index,
            None => {
                let index = self.arrangement_index(0);
                self.arrangement = Some(index);
                index
            }
        }
    }

    fn channel_event(&mut self, event: &Event) {
        let number = event.number();
        let text = if reader::is_text(event.id) {
            self.text(event)
        } else {
            None
        };
        let Some(channel) = self.channel() else {
            return;
        };
        match event.id {
            ids::CHANNEL_TYPE => {
                channel.kind = match number {
                    Some(0) => ChannelKind::Sampler,
                    Some(2 | 4) => ChannelKind::Instrument,
                    Some(3) => ChannelKind::Layer,
                    Some(5) => ChannelKind::Automation,
                    Some(other) => ChannelKind::Unknown(other as u8),
                    None => ChannelKind::Sampler,
                }
            }
            ids::CHANNEL_ENABLED => channel.enabled = number != Some(0),
            ids::CHANNEL_INSERT => {
                channel.insert = number.and_then(|n| u16::try_from(n as u8 as i8).ok())
            }
            ids::CHANNEL_INSERT_WORD => {
                channel.insert = number.and_then(|n| u16::try_from(n as u16 as i16).ok())
            }
            ids::CHANNEL_NAME_OLD => {
                if channel.name.is_none() {
                    channel.name = text;
                }
            }
            ids::SAMPLE_PATH => channel.sample_path = text.filter(|t| !t.is_empty()),
            ids::CHANNEL_LEVELS => {
                if let Some(data) = event.data() {
                    let mut cursor = Cursor::new(data);
                    if let (Some(pan), Some(volume)) = (cursor.i32(), cursor.u32()) {
                        channel.pan = Some(pan);
                        channel.volume = Some(volume);
                        channel.pitch_cents = cursor.i32();
                    }
                }
            }
            ids::CHANNEL_VOLUME_WORD => channel.volume = channel.volume.or(number),
            ids::CHANNEL_PAN_WORD => channel.pan = channel.pan.or(number.map(|n| n as i32)),
            ids::CHANNEL_VOLUME_BYTE => channel.volume = channel.volume.or(number.map(|n| n * 100)),
            ids::CHANNEL_PAN_BYTE => channel.pan = channel.pan.or(number.map(|n| n as i32 * 100)),
            ids::LAYER_CHILD => {
                if let Some(child) = number {
                    channel.children.push(child as u16);
                }
            }
            ids::AUTOMATION_DATA => {
                channel.automation = event.data().map(automation_clip);
            }
            ids::CHANNEL_PARAMETERS => {
                channel.root_note = event
                    .data()
                    .and_then(|d| Cursor::new(d.get(16..)?).i32())
                    .and_then(|n| u8::try_from(n).ok())
                    .filter(|n| *n <= 127);
            }
            _ => {}
        }
    }

    fn plugin_event(&mut self, event: &Event) {
        let text = if reader::is_text(event.id) {
            self.text(event)
        } else {
            None
        };
        match self.scope {
            Scope::Channel(index) => {
                let Some(channel) = self.project.channels.get_mut(index) else {
                    return;
                };
                match event.id {
                    ids::PLUGIN_COLOR => channel.color = event.number().map(Rgb::from_dword),
                    // The channel's own name, whatever its plugin.
                    ids::PLUGIN_NAME => channel.name = text,
                    _ => apply_plugin(
                        channel.plugin.get_or_insert_with(Plugin::default),
                        event,
                        text,
                    ),
                }
            }
            Scope::Mixer => {
                // FL 9 has no slot index events: a new plugin name starts the
                // next slot.
                if event.id == ids::PLUGIN_INTERNAL_NAME
                    && self
                        .pending
                        .as_ref()
                        .is_some_and(|p| !p.internal_name.is_empty())
                {
                    self.flush_pending(None);
                }
                let pending = self.pending.get_or_insert_with(Plugin::default);
                match event.id {
                    ids::PLUGIN_NAME => pending.name = text,
                    ids::PLUGIN_COLOR => {}
                    _ => apply_plugin(pending, event, text),
                }
            }
            Scope::None => {}
        }
    }

    fn pattern_event(&mut self, event: &Event) {
        let text = if reader::is_text(event.id) {
            self.text(event)
        } else {
            None
        };
        let Some(pattern) = self.pattern.and_then(|i| self.project.patterns.get_mut(i)) else {
            return;
        };
        match event.id {
            ids::PATTERN_NOTES => {
                if let Some(data) = event.data() {
                    let records = data.as_chunks::<24>().0;
                    pattern.notes.extend(records.iter().filter_map(|r| note(r)));
                }
            }
            ids::PATTERN_NAME => pattern.name = text,
            ids::PATTERN_COLOR => pattern.color = event.number().map(Rgb::from_dword),
            ids::PATTERN_LENGTH => pattern.length = event.number().filter(|n| *n > 0),
            ids::PATTERN_CONTROLLERS => {
                pattern.controller_events += event.data().map_or(0, |d| d.len() / 12)
            }
            _ => {}
        }
    }

    fn arrangement_event(&mut self, event: &Event) {
        let text = if reader::is_text(event.id) {
            self.text(event)
        } else {
            None
        };
        let major = self.project.major_version();
        let index = self.current_arrangement();
        let arrangement = &mut self.project.arrangements[index];
        self.markers = MarkerOwner::Arrangement(index);
        match event.id {
            ids::ARRANGEMENT_NAME => arrangement.name = text,
            ids::PLAYLIST => {
                if let Some(data) = event.data() {
                    arrangement.items.extend(playlist(data, major));
                }
            }
            ids::TRACK_DATA => {
                let data = event.data().unwrap_or_default();
                let mut cursor = Cursor::new(data);
                let number = cursor.u32();
                let color = cursor.u32();
                let index = number
                    .map(|n| n.saturating_sub(1) as u16)
                    .unwrap_or(arrangement.tracks.len() as u16);
                arrangement.tracks.push(PlaylistTrack {
                    index,
                    name: None,
                    color: color.map(Rgb::from_dword),
                });
            }
            ids::TRACK_NAME => {
                if let Some(track) = arrangement.tracks.last_mut() {
                    track.name = text;
                }
            }
            _ => {}
        }
    }

    fn marker_event(&mut self, event: &Event) {
        let text = if reader::is_text(event.id) {
            self.text(event)
        } else {
            None
        };
        let markers = match self.markers {
            MarkerOwner::Arrangement(index) => &mut self.project.arrangements[index].markers,
            // Markers inside patterns only mark the piano roll.
            MarkerOwner::Pattern(_) | MarkerOwner::None => return,
        };
        match event.id {
            ids::MARKER_POSITION => {
                if let Some(value) = event.number() {
                    markers.push(Marker {
                        position: value & 0x00FF_FFFF,
                        action: (value >> 24) as u8,
                        ..Marker::default()
                    });
                }
            }
            ids::MARKER_NAME => {
                if let Some(marker) = markers.last_mut() {
                    marker.name = text;
                }
            }
            ids::MARKER_NUMERATOR => {
                if let Some(marker) = markers.last_mut() {
                    marker.numerator = event.number().map(|n| n as u8);
                }
            }
            ids::MARKER_DENOMINATOR => {
                if let Some(marker) = markers.last_mut() {
                    marker.denominator = event.number().map(|n| n as u8);
                }
            }
            _ => {}
        }
    }

    fn insert_mut(&mut self) -> Option<&mut Insert> {
        if self.insert >= MAX_INSERTS {
            return None;
        }
        let index = self.insert as u16;
        let position = match self.project.inserts.iter().position(|i| i.index == index) {
            Some(position) => position,
            None => {
                self.project.inserts.push(Insert {
                    index,
                    ..Insert::default()
                });
                self.project.inserts.len() - 1
            }
        };
        self.project.inserts.get_mut(position)
    }

    fn insert_event(&mut self, event: &Event) {
        if self.scope != Scope::Mixer {
            self.scope = Scope::Mixer;
            self.pending = None;
        }
        let text = if reader::is_text(event.id) {
            self.text(event)
        } else {
            None
        };
        match event.id {
            ids::SLOT_INDEX => {
                let index = event.number().map(|n| (n & 0xFF) as u8);
                self.flush_pending(index);
            }
            ids::INSERT_OUTPUT => {
                self.flush_pending(None);
                self.insert_mut();
                self.insert += 1;
            }
            ids::INSERT_NAME => {
                if let Some(insert) = self.insert_mut() {
                    insert.name = text.filter(|t| !t.is_empty());
                }
            }
            ids::INSERT_COLOR => {
                if let Some(insert) = self.insert_mut() {
                    insert.color = event.number().map(Rgb::from_dword);
                }
            }
            ids::INSERT_FLAGS => {
                let flags = event.data().and_then(|d| Cursor::new(d.get(4..)?).u32());
                if let Some(insert) = self.insert_mut() {
                    insert.flags = flags;
                }
            }
            ids::INSERT_ROUTING => {
                let routes: Vec<u16> = event
                    .data()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .filter(|(_, on)| **on != 0)
                    .map(|(target, _)| target as u16)
                    .collect();
                if let Some(insert) = self.insert_mut() {
                    insert.routes = routes;
                }
            }
            _ => {}
        }
    }

    /// Puts the plugin read since the last slot into slot `index` (the next
    /// free one if `None`). Leftovers without a plugin name are dropped.
    fn flush_pending(&mut self, index: Option<u8>) {
        let Some(plugin) = self.pending.take() else {
            return;
        };
        if plugin.internal_name.is_empty() && plugin.name.is_none() {
            return;
        }
        let Some(insert) = self.insert_mut() else {
            return;
        };
        if insert.slots.len() >= MAX_SLOTS {
            return;
        }
        let index = index.unwrap_or_else(|| {
            insert
                .slots
                .iter()
                .map(|s| s.index.saturating_add(1))
                .max()
                .unwrap_or(0)
        });
        insert.slots.retain(|s| s.index != index);
        insert.slots.push(Slot {
            index,
            plugin,
            enabled: None,
            mix: None,
        });
        insert.slots.sort_by_key(|s| s.index);
    }

    fn mixer_params(&mut self, data: &[u8]) {
        let records = data.as_chunks::<12>().0;
        let location = |record: &[u8; 12]| u16::from_le_bytes([record[6], record[7]]);
        // The master's volume comes first among the volumes, at the base
        // itself; failing that, the version says.
        let by_version = if self.project.major_version().is_some_and(|m| m >= 26) {
            mixer::BASE_26
        } else {
            mixer::DEFAULT_BASE
        };
        let base = records
            .iter()
            .find(|r| r[4] == mixer::VOLUME && r[5] == mixer::OWN)
            .map(location)
            .filter(|l| l & 0x0FFF == 0)
            .unwrap_or(by_version);
        self.mixer_base = Some(base);
        for record in records {
            let id = record[4];
            let value = i32::from_le_bytes([record[8], record[9], record[10], record[11]]);
            let Some((insert, slot)) = mixer_location(location(record), base) else {
                continue;
            };
            let Some(insert) = self.project.inserts.iter_mut().find(|i| i.index == insert) else {
                continue;
            };
            if record[5] == mixer::ROUTE {
                let target = u16::from(id);
                insert.route_levels.retain(|(t, _)| *t != target);
                insert.route_levels.push((target, value));
                continue;
            }
            match id {
                mixer::VOLUME => insert.volume = Some(value),
                mixer::PAN => insert.pan = Some(value),
                id if (mixer::ROUTE_VOLUME..mixer::VOLUME).contains(&id) => {
                    let target = u16::from(id - mixer::ROUTE_VOLUME);
                    insert.route_levels.retain(|(t, _)| *t != target);
                    insert.route_levels.push((target, value));
                }
                mixer::SLOT_ENABLED | mixer::SLOT_MIX => {
                    if let Some(slot) = insert.slots.iter_mut().find(|s| s.index == slot) {
                        if id == mixer::SLOT_ENABLED {
                            slot.enabled = Some(value != 0);
                        } else {
                            slot.mix = Some(value);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn finish(mut self) -> Project {
        self.flush_pending(None);
        let base = self.mixer_base.unwrap_or_else(|| {
            if self.project.major_version().is_some_and(|m| m >= 26) {
                mixer::BASE_26
            } else {
                mixer::DEFAULT_BASE
            }
        });
        self.project.automation_links = self
            .links
            .iter()
            .map(|&(channel, param, destination)| AutomationLink {
                channel,
                target: link_target(param, destination, base),
            })
            .collect();
        self.project.mixer_base = base;
        let project = &mut self.project;
        if project.tempo.is_none()
            && let Some(coarse) = self.tempo_coarse
        {
            let fine = self.tempo_fine.map_or(0.0, |f| f64::from(f) / 1000.0);
            project.tempo = Some(f64::from(coarse) + fine);
        }
        if let Some(numerator) = self.time_numerator.filter(|n| (1..=32).contains(n)) {
            // The second number is the beat's length in newer versions and
            // steps per beat in older ones; only a power of two can be a
            // denominator.
            let denominator = self
                .time_steps
                .filter(|d| matches!(d, 2 | 4 | 8 | 16))
                .unwrap_or(4);
            project.time_signature = Some((numerator, denominator));
        }
        // Before FL 12, inserts 100 to 103 were the four send tracks, and
        // every insert had a route to each with a send knob that starts at
        // zero; files don't store the knob until it's turned.
        if project.major_version().is_some_and(|m| m < 12) {
            for insert in &mut project.inserts {
                for send in 100..=103 {
                    if insert.routes.contains(&send)
                        && !insert.route_levels.iter().any(|(t, _)| *t == send)
                    {
                        insert.route_levels.push((send, 0));
                    }
                }
            }
        }
        for channel in &mut project.channels {
            // FL stores the Sampler, and audio clips, as instruments with an
            // empty plugin name.
            let sampler_like = channel
                .plugin
                .as_ref()
                .is_none_or(|p| p.internal_name.trim().is_empty());
            if channel.kind == ChannelKind::Instrument && sampler_like {
                channel.kind = ChannelKind::Sampler;
            }
            if channel.kind == ChannelKind::Sampler {
                channel.plugin = None;
            }
        }
        self.project
    }
}

fn apply_plugin(plugin: &mut Plugin, event: &Event, text: Option<String>) {
    match event.id {
        ids::PLUGIN_INTERNAL_NAME => plugin.internal_name = text.unwrap_or_default(),
        ids::PLUGIN_NAME => plugin.name = text,
        ids::PLUGIN_STATE => {
            if let Some(data) = event.data() {
                plugin.state = data.to_vec();
                if plugin.is_vst() {
                    plugin.vst = vst_info(data);
                }
            }
        }
        _ => {}
    }
}

/// A VST's name, vendor, path and identity from FL's wrapper state: a
/// 32-bit kind, then records of a 32-bit id, a 64-bit length and that many
/// bytes. The identity is in record 51 (a VST2 plugin's unique id), 52 (a
/// VST3 class id, as the system that saved it had it in memory) or 58 (a
/// CLAP id); which one names the plugin goes by its file, since a VST2
/// may carry a class id too.
pub(crate) fn vst_info(data: &[u8]) -> Option<VstInfo> {
    let mut cursor = Cursor::new(data);
    cursor.u32()?;
    let mut info = VstInfo::default();
    let mut found = false;
    let (mut vst2, mut vst3, mut clap) = (None, None, None);
    while !cursor.is_empty() {
        let (Some(id), Some(len)) = (cursor.u32(), cursor.u64()) else {
            break;
        };
        let Some(bytes) = usize::try_from(len).ok().and_then(|len| cursor.take(len)) else {
            break;
        };
        let text = || {
            String::from_utf8_lossy(bytes)
                .trim_end_matches('\0')
                .to_owned()
        };
        match id {
            51 => vst2 = <[u8; 4]>::try_from(bytes).ok().map(i32::from_le_bytes),
            52 => vst3 = <[u8; 16]>::try_from(bytes).ok(),
            54 => {
                info.name = text();
                found = true;
            }
            55 => info.path = text(),
            56 => info.vendor = text(),
            58 => clap = Some(text()).filter(|t| !t.is_empty()),
            _ => {}
        }
    }
    info.plugin = plugin_id(&info.path, vst2, vst3, clap);
    found.then_some(info)
}

/// Which plugin the wrapper's records name, by the plugin file's kind.
fn plugin_id(
    path: &str,
    vst2: Option<i32>,
    vst3: Option<[u8; 16]>,
    clap: Option<String>,
) -> Option<PluginId> {
    let lower = path.to_lowercase();
    let file = |ext: &str| lower.ends_with(ext) || lower.contains(&format!("{ext}\\"));
    // Saved on Windows, a VST3 class id is in COM's byte order.
    let windows = path.contains('\\') || path.get(1..2) == Some(":");
    let vst3 = || {
        vst3.map(|bytes| PluginId {
            format: PluginFormat::Vst3,
            id: plugins::state::vst3_id(&bytes, windows),
        })
    };
    let vst2 = || {
        vst2.map(|id| PluginId {
            format: PluginFormat::Vst2,
            id: plugins::state::vst2_id(id),
        })
    };
    if let Some(id) = clap {
        return Some(PluginId {
            format: PluginFormat::Clap,
            id,
        });
    }
    if file(".vst3") {
        return vst3();
    }
    if file(".dll") || file(".so") || file(".vst") {
        return vst2();
    }
    vst3().or_else(vst2)
}

fn note(record: &[u8]) -> Option<Note> {
    let mut c = Cursor::new(record);
    Some(Note {
        position: c.u32()?,
        flags: c.u16()?,
        channel: c.u16()?,
        length: c.u32()?,
        key: c.u16()?,
        group: c.u16()?,
        fine_pitch: c.u8()?,
        release: {
            c.u8()?;
            c.u8()?
        },
        midi_channel: c.u8()?,
        pan: c.u8()?,
        velocity: c.u8()?,
        mod_x: c.u8()?,
        mod_y: c.u8()?,
    })
}

/// Bytes per playlist item: 32 before FL 21, 60 until FL 24, 80 in FL 25,
/// 88 from FL 26. The likeliest first; the others in case.
fn item_sizes(major: Option<u32>) -> [usize; 4] {
    match major {
        Some(m) if m >= 26 => [88, 80, 60, 32],
        Some(25) => [80, 88, 60, 32],
        Some(m) if m >= 21 => [60, 80, 88, 32],
        Some(_) => [32, 60, 80, 88],
        None => [88, 80, 60, 32],
    }
}

/// Every item's 16-bit "pattern base" is 20480, which tells a right record
/// size from a wrong one.
const PATTERN_BASE: u16 = 20480;

fn playlist(data: &[u8], major: Option<u32>) -> Vec<PlaylistItem> {
    let base_at = |size: usize, i: usize| {
        data.get(i * size + 4..i * size + 6)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let fits = |size: usize| {
        data.len().is_multiple_of(size)
            && (0..data.len() / size).all(|i| matches!(base_at(size, i), Some(PATTERN_BASE | 0)))
    };
    let sizes = item_sizes(major);
    let Some(size) = sizes.iter().copied().find(|s| fits(*s)).or_else(|| {
        sizes
            .iter()
            .copied()
            .find(|s| data.len().is_multiple_of(*s))
    }) else {
        return Vec::new();
    };
    // Tracks are stored counting up from the bottom of the playlist: 500
    // of them since FL 12.9, 199 before.
    let records: Vec<&[u8]> = data.chunks_exact(size).collect();
    let tall = major.is_none_or(|m| m >= 20)
        || records
            .iter()
            .any(|r| u16::from_le_bytes([r[12], r[13]]) > 198);
    let bottom: u16 = if tall { 499 } else { 198 };
    let blocks = major.is_some_and(|m| m < 12);
    records
        .into_iter()
        .filter_map(|record| playlist_item(record, bottom, blocks))
        .collect()
}

/// Before FL 12, pattern blocks have no pattern base and this index, and
/// keep their pattern's number in the track field, counting down from 999.
const BLOCK_INDEX: u16 = 0x6000;
const BLOCK_TOP: u16 = 999;

fn playlist_item(record: &[u8], bottom: u16, blocks: bool) -> Option<PlaylistItem> {
    let mut c = Cursor::new(record);
    let position = c.u32()?;
    let base = c.u16()?;
    let index = c.u16()?;
    let length = c.u32()?;
    let track = c.u16()?;
    let _group = c.u16()?;
    let _unknown = c.u16()?;
    let flags = c.u16()?;
    c.skip(4)?;
    let start = c.u32()?;
    let end = c.u32()?;
    if blocks && base == 0 && index == BLOCK_INDEX && (1..BLOCK_TOP).contains(&track) {
        let pattern = BLOCK_TOP - track;
        return Some(PlaylistItem {
            position,
            length,
            target: Some(ItemTarget::Pattern(pattern)),
            track: pattern - 1,
            muted: flags & 0x2000 != 0,
            start_offset: None,
            end_offset: None,
        });
    }
    // Unused records hold garbage, with no pattern base.
    if base == 0 || track > bottom {
        return None;
    }
    let target = if index > base {
        ItemTarget::Pattern(index - base)
    } else {
        ItemTarget::Channel(index)
    };
    // Patterns count their trims in ticks, clips in quarter notes; both
    // use -1 (as an integer, or a float) for "not trimmed".
    let offset = |bits: u32| match target {
        ItemTarget::Pattern(_) => {
            let ticks = bits as i32;
            (ticks >= 0).then_some(f64::from(ticks))
        }
        ItemTarget::Channel(_) => {
            let beats = f32::from_bits(bits);
            (beats.is_finite() && beats >= 0.0).then_some(f64::from(beats))
        }
    };
    Some(PlaylistItem {
        position,
        length,
        target: Some(target),
        track: bottom - track,
        muted: flags & 0x2000 != 0,
        start_offset: offset(start),
        end_offset: offset(end),
    })
}

/// An automation clip's points: after a 21-byte header (the last four bytes
/// the count), 24 bytes each: the distance from the previous point in
/// quarter notes, the value, the tension, and 4 more bytes.
fn automation_clip(data: &[u8]) -> AutomationClip {
    let mut points = Vec::new();
    let mut cursor = Cursor::new(data);
    if cursor.skip(17).is_none() {
        return AutomationClip { points };
    }
    let count = cursor.u32().unwrap_or(0) as usize;
    let mut position = 0.0;
    for _ in 0..count.min(data.len() / 24) {
        let (Some(delta), Some(value), Some(tension), Some(_)) =
            (cursor.f64(), cursor.f64(), cursor.f32(), cursor.u32())
        else {
            break;
        };
        if !(delta.is_finite() && value.is_finite()) {
            continue;
        }
        // No song is a million quarter notes long.
        position = (position + delta.max(0.0)).min(1e6);
        points.push(AutomationPoint {
            position,
            value: value.clamp(0.0, 1.0),
            tension: if tension.is_finite() {
                tension.clamp(-1.0, 1.0)
            } else {
                0.0
            },
        });
    }
    AutomationClip { points }
}

/// A remote controller record: which automation clip (or controller), the
/// parameter, and where it is.
fn raw_link(data: &[u8]) -> Option<(u16, u16, u16)> {
    let mut c = Cursor::new(data);
    c.skip(2)?;
    let channel = c.u16()?;
    c.skip(4)?;
    let param = c.u16()?;
    let destination = c.u16()?;
    Some((channel, param, destination))
}

/// An insert and slot from a mixer location word, given the file's base.
pub(crate) fn mixer_location(location: u16, base: u16) -> Option<(u16, u8)> {
    let offset = location.checked_sub(base)?;
    // 128 inserts of 64 slots each, at most.
    (offset < 0x2000).then_some((offset >> 6, (offset & 0x3F) as u8))
}

/// What a link drives, from its parameter and destination words.
fn link_target(param: u16, destination: u16, base: u16) -> AutomationTarget {
    if let Some((insert, slot)) = mixer_location(destination, base) {
        AutomationTarget::Insert {
            insert,
            slot: Some(slot),
            param,
        }
    } else if destination & 0x4000 != 0 {
        AutomationTarget::Global { param }
    } else if destination < 0x2000 {
        AutomationTarget::Channel {
            channel: destination,
            param: param & 0x7FFF,
            plugin: param & 0x8000 != 0,
        }
    } else {
        AutomationTarget::Unknown { param, destination }
    }
}
