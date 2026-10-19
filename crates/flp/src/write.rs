//! Writes FL Studio files event by event: enough of the format to build
//! test projects, laid out the way FL 20 lays them out (or, where the
//! version given differs, FL 21, 25 or 26). Not a general writer; nothing
//! in canopy saves `.flp` files.

use crate::ids::{self, mixer};
use crate::model::{AutomationPoint, Note};
use crate::reader::{DWORD, TEXT, WORD};

/// The channel type byte FL writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelType {
    Sampler = 0,
    Generator = 2,
    Layer = 3,
    AudioClip = 4,
    Automation = 5,
}

/// One playlist item, as FL stores it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item {
    pub position: u32,
    pub length: u32,
    /// A pattern's iid, or a channel's.
    pub pattern: Option<u16>,
    pub channel: Option<u16>,
    /// Playlist track from the top, from 0.
    pub track: u16,
    pub muted: bool,
    /// Ticks for patterns, quarter notes for clips.
    pub start_offset: Option<f64>,
    pub end_offset: Option<f64>,
}

impl Item {
    pub fn pattern(iid: u16, position: u32, length: u32) -> Item {
        Item {
            position,
            length,
            pattern: Some(iid),
            channel: None,
            track: 0,
            muted: false,
            start_offset: None,
            end_offset: None,
        }
    }

    pub fn clip(channel: u16, position: u32, length: u32) -> Item {
        Item {
            channel: Some(channel),
            pattern: None,
            ..Item::pattern(0, position, length)
        }
    }
}

/// An effect in a mixer slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotPlugin {
    pub index: u8,
    pub internal_name: String,
    pub name: Option<String>,
    pub state: Vec<u8>,
}

/// FL's VST wrapper state: a 32-bit kind, then each record as a 32-bit
/// id, a 64-bit length and its bytes. 54 is the plugin's name, 55 its
/// path, 56 its vendor, 51/52/58 its identity, 53 its own state.
pub fn wrapper_state(records: &[(u32, &[u8])]) -> Vec<u8> {
    let mut data = 12u32.to_le_bytes().to_vec();
    for (id, bytes) in records {
        data.extend(id.to_le_bytes());
        data.extend((bytes.len() as u64).to_le_bytes());
        data.extend(*bytes);
    }
    data
}

#[derive(Debug, Clone)]
pub struct Writer {
    format: i16,
    channels: u16,
    ppq: u16,
    major: u32,
    events: Vec<u8>,
}

impl Writer {
    /// A project at `ppq`, claiming to be from FL `version`.
    pub fn new(ppq: u16, version: &str) -> Writer {
        let mut writer = Writer {
            format: 0,
            channels: 0,
            ppq,
            major: version
                .split('.')
                .next()
                .and_then(|m| m.parse().ok())
                .unwrap_or(20),
            events: Vec::new(),
        };
        let mut ascii = version.as_bytes().to_vec();
        ascii.push(0);
        writer.data(ids::VERSION, &ascii);
        writer
    }

    pub fn set_format(&mut self, format: i16) -> &mut Self {
        self.format = format;
        self
    }

    pub fn byte(&mut self, id: u8, value: u8) -> &mut Self {
        debug_assert!(id < WORD);
        self.events.extend([id, value]);
        self
    }

    pub fn word(&mut self, id: u8, value: u16) -> &mut Self {
        debug_assert!((WORD..DWORD).contains(&id));
        self.events.push(id);
        self.events.extend(value.to_le_bytes());
        self
    }

    pub fn dword(&mut self, id: u8, value: u32) -> &mut Self {
        debug_assert!((DWORD..TEXT).contains(&id));
        self.events.push(id);
        self.events.extend(value.to_le_bytes());
        self
    }

    pub fn data(&mut self, id: u8, data: &[u8]) -> &mut Self {
        debug_assert!(id >= TEXT);
        self.events.push(id);
        let mut len = data.len();
        loop {
            let byte = (len & 0x7F) as u8;
            len >>= 7;
            if len == 0 {
                self.events.push(byte);
                break;
            }
            self.events.push(byte | 0x80);
        }
        self.events.extend_from_slice(data);
        self
    }

    /// Text as FL 11.5 and later write it: UTF-16LE, NUL-terminated.
    pub fn text(&mut self, id: u8, text: &str) -> &mut Self {
        let bytes: Vec<u8> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        self.data(id, &bytes)
    }

