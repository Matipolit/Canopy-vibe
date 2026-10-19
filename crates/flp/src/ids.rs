//! Event ids, as FL writes them and as the importer uses them. Cross-checked
//! between PyFLP, FLParser, dawtool and flpdiff, and against real files from
//! FL 9, 20 and 25.

use crate::reader::{DATA, DWORD, TEXT, WORD};

// Project.
pub const TIME_SIG_NUMERATOR: u8 = 17;
pub const TIME_SIG_DENOMINATOR: u8 = 18;
/// Integer BPM, in files before FL 3.4.
pub const TEMPO_COARSE: u8 = WORD + 2;
/// Thousandths of a BPM, added to the coarse tempo.
pub const TEMPO_FINE: u8 = WORD + 29;
/// BPM × 1000.
pub const TEMPO: u8 = DWORD + 28;
pub const TITLE: u8 = TEXT + 2;
pub const VERSION: u8 = TEXT + 7;

// Channels.
pub const CHANNEL_ENABLED: u8 = 0;
pub const CHANNEL_VOLUME_BYTE: u8 = 2;
pub const CHANNEL_PAN_BYTE: u8 = 3;
pub const CHANNEL_TYPE: u8 = 21;
/// The mixer insert, as a signed byte (-1 for none), until FL 25.
pub const CHANNEL_INSERT: u8 = 22;
pub const CHANNEL_NEW: u8 = WORD;
pub const CHANNEL_VOLUME_WORD: u8 = WORD + 8;
pub const CHANNEL_PAN_WORD: u8 = WORD + 9;
pub const LAYER_CHILD: u8 = WORD + 30;
/// The mixer insert, as a signed word, from FL 26 (which has more inserts
/// than a byte holds).
pub const CHANNEL_INSERT_WORD: u8 = WORD + 40;
/// The channel's name in old files; in FL 25 the first one is a banner.
pub const CHANNEL_NAME_OLD: u8 = TEXT;
pub const SAMPLE_PATH: u8 = TEXT + 4;
/// Pan, volume and pitch (cents): three 32-bit numbers, then filter settings.
pub const CHANNEL_LEVELS: u8 = DATA + 11;
/// A channel's sampler and keyboard settings, 32-bit numbers; the fifth is
/// its root note (60 for FL's C5).
pub const CHANNEL_PARAMETERS: u8 = DATA + 7;
pub const AUTOMATION_DATA: u8 = DATA + 26;

// Plugins: the same ids serve a channel's generator and a mixer slot's effect.
pub const PLUGIN_COLOR: u8 = DWORD;
pub const PLUGIN_INTERNAL_NAME: u8 = TEXT + 9;
pub const PLUGIN_NAME: u8 = TEXT + 11;
pub const PLUGIN_STATE: u8 = DATA + 5;

// Patterns.
pub const PATTERN_NEW: u8 = WORD + 1;
pub const PATTERN_COLOR: u8 = DWORD + 22;
pub const PATTERN_LENGTH: u8 = DWORD + 36;
pub const PATTERN_NAME: u8 = TEXT + 1;
pub const PATTERN_CONTROLLERS: u8 = DATA + 15;
pub const PATTERN_NOTES: u8 = DATA + 16;

// Arrangements and markers.
pub const ARRANGEMENT_NEW: u8 = WORD + 35;
pub const CURRENT_ARRANGEMENT: u8 = WORD + 36;
pub const ARRANGEMENT_NAME: u8 = TEXT + 49;
pub const PLAYLIST: u8 = DATA + 25;
pub const TRACK_DATA: u8 = DATA + 30;
pub const TRACK_NAME: u8 = TEXT + 47;
pub const MARKER_NUMERATOR: u8 = 33;
pub const MARKER_DENOMINATOR: u8 = 34;
pub const MARKER_POSITION: u8 = DWORD + 20;
pub const MARKER_NAME: u8 = TEXT + 13;

// Mixer.
pub const INSERT_ICON: u8 = WORD + 31;
pub const SLOT_INDEX: u8 = WORD + 34;
/// Closes the insert being read; the value is where its output goes.
pub const INSERT_OUTPUT: u8 = DWORD + 19;
pub const INSERT_COLOR: u8 = DWORD + 21;
pub const INSERT_INPUT: u8 = DWORD + 26;
pub const INSERT_NAME: u8 = TEXT + 12;
pub const INSERT_ROUTING: u8 = DATA + 27;
pub const INSERT_FLAGS: u8 = DATA + 28;
/// Every insert's and slot's knobs, in one list at the end of the file.
pub const MIXER_PARAMS: u8 = DATA + 17;

// Controllers.
/// What an automation clip is linked to.
pub const REMOTE_CONTROLLER: u8 = DATA + 19;

/// Mixer parameter ids, in [`MIXER_PARAMS`] records and automation links.
///
/// Both say where a parameter is as one word: a base, plus the insert's
/// number times 64, plus the slot. The base was `0x2000` until FL 25 and is
/// `0x7000` from FL 26; the master's own volume record, the first of its
/// kind, gives it away.
pub mod mixer {
    /// The base until FL 25, when a file doesn't show its own.
    pub const DEFAULT_BASE: u16 = 0x2000;
    /// The base from FL 26.
    pub const BASE_26: u16 = 0x7000;
    /// The high byte of an insert's or slot's own parameter (not a plugin's).
    pub const OWN: u8 = 0x1F;
    /// From FL 26, the high byte of a route's level; the id is then the
    /// insert the route goes to.
    pub const ROUTE: u8 = 0x20;
    pub const SLOT_ENABLED: u8 = 0;
    pub const SLOT_MIX: u8 = 1;
    /// Send levels: this plus the target insert's number.
    pub const ROUTE_VOLUME: u8 = 64;
    pub const VOLUME: u8 = 192;
    pub const PAN: u8 = 193;
}
