//! What a scan learns about a plugin without running it: who made it, what
//! kind of device it is, and its audio and note ports.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A plugin format we can host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PluginFormat {
    Clap,
    Vst3,
    Vst2,
}

impl PluginFormat {
    pub const ALL: [PluginFormat; 3] = [PluginFormat::Clap, PluginFormat::Vst3, PluginFormat::Vst2];

    /// The name users know it by: "CLAP", "VST3", "VST2".
    pub fn name(self) -> &'static str {
        match self {
            PluginFormat::Clap => "CLAP",
            PluginFormat::Vst3 => "VST3",
            PluginFormat::Vst2 => "VST2",
        }
    }

    /// Lowercase, for command lines and file names: "clap", "vst3",
    /// "vst2".
    pub fn key(self) -> &'static str {
        match self {
            PluginFormat::Clap => "clap",
            PluginFormat::Vst3 => "vst3",
            PluginFormat::Vst2 => "vst2",
        }
    }

    /// The format [`Self::key`] names.
    pub fn from_key(key: &str) -> Option<PluginFormat> {
        PluginFormat::ALL.into_iter().find(|f| f.key() == key)
    }

    /// The extension of its files and bundles, without the dot. VST2
    /// plugins are plain libraries: "so" on Linux, "dll" on Windows.
    pub fn extension(self) -> &'static str {
        match self {
            PluginFormat::Vst2 if cfg!(windows) => "dll",
            PluginFormat::Vst2 => "so",
            _ => self.key(),
        }
    }
}

impl fmt::Display for PluginFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a plugin does, for the device pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginKind {
    /// Turns notes into sound (a synth, a sampler, a drum machine).
    Instrument,
    /// Changes sound (it may also listen to notes, like a vocoder).
    Effect,
    /// Changes notes (an arpeggiator, a chord generator); its output notes
    /// are in [`crate::Processor::output_events`].
    NoteEffect,
}

impl PluginKind {
    pub fn name(self) -> &'static str {
        match self {
            PluginKind::Instrument => "instrument",
            PluginKind::Effect => "effect",
            PluginKind::NoteEffect => "note effect",
        }
    }
}

/// One audio input or output of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioPortInfo {
    pub name: String,
    pub channels: u32,
    /// The main input or output, as opposed to a sidechain or an extra
    /// output. [`crate::Processor::process_stereo`] uses the first main
    /// port in each direction.
    pub main: bool,
}

/// One note (MIDI) input or output of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotePortInfo {
    pub name: String,
}

/// A plugin a scan found, and everything needed to load it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub format: PluginFormat,
    /// Identifies the plugin within its format, stably across versions and
    /// machines: a CLAP plugin's id ("com.u-he.diva"); a VST3 class id as
    /// 32 hexadecimal digits in the byte order of Steinberg's FUID strings,
    /// which is the same for a plugin on Windows and bridged to Linux; or a
    /// VST2 plugin's unique id (an `i32`, often four characters like
    /// `'Vtal'`) as 8 uppercase hexadecimal digits of its bits, most
    /// significant first ("5674616C"). VST2 ids are only meant to be unique
    /// among a vendor's plugins, so [`Self::path`] matters too.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub kind: PluginKind,
    /// CLAP features ("synthesizer", "stereo"), VST3 subcategories ("Fx",
    /// "Dynamics"), as the plugin lists them, or the one VST2 category
    /// ("Effect", "Synth", "Analysis"...).
    pub categories: Vec<String>,
    /// The file (CLAP, VST2, or a single-file Windows VST3) or bundle
    /// folder (VST3) to load.
    pub path: PathBuf,
    /// Audio ports. VST2 plugins only number their channels: the first two
    /// (or one) make the main port, the next two the sidechain input (or a
    /// second output), the rest more ports of two.
    pub audio_inputs: Vec<AudioPortInfo>,
    pub audio_outputs: Vec<AudioPortInfo>,
    pub note_inputs: Vec<NotePortInfo>,
    pub note_outputs: Vec<NotePortInfo>,
    /// A Windows plugin made loadable on Linux by yabridge.
    pub bridged: bool,
}

impl PluginInfo {
    /// The first main audio input, if it has one.
    pub fn main_input(&self) -> Option<&AudioPortInfo> {
        main_port(&self.audio_inputs)
    }

    /// The first main audio output, if it has one.
    pub fn main_output(&self) -> Option<&AudioPortInfo> {
        main_port(&self.audio_outputs)
    }

    /// The audio input after the main one (a sidechain), if it has one:
    /// what [`crate::Processor::process`] fills from its `aux` channels.
    pub fn aux_input(&self) -> Option<&AudioPortInfo> {
        aux_port_index(&self.audio_inputs).map(|i| &self.audio_inputs[i])
    }
}

/// The first input that isn't the main one.
pub(crate) fn aux_port_index(ports: &[AudioPortInfo]) -> Option<usize> {
    let main = main_port_index(ports);
    (0..ports.len()).find(|&i| Some(i) != main)
}

/// The first port marked main, or else the first port.
pub(crate) fn main_port(ports: &[AudioPortInfo]) -> Option<&AudioPortInfo> {
    main_port_index(ports).map(|i| &ports[i])
}

pub(crate) fn main_port_index(ports: &[AudioPortInfo]) -> Option<usize> {
    ports
        .iter()
        .position(|p| p.main)
        .or((!ports.is_empty()).then_some(0))
}

/// The kind a plugin's categories and ports suggest. Categories win; a
/// plugin that names none is an instrument if it takes notes and makes
/// sound from nothing, a note effect if it only deals in notes, and an
/// effect otherwise.
pub(crate) fn guess_kind(
    instrument: bool,
    effect: bool,
    note_effect: bool,
    info: &PluginInfo,
) -> PluginKind {
    if instrument {
        PluginKind::Instrument
    } else if note_effect && !effect {
        PluginKind::NoteEffect
    } else if effect {
        PluginKind::Effect
    } else if info.audio_inputs.is_empty() && !info.audio_outputs.is_empty() {
        PluginKind::Instrument
    } else if info.audio_outputs.is_empty() && !info.note_outputs.is_empty() {
        PluginKind::NoteEffect
    } else {
        PluginKind::Effect
    }
}
