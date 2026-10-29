//! The plain data that goes into and out of processing: the audio setup a
//! plugin is activated for, one block's events and transport, and what the
//! plugin tells the host back on the main thread.

use crate::ParamId;

/// What a plugin is activated for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioConfig {
    pub sample_rate: f64,
    /// The shortest block the host will process (1 if it may split blocks
    /// anywhere, as for sample-accurate automation).
    pub min_block: u32,
    /// The longest block. [`crate::Processor`] buffers are this long, and
    /// longer blocks are split.
    pub max_block: u32,
    /// Rendering faster or slower than real time (an export), so the plugin
    /// may use its slower, better modes. To switch, activate again.
    pub offline: bool,
}

impl AudioConfig {
    /// Real-time processing in blocks of 1 to `max_block` frames.
    pub fn new(sample_rate: f64, max_block: u32) -> AudioConfig {
        AudioConfig {
            sample_rate,
            min_block: 1,
            max_block: max_block.max(1),
            offline: false,
        }
    }
}

/// Something that happens at a sample within a block: a note, MIDI, or a
/// parameter change. A block's events are sorted by frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// `channel` is 0 to 15, `key` 0 to 127, `velocity` 0 to 1.
    NoteOn {
        frame: u32,
        channel: u8,
        key: u8,
        velocity: f32,
    },
    /// Ends the note started on the same channel and key; `velocity` is the
    /// release velocity.
    NoteOff {
        frame: u32,
        channel: u8,
        key: u8,
        velocity: f32,
    },
    /// Ends every note the host started and hasn't ended (on seek, stop).
    AllNotesOff { frame: u32 },
    /// Retunes the sounding note on `channel` and `key` to `semitones` from
    /// its own pitch: a bend of that note alone. CLAP plugins get it as a
    /// tuning note expression; VST3 ones as a tuning note expression value
    /// if they take that expression; VST2 has no way to say it, and MIDI
    /// note input (without MPE) none either: they don't hear it.
    NoteTuning {
        frame: u32,
        channel: u8,
        key: u8,
        semitones: f64,
    },
    /// A MIDI 1.0 channel message: a controller, pitch bend, pressure, or
    /// a program change. CLAP and VST2 plugins get it as MIDI; VST3 ones as
    /// the parameter their `IMidiMapping` assigns to it, if any. Note ons
    /// and offs given this way are sent as notes.
    Midi { frame: u32, data: [u8; 3] },
    /// Sets a parameter, in its own units ([`crate::ParamInfo`]).
    Param { frame: u32, id: ParamId, value: f64 },
    /// Modulates a parameter without moving it: what the plugin hears is
    /// its value plus `amount` (CLAP's `PARAM_MOD`). For a parameter the
    /// plugin can't modulate (any VST3 or VST2 one, a CLAP one without the
    /// modulatable flag) the processor sends the sum as a value change
    /// instead, so the plugin's own knob moves. An amount of 0 ends it.
    ParamMod {
        frame: u32,
        id: ParamId,
        amount: f64,
    },
}

impl Event {
    pub fn frame(&self) -> u32 {
        match *self {
            Event::NoteOn { frame, .. }
            | Event::NoteOff { frame, .. }
            | Event::AllNotesOff { frame }
            | Event::NoteTuning { frame, .. }
            | Event::Midi { frame, .. }
            | Event::Param { frame, .. }
            | Event::ParamMod { frame, .. } => frame,
        }
    }

    /// The same event at another frame.
    pub fn at(self, frame: u32) -> Event {
        let mut event = self;
        match &mut event {
            Event::NoteOn { frame: f, .. }
            | Event::NoteOff { frame: f, .. }
            | Event::AllNotesOff { frame: f }
            | Event::NoteTuning { frame: f, .. }
            | Event::Midi { frame: f, .. }
            | Event::Param { frame: f, .. }
            | Event::ParamMod { frame: f, .. } => *f = frame,
        }
        event
    }
}

/// Where the song is, for plugins that follow the tempo or the beat.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transport {
    pub playing: bool,
    pub recording: bool,
    /// Beats (quarter notes) per minute.
    pub tempo: f64,
    /// Numerator and denominator, e.g. `(6, 8)`.
    pub time_signature: (u16, u16),
    /// The block's first sample, counted from the song's start.
    pub sample_position: i64,
    /// The same moment in quarter notes.
    pub beat: f64,
    /// Where the bar the block starts in began, in quarter notes, and its
    /// number counting from 0.
    pub bar_start: f64,
    pub bar_number: i32,
    /// The loop, in quarter notes, when looping is on.
    pub loop_range: Option<(f64, f64)>,
}

impl Transport {
    /// Stopped at the start of a 4/4 song at 120 BPM.
    pub const STOPPED: Transport = Transport {
        playing: false,
        recording: false,
        tempo: 120.0,
        time_signature: (4, 4),
        sample_position: 0,
        beat: 0.0,
        bar_start: 0.0,
        bar_number: 0,
        loop_range: None,
    };
}

impl Default for Transport {
    fn default() -> Self {
        Transport::STOPPED
    }
}

/// What a plugin said about the block it just processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// It failed: its output isn't usable (see [`crate::Processor::process`]
    /// for what the outputs hold then).
    Error,
    /// Keep calling it.
    Continue,
    /// Keep calling it while its output isn't silent.
    ContinueIfNotQuiet,
    /// It's ringing out ([`crate::PluginInstance::tail`]); then it may sleep.
    Tail,
    /// Its output stays silent until new input or events arrive, so the
    /// host may stop calling it until then.
    Sleep,
}

/// How long a plugin keeps sounding after its input goes silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    Samples(u32),
    /// Forever (an oscillator, a frozen reverb).
    Infinite,
}

/// Something a plugin did or asked for, reported on the main thread by
/// [`crate::PluginInstance::take_events`] after [`crate::Host::idle`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PluginEvent {
    /// The user started (`begin`) or finished moving a control in the
    /// plugin's own editor. The values in between are one gesture: one
    /// undo step, one stretch of recorded automation.
    ParamGesture { id: ParamId, begin: bool },
    /// The plugin changed a parameter itself: from its editor (CLAP output
    /// events, VST3 `performEdit`, VST2 `audioMasterAutomate`) or as its
    /// own automation. The value is in the parameter's units and already on
    /// its way to the audio thread (the host forwards VST3 edits to the
    /// processor; a VST2 plugin's own value is the one it processes with).
    ParamValue { id: ParamId, value: f64 },
    /// Parameter values changed wholesale (a preset loaded in the editor):
    /// read them again. With `infos`, names, ranges or the list itself
    /// changed too: read [`crate::PluginInstance::params`] again.
    ParamsRescanned { infos: bool },
    /// The plugin needs deactivating and activating again: its latency or
    /// its ports changed, or it wants to reload. Bring its processor back
    /// from the audio thread and call [`crate::PluginInstance::restart`].
    RestartRequested,
    /// Its latency (in samples) is now this, as of its last activation.
    LatencyChanged(u32),
    /// Its tail length changed: ask [`crate::PluginInstance::tail`].
    TailChanged,
    /// Its state changed in a way worth saving (the project is now dirty).
    StateDirty,
    /// The user closed its editor window.
    EditorClosed,
}