    /// Text as FL before 11.5 wrote it: one byte a character.
    pub fn ansi(&mut self, id: u8, text: &str) -> &mut Self {
        let mut bytes: Vec<u8> = text.chars().map(|c| c as u32 as u8).collect();
        bytes.push(0);
        self.data(id, &bytes)
    }

    /// Bytes as they are, for events the other methods can't write.
    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.events.extend_from_slice(bytes);
        self
    }

    pub fn tempo(&mut self, bpm: f64) -> &mut Self {
        self.dword(ids::TEMPO, (bpm * 1000.0).round() as u32)
    }

    pub fn time_signature(&mut self, numerator: u8, denominator: u8) -> &mut Self {
        self.byte(ids::TIME_SIG_NUMERATOR, numerator)
            .byte(ids::TIME_SIG_DENOMINATOR, denominator)
    }

    /// Starts a channel; `plugin` is its generator's internal name (empty
    /// for the Sampler).
    pub fn channel(&mut self, iid: u16, kind: ChannelType, plugin: &str, name: &str) -> &mut Self {
        self.channels = self.channels.max(iid + 1);
        self.word(ids::CHANNEL_NEW, iid)
            .byte(ids::CHANNEL_TYPE, kind as u8)
            .text(ids::PLUGIN_INTERNAL_NAME, plugin)
            .text(ids::PLUGIN_NAME, name)
            .byte(ids::CHANNEL_ENABLED, 1)
    }

    /// The current channel's volume (`0..=12800`) and pan (`0..=12800`).
    pub fn levels(&mut self, volume: u32, pan: i32) -> &mut Self {
        let mut data = Vec::new();
        data.extend(pan.to_le_bytes());
        data.extend(volume.to_le_bytes());
        data.extend([0u8; 16]);
        self.data(ids::CHANNEL_LEVELS, &data)
    }

    pub fn color(&mut self, r: u8, g: u8, b: u8) -> &mut Self {
        self.dword(ids::PLUGIN_COLOR, u32::from_le_bytes([r, g, b, 0]))
    }

    /// Routes the current channel to mixer insert `insert`: a byte until
    /// FL 25, a word from FL 26.
    pub fn route_to(&mut self, insert: u8) -> &mut Self {
        if self.major >= 26 {
            self.word(ids::CHANNEL_INSERT_WORD, u16::from(insert))
        } else {
            self.byte(ids::CHANNEL_INSERT, insert)
        }
    }

    /// Where `insert`'s `slot` is, as mixer parameters and automation links
    /// say: from a base of `0x2000`, or `0x7000` from FL 26.
    pub fn location(&self, insert: u16, slot: u8) -> u16 {
        let base = if self.major >= 26 { 0x7000 } else { 0x2000 };
        base + (insert << 6) + u16::from(slot)
    }

    pub fn sample(&mut self, path: &str) -> &mut Self {
        self.text(ids::SAMPLE_PATH, path)
    }

    /// The current generator's state, as a VST wrapper with this name.
    pub fn vst(&mut self, name: &str) -> &mut Self {
        let mut data = 8u32.to_le_bytes().to_vec();
        data.extend(54u32.to_le_bytes());
        data.extend((name.len() as u64).to_le_bytes());
        data.extend(name.as_bytes());
        self.data(ids::PLUGIN_STATE, &data)
    }

    /// The current generator's state, as a VST wrapper with these records
    /// (see [`wrapper_state`]).
    pub fn wrapper(&mut self, records: &[(u32, &[u8])]) -> &mut Self {
        self.data(ids::PLUGIN_STATE, &wrapper_state(records))
    }

    /// An automation clip's points, for the current channel.
    pub fn automation(&mut self, points: &[AutomationPoint]) -> &mut Self {
        let mut data = vec![1, 0, 0, 0, 64, 0, 0, 0, 1, 4, 0, 0, 0, 3, 0, 0, 0];
        data.extend((points.len() as u32).to_le_bytes());
        let mut previous = 0.0;
        for point in points {
            data.extend((point.position - previous).to_le_bytes());
            previous = point.position;
            data.extend(point.value.to_le_bytes());
            data.extend(point.tension.to_le_bytes());
            data.extend([0u8; 4]);
        }
        self.data(ids::AUTOMATION_DATA, &data)
    }

    pub fn pattern_notes(&mut self, iid: u16, notes: &[Note]) -> &mut Self {
        let mut data = Vec::new();
        for note in notes {
            data.extend(note.position.to_le_bytes());
            data.extend(note.flags.to_le_bytes());
            data.extend(note.channel.to_le_bytes());
            data.extend(note.length.to_le_bytes());
            data.extend(note.key.to_le_bytes());
            data.extend(note.group.to_le_bytes());
            data.extend([
                note.fine_pitch,
                0,
                note.release,
                note.midi_channel,
                note.pan,
                note.velocity,
                note.mod_x,
                note.mod_y,
            ]);
        }
        self.word(ids::PATTERN_NEW, iid)
            .data(ids::PATTERN_NOTES, &data)
    }

    pub fn pattern_name(&mut self, iid: u16, name: &str) -> &mut Self {
        self.word(ids::PATTERN_NEW, iid)
            .text(ids::PATTERN_NAME, name)
    }

    /// An arrangement with these items, in the record size `version` uses.
    pub fn arrangement(&mut self, iid: u16, name: &str, items: &[Item]) -> &mut Self {
        let size = match self.major {
            m if m >= 26 => 88,
            25 => 80,
            m if m >= 21 => 60,
            _ => 32,
        };
        let mut data = Vec::new();
        for item in items {
            let start = data.len();
            let (index, offsets_as_ticks) = match (item.pattern, item.channel) {
                (Some(pattern), _) => (20480 + pattern, true),
                (None, Some(channel)) => (channel, false),
                (None, None) => (0, true),
            };
            data.extend(item.position.to_le_bytes());
            data.extend(20480u16.to_le_bytes());
            data.extend(index.to_le_bytes());
            data.extend(item.length.to_le_bytes());
            data.extend((499 - item.track.min(499)).to_le_bytes());
            data.extend(0u16.to_le_bytes());
            data.extend(120u16.to_le_bytes());
            let flags: u16 = if item.muted { 0x2040 } else { 0x40 };
            data.extend(flags.to_le_bytes());
            data.extend([64, 100, 128, 128]);
            for offset in [item.start_offset, item.end_offset] {
                let bits = match (offset, offsets_as_ticks) {
                    (Some(ticks), true) => (ticks.round() as i32) as u32,
                    (None, true) => u32::MAX,
                    (Some(quarters), false) => (quarters as f32).to_bits(),
                    (None, false) => (-1.0f32).to_bits(),
                };
                data.extend(bits.to_le_bytes());
            }
            data.resize(start + size, 0);
        }
        self.word(ids::ARRANGEMENT_NEW, iid)
            .text(ids::ARRANGEMENT_NAME, name)
            .data(ids::PLAYLIST, &data)
    }

    /// A time marker in the arrangement just written.
    pub fn marker(&mut self, position: u32, action: u8, name: &str) -> &mut Self {
        self.dword(ids::MARKER_POSITION, (u32::from(action) << 24) | position)
            .text(ids::MARKER_NAME, name)
    }

    /// The current arrangement, as FL notes which one was open.
    pub fn current_arrangement(&mut self, iid: u16) -> &mut Self {
        self.word(ids::CURRENT_ARRANGEMENT, iid)
    }

    /// One mixer insert, closed off. Inserts are numbered in the order
    /// they're written, the master first.
    pub fn insert(&mut self, name: &str, slots: &[SlotPlugin], routes: &[u16]) -> &mut Self {
        self.insert_with_flags(name, 0x4C, slots, routes)
    }

    /// An insert with these flags: 4 is effects on, 8 the insert on.
    pub fn insert_with_flags(
        &mut self,
        name: &str,
        insert_flags: u32,
        slots: &[SlotPlugin],
        routes: &[u16],
    ) -> &mut Self {
        if !name.is_empty() {
            self.text(ids::INSERT_NAME, name);
        }
        let mut flags = vec![0u8; 4];
        flags.extend(insert_flags.to_le_bytes());
        flags.extend([0u8; 4]);
        self.data(ids::INSERT_FLAGS, &flags);
        for index in 0..10u8 {
            if let Some(slot) = slots.iter().find(|s| s.index == index) {
                self.text(ids::PLUGIN_INTERNAL_NAME, &slot.internal_name);
                if let Some(name) = &slot.name {
                    self.text(ids::PLUGIN_NAME, name);
                }
                self.data(ids::PLUGIN_STATE, &slot.state);
            }
            self.word(ids::SLOT_INDEX, u16::from(index));
        }
        let mut routing = vec![0u8; 127];
        for route in routes {
            if let Some(byte) = routing.get_mut(usize::from(*route)) {
                *byte = 1;
            }
        }
        self.data(ids::INSERT_ROUTING, &routing)
            .dword(ids::INSERT_INPUT, u32::MAX)
            .dword(ids::INSERT_OUTPUT, u32::MAX)
    }

    /// The mixer's knobs: `(insert, slot, parameter id, value)`.
    pub fn mixer_params(&mut self, params: &[(u16, u8, u8, i32)]) -> &mut Self {
        let mut data = Vec::new();
        for &(insert, slot, id, value) in params {
            data.extend([0u8; 4]);
            data.push(id);
            data.push(mixer::OWN);
            data.extend(self.location(insert, slot).to_le_bytes());
            data.extend(value.to_le_bytes());
        }
        self.data(ids::MIXER_PARAMS, &data)
    }

    /// A route's level from `insert` to `target` (0 sidechains only), as
    /// raw mixer parameter bytes the way this version writes them.
    pub fn route_level(&mut self, insert: u16, target: u8, level: i32) -> &mut Self {
        let mut data = vec![0u8; 4];
        if self.major >= 26 {
            data.extend([target, mixer::ROUTE]);
        } else {
            data.extend([mixer::ROUTE_VOLUME + target, mixer::OWN]);
        }
        data.extend(self.location(insert, 0).to_le_bytes());
        data.extend(level.to_le_bytes());
        self.data(ids::MIXER_PARAMS, &data)
    }

    /// The current channel's sampler settings: its root note and pitch.
    pub fn sampler_settings(&mut self, root: u8, cents: i32) -> &mut Self {
        let mut levels = Vec::new();
        levels.extend(6400i32.to_le_bytes());
        levels.extend(10_000u32.to_le_bytes());
        levels.extend(cents.to_le_bytes());
        levels.extend([0u8; 12]);
        self.data(ids::CHANNEL_LEVELS, &levels);
        let mut params = vec![0u8; 16];
        params.extend(i32::from(root).to_le_bytes());
        params.resize(158, 0);
        self.data(ids::CHANNEL_PARAMETERS, &params)
    }

    /// Insert volume and pan, for [`Self::mixer_params`].
    pub fn insert_fader(insert: u16, volume: i32, pan: i32) -> [(u16, u8, u8, i32); 2] {
        [
            (insert, 0, mixer::VOLUME, volume),
            (insert, 0, mixer::PAN, pan),
        ]
    }

    /// Links automation clip `channel` to a target: `param` and
    /// `destination` as FL packs them.
    pub fn link(&mut self, channel: u16, param: u16, destination: u16) -> &mut Self {
        let mut data = vec![0, 0];
        data.extend(u32::from(channel).to_le_bytes());
        data.extend([0, 0]);
        data.extend(param.to_le_bytes());
        data.extend(destination.to_le_bytes());
        data.extend([8, 0, 0, 0, 0xD5, 1, 0, 0]);
        self.data(ids::REMOTE_CONTROLLER, &data)
    }

    pub fn finish(&self) -> Vec<u8> {
        let mut bytes = b"FLhd".to_vec();
        bytes.extend(6u32.to_le_bytes());
        bytes.extend(self.format.to_le_bytes());
        bytes.extend(self.channels.to_le_bytes());
        bytes.extend(self.ppq.to_le_bytes());
        bytes.extend(b"FLdt");
        bytes.extend((self.events.len() as u32).to_le_bytes());
        bytes.extend(&self.events);
        bytes
    }
}

/// A note with FL's defaults: velocity 100, centered, in tune.
pub fn note(channel: u16, position: u32, length: u32, key: u16) -> Note {
    Note {
        position,
        channel,
        length,
        key,
        fine_pitch: 120,
        release: 64,
        pan: 64,
        velocity: 100,
        mod_x: 128,
        mod_y: 128,
        ..Note::default()
    }
}
